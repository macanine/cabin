//! Resumable, verified HTTP downloads (ureq; no external `curl`).
//!
//! A download streams into `<destination>.part`, resumes with a `Range`
//! request after a dropped connection, is checked against its SHA-256 or
//! SHA-512 (`Checksum`), and
//! only then is renamed into place. While it runs, `progress(destination)`
//! reports bytes, total and speed from an in-memory counter, which the
//! installer's progress poller reads. `cancel_all` stops running downloads
//! (used when the user cancels an install).

use crate::util::LockExt;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::util::{format_bytes, log_line, message, sha256_file, sha512_file};
use crate::{Error, Result};

const ATTEMPTS: u32 = 5;

/// The expected digest of a download, as its publisher states it (Ubuntu
/// lists SHA-256 sums; QEMU release assets and others may state SHA-512).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checksum {
    Sha256(String),
    Sha512(String),
}

impl Checksum {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Sha256(_) => "SHA-256",
            Self::Sha512(_) => "SHA-512",
        }
    }

    pub fn hex(&self) -> &str {
        match self {
            Self::Sha256(hex) | Self::Sha512(hex) => hex,
        }
    }

    /// A lowercase hex digest of the right length for its algorithm.
    pub fn is_well_formed(&self) -> bool {
        let len = match self {
            Self::Sha256(_) => 64,
            Self::Sha512(_) => 128,
        };
        let hex = self.hex();
        hex.len() == len
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    }

    /// The file's digest with this checksum's algorithm.
    pub fn of_file(&self, path: &Path) -> Result<String> {
        match self {
            Self::Sha256(_) => sha256_file(path),
            Self::Sha512(_) => sha512_file(path),
        }
    }

    pub fn matches(&self, path: &Path) -> Result<bool> {
        Ok(self.of_file(path)? == self.hex())
    }
}
const CHUNK: usize = 256 * 1024;

/// Live state of a running download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub done_bytes: u64,
    pub total_bytes: Option<u64>,
    /// Average speed of the current attempt.
    pub bytes_per_sec: Option<u64>,
    /// All bytes are here and the checksum is being verified.
    pub verifying: bool,
}

struct Active {
    done: AtomicU64,
    /// 0 = unknown.
    total: AtomicU64,
    verifying: AtomicBool,
    attempt: Mutex<(Instant, u64)>,
    cancel_epoch: u64,
}

fn active() -> &'static Mutex<HashMap<PathBuf, Arc<Active>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<PathBuf, Arc<Active>>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

static CANCEL: AtomicU64 = AtomicU64::new(0);

/// Stop every running download; each fails with "cancelled".
pub fn cancel_all() {
    CANCEL.fetch_add(1, Ordering::SeqCst);
}

/// Progress of the download into `destination`, if one is running.
pub fn progress(destination: &Path) -> Option<DownloadProgress> {
    let entry = active().lock().ok()?.get(destination).cloned()?;
    let done = entry.done.load(Ordering::Relaxed);
    let total = Some(entry.total.load(Ordering::Relaxed)).filter(|t| *t > 0);
    let (since, start_bytes) = *entry.attempt.lock().ok()?;
    let elapsed = since.elapsed().as_secs_f64();
    let bytes_per_sec = (elapsed >= 1.0 && done > start_bytes)
        .then(|| ((done - start_bytes) as f64 / elapsed) as u64);
    Some(DownloadProgress {
        done_bytes: done,
        total_bytes: total,
        bytes_per_sec,
        verifying: entry.verifying.load(Ordering::Relaxed),
    })
}

/// Removes the progress entry when the download ends, however it ends.
struct Registered(PathBuf);

impl Drop for Registered {
    fn drop(&mut self) {
        if let Ok(mut map) = active().lock() {
            map.remove(&self.0);
        }
    }
}

const READ_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

fn bypasses_proxy(url: &url::Url, rules: &str) -> bool {
    let Some(host) = url.host_str().map(|host| host.trim_end_matches('.')) else {
        return false;
    };
    rules.split([',', ';']).map(str::trim).any(|rule| {
        if rule == "*" || (rule.eq_ignore_ascii_case("<local>") && !host.contains(['.', ':'])) {
            return true;
        }
        let (rule, port) = rule
            .rsplit_once(':')
            .filter(|(name, _)| !name.contains(':') || name.ends_with(']'))
            .and_then(|(name, port)| Some((name, Some(port.parse::<u16>().ok()?))))
            .unwrap_or((rule, None));
        if port.is_some() && port != url.port_or_known_default() {
            return false;
        }
        let rule = rule.trim_start_matches('.').trim_end_matches('.');
        if rule.is_empty() {
            return false;
        }
        if rule.contains('*') {
            let pattern = regex::escape(rule).replace(r"\*", ".*");
            return regex::Regex::new(&format!("(?i)^{pattern}$"))
                .is_ok_and(|re| re.is_match(host));
        }
        let host = host.to_ascii_lowercase();
        let rule = rule.to_ascii_lowercase();
        host == rule
            || host
                .strip_suffix(&rule)
                .is_some_and(|prefix| prefix.ends_with('.'))
    })
}

fn env_proxy(url: &url::Url, get: impl Fn(&str) -> Option<String>) -> Option<String> {
    let env = |lower: &str, upper: &str| {
        get(lower)
            .filter(|value| !value.trim().is_empty())
            .or_else(|| get(upper).filter(|value| !value.trim().is_empty()))
    };
    let proxy = match url.scheme() {
        "https" => env("https_proxy", "HTTPS_PROXY"),
        "http" if get("REQUEST_METHOD").is_none() => env("http_proxy", "HTTP_PROXY"),
        "http" => get("http_proxy").filter(|value| !value.trim().is_empty()),
        _ => None,
    };
    proxy.or_else(|| env("all_proxy", "ALL_PROXY"))
}

#[cfg(any(windows, test))]
fn manual_proxy(url: &url::Url, servers: &str, bypass: &str) -> Option<String> {
    if bypasses_proxy(url, bypass) {
        return None;
    }
    servers
        .split(';')
        .map(str::trim)
        .find_map(|entry| match entry.split_once('=') {
            Some((scheme, address)) if scheme.eq_ignore_ascii_case(url.scheme()) => Some(address),
            None if !entry.is_empty() => Some(entry),
            _ => None,
        })
        .map(str::to_owned)
}

#[cfg(not(windows))]
fn system_proxy(_: &url::Url) -> Option<String> {
    None
}

#[cfg(windows)]
fn system_proxy(url: &url::Url) -> Option<String> {
    // WinHTTP exposes the signed-in user's manual Windows proxy and bypass list.
    #[repr(C)]
    #[derive(Default)]
    struct ProxyConfig {
        auto_detect: i32,
        auto_config: *mut u16,
        proxy: *mut u16,
        bypass: *mut u16,
    }
    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpGetIEProxyConfigForCurrentUser(config: *mut ProxyConfig) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
    }

    let take_string = |ptr: *mut u16| {
        if ptr.is_null() {
            return String::new();
        }
        // WinHTTP returns NUL-terminated UTF-16 strings owned by the caller.
        unsafe {
            let len = (0..).find(|&index| *ptr.add(index) == 0).unwrap();
            let value = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            GlobalFree(ptr.cast());
            value
        }
    };
    let mut config = ProxyConfig::default();
    let success = unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut config) } != 0;
    let proxy = take_string(config.proxy);
    let bypass = take_string(config.bypass);
    let _ = take_string(config.auto_config);
    success
        .then(|| manual_proxy(url, &proxy, &bypass))
        .flatten()
}

pub(crate) struct HttpOptions {
    pub read_timeout: Duration,
    /// A deadline for small metadata requests, including body reads.
    pub timeout: Option<Duration>,
}

/// ureq's error type is large, and every caller matches on it directly;
/// boxing it would only move the copy into each call site.
#[allow(clippy::result_large_err)]
pub(crate) fn request(
    method: &str,
    target: &str,
    range: Option<u64>,
    options: &HttpOptions,
) -> std::result::Result<ureq::Response, ureq::Error> {
    let mut url = url::Url::parse(target)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let started = Instant::now();
    for hop in 0..=5 {
        let var = |name: &str| std::env::var(name).ok();
        let bypass = var("no_proxy")
            .or_else(|| var("NO_PROXY"))
            .unwrap_or_default();
        let proxy = if bypasses_proxy(&url, &bypass) {
            None
        } else {
            env_proxy(&url, var).or_else(|| system_proxy(&url))
        };
        let remaining = options
            .timeout
            .map(|timeout| timeout.saturating_sub(started.elapsed()));
        let mut builder = ureq::AgentBuilder::new()
            .resolver(crate::ipv4::resolve)
            .redirects(0)
            .timeout_connect(remaining.map_or(CONNECT_TIMEOUT, |time| time.min(CONNECT_TIMEOUT)))
            .timeout_read(options.read_timeout)
            .try_proxy_from_env(false)
            .user_agent(concat!("cabin/", env!("CARGO_PKG_VERSION")));
        if let Some(timeout) = remaining {
            if timeout.is_zero() {
                return Err(std::io::Error::from(std::io::ErrorKind::TimedOut).into());
            }
            builder = builder.timeout(timeout);
        }
        if let Some(proxy) = proxy {
            builder = builder.proxy(ureq::Proxy::new(proxy.trim())?);
        }
        let mut request = builder.build().request_url(method, &url);
        if let Some(offset) = range {
            request = request.set("Range", &format!("bytes={offset}-"));
        }
        let response = request.call()?;
        if !matches!(response.status(), 301 | 302 | 303 | 307 | 308) {
            return Ok(response);
        }
        let location = response.header("Location").ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "redirect is missing its Location header",
            )
        })?;
        let next = url
            .join(location)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if url.scheme() == "https" && next.scheme() != "https" {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "HTTPS download redirected to an insecure URL",
            )
            .into());
        }
        if hop == 5 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "too many download redirects",
            )
            .into());
        }
        url = next;
    }
    unreachable!("redirect limit enforced above");
}

pub(crate) fn http_error(error: &ureq::Error) -> String {
    let detail = error.to_string();
    if detail.contains("tls connection") {
        format!(
            "TLS verification or handshake failed. Check this computer's date and time and whether its proxy certificate is trusted: {detail}"
        )
    } else if error.kind() == ureq::ErrorKind::Dns {
        format!("download DNS lookup failed: {detail}")
    } else {
        format!("could not connect: {detail}")
    }
}

#[derive(Debug)]
enum TransferError {
    Retry(String),
    Source(String),
    ResetPartial(bool),
    Local(Error),
}

impl From<std::io::Error> for TransferError {
    fn from(error: std::io::Error) -> Self {
        Self::Local(error.into())
    }
}

fn range_bounds(value: &str) -> Option<(u64, u64, u64)> {
    let (bounds, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = bounds.split_once('-')?;
    let (start, end, total) = (start.parse().ok()?, end.parse().ok()?, total.parse().ok()?);
    (start <= end && end < total).then_some((start, end, total))
}

pub(crate) fn is_html(response: &ureq::Response) -> bool {
    response.header("Content-Type").is_some_and(|value| {
        let media_type = value.split(';').next().unwrap_or("").trim();
        media_type.eq_ignore_ascii_case("text/html")
            || media_type.eq_ignore_ascii_case("application/xhtml+xml")
    })
}

/// None means the prefix could still be HTML and needs another read.
fn html_prefix(bytes: &[u8]) -> Option<bool> {
    const BOM: &[u8] = b"\xef\xbb\xbf";
    if bytes.len() < BOM.len() && BOM.starts_with(bytes) {
        return None;
    }
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes).trim_ascii_start();
    let tags = [
        b"<!doctype html".as_slice(),
        b"<html",
        b"<head",
        b"<body",
        b"<title",
        b"<meta",
        b"<!--",
    ];
    if tags.iter().any(|tag| {
        bytes
            .get(..tag.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(tag))
    }) {
        return Some(true);
    }
    if tags.iter().any(|tag| {
        tag.get(..bytes.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(bytes))
    }) {
        return None;
    }
    Some(false)
}

fn check_cancelled(state: &Active) -> Result<()> {
    if CANCEL.load(Ordering::SeqCst) != state.cancel_epoch {
        return message("download cancelled");
    }
    Ok(())
}

fn read_body(
    reader: &mut impl Read,
    buffer: &mut [u8],
    timeout: Duration,
    state: &Active,
) -> std::result::Result<usize, TransferError> {
    check_cancelled(state).map_err(TransferError::Local)?;
    reader.read(buffer).map_err(|error| {
        use std::io::ErrorKind::{TimedOut, WouldBlock};
        if matches!(error.kind(), TimedOut | WouldBlock) {
            TransferError::Retry(format!(
                "download stalled (no bytes received for {} seconds)",
                timeout.as_secs_f64()
            ))
        } else {
            TransferError::Retry(format!("connection dropped while downloading: {error}"))
        }
    })
}

/// One attempt: append only after validating the server's byte range.
fn attempt(
    url: &str,
    part: &Path,
    state: &Active,
    log: &Arc<Mutex<File>>,
    timeout: Duration,
) -> std::result::Result<bool, TransferError> {
    check_cancelled(state).map_err(TransferError::Local)?;
    let offset = match fs::metadata(part) {
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error.into()),
    };
    let response = match request(
        "GET",
        url,
        (offset > 0).then_some(offset),
        &HttpOptions {
            read_timeout: timeout,
            timeout: None,
        },
    ) {
        Err(ureq::Error::Status(416, response)) if offset > 0 => {
            let total = response
                .header("Content-Range")
                .and_then(|value| value.strip_prefix("bytes */"))
                .and_then(|value| value.parse::<u64>().ok());
            return Err(TransferError::ResetPartial(
                total.is_none_or(|total| total == offset),
            ));
        }
        Err(ureq::Error::Status(code, response)) => {
            let detail = format!("HTTP {code} {} for {url}", response.status_text());
            return Err(if code == 408 || code == 429 || code >= 500 {
                TransferError::Retry(detail)
            } else {
                TransferError::Source(detail)
            });
        }
        Err(error) => return Err(TransferError::Retry(http_error(&error))),
        Ok(response) => response,
    };
    let portal = || {
        TransferError::Source(format!(
            "download received an HTML page instead of the file from {url}; sign in to the network if it requires a captive portal"
        ))
    };
    if is_html(&response) {
        return Err(portal());
    }
    let length = response
        .header("Content-Length")
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| {
            TransferError::Source(format!("invalid download Content-Length from {url}"))
        })?;
    let (start, total, end) = match response.status() {
        200 => {
            if offset > 0 {
                log_line(
                    log,
                    "the server does not support resuming; starting from zero",
                );
            }
            (0, length, length)
        }
        206 if offset > 0 => {
            let (start, end, total) = response
                .header("Content-Range")
                .and_then(range_bounds)
                .filter(|(start, _, _)| *start == offset)
                .ok_or(TransferError::ResetPartial(false))?;
            if length.is_some_and(|len| len != end - start + 1) {
                return Err(TransferError::Source(format!(
                    "download Content-Length does not match Content-Range from {url}"
                )));
            }
            (start, Some(total), Some(end + 1))
        }
        code => {
            return Err(TransferError::Source(format!(
                "unexpected HTTP {code} for {url}"
            )));
        }
    };
    state.total.store(total.unwrap_or(0), Ordering::Relaxed);
    state.done.store(start, Ordering::Relaxed);
    *state.attempt.locked() = (Instant::now(), start);

    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; CHUNK];
    // Sniff the first bytes even if a portal omits its HTML content type or
    // sends the markup in several packets. Inspect only the start of the file.
    let mut read = 0;
    if start == 0 {
        loop {
            let count = read_body(&mut reader, &mut buffer[read..], timeout, state)?;
            read += count;
            match html_prefix(&buffer[..read]) {
                Some(true) => return Err(portal()),
                Some(false) => break,
                None if count == 0 || read >= 512 => break,
                None => {}
            }
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(start > 0)
        .truncate(start == 0)
        .open(part)?;
    let mut done = start;
    let mut next_report = 10u64;
    loop {
        if read > 0 {
            done += read as u64;
            if end.is_some_and(|end| done > end) {
                return Err(TransferError::Source(format!(
                    "download exceeded its declared length from {url}"
                )));
            }
            write_chunk(&mut file, &buffer[..read], part)?;
            state.done.store(done, Ordering::Relaxed);
            if let Some(total) = total {
                let percent = done.saturating_mul(100) / total.max(1);
                if percent >= next_report {
                    let speed = progress_speed(state);
                    log_line(
                        log,
                        &format!(
                            "{percent}% · {} of {}{}",
                            format_bytes(Some(done)),
                            format_bytes(Some(total)),
                            speed
                                .map(|s| format!(" · {}/s", format_bytes(Some(s))))
                                .unwrap_or_default()
                        ),
                    );
                    next_report = (percent / 10 + 1) * 10;
                }
            }
        }
        read = read_body(&mut reader, &mut buffer, timeout, state)?;
        if read == 0 {
            break;
        }
    }
    if total.is_some_and(|total| done != total) {
        return Err(TransferError::Retry(format!(
            "download Content-Length mismatch or incomplete range at {}",
            format_bytes(Some(done))
        )));
    }
    // Catch deferred write errors (including a full volume) before publishing.
    file.sync_all()?;
    Ok(start > 0)
}

fn write_chunk(
    file: &mut impl Write,
    data: &[u8],
    path: &Path,
) -> std::result::Result<(), TransferError> {
    file.write_all(data)
        .map_err(|error| TransferError::Local(storage_error(error.into(), path)))
}

fn progress_speed(state: &Active) -> Option<u64> {
    let (since, start) = *state.attempt.lock().ok()?;
    let elapsed = since.elapsed().as_secs_f64();
    let done = state.done.load(Ordering::Relaxed);
    (elapsed >= 1.0 && done > start).then(|| ((done - start) as f64 / elapsed) as u64)
}

pub(crate) fn partial_path(destination: &Path) -> PathBuf {
    let mut path = destination.as_os_str().to_owned();
    path.push(".part");
    path.into()
}

fn storage_error(error: Error, destination: &Path) -> Error {
    if matches!(&error, Error::Io(error) if error.kind() == std::io::ErrorKind::StorageFull) {
        return Error::Message(format!(
            "not enough free space to download to {}",
            destination.display()
        ));
    }
    error
}

/// Download `url` to `destination` and verify its checksum. An existing,
/// verified file is reused; a partial `.part` file is resumed.
pub fn download_verified(
    url: &str,
    destination: &Path,
    expected: &Checksum,
    log: &Arc<Mutex<File>>,
) -> Result<()> {
    download_first(&[url.to_owned()], destination, expected, log).map(|_| ())
}

/// Try each candidate URL in order until one downloads and verifies. Returns
/// the URL that worked.
pub fn download_first(
    urls: &[String],
    destination: &Path,
    expected: &Checksum,
    log: &Arc<Mutex<File>>,
) -> Result<String> {
    download_sources(
        urls,
        destination,
        expected,
        log,
        READ_TIMEOUT,
        Duration::from_secs(2),
    )
    .map_err(|error| storage_error(error, destination))
}

fn download_sources(
    urls: &[String],
    destination: &Path,
    expected: &Checksum,
    log: &Arc<Mutex<File>>,
    read_timeout: Duration,
    backoff: Duration,
) -> Result<String> {
    let Some(first) = urls.first() else {
        return message("no download source");
    };
    let cancel_epoch = CANCEL.load(Ordering::SeqCst);
    if let Some(parent) = destination.parent() {
        crate::util::ensure_dir(parent)?;
    }
    let part = partial_path(destination);
    if destination.exists() {
        if expected.matches(destination)? {
            log_line(
                log,
                &format!("already present and verified: {}", destination.display()),
            );
            return Ok(first.clone());
        }
        log_line(
            log,
            "existing file has the wrong checksum; downloading again",
        );
        fs::remove_file(destination)?;
    }

    let state = Arc::new(Active {
        done: AtomicU64::new(fs::metadata(&part).map(|m| m.len()).unwrap_or(0)),
        total: AtomicU64::new(0),
        verifying: AtomicBool::new(false),
        attempt: Mutex::new((Instant::now(), 0)),
        cancel_epoch,
    });
    active()
        .locked()
        .insert(destination.to_path_buf(), state.clone());
    let _registered = Registered(destination.to_path_buf());
    let mut failures = Vec::new();
    for url in urls {
        log_line(
            log,
            &format!("download {url}\n  -> {}", destination.display()),
        );
        let mut last_error = String::new();
        let mut reset = false;
        let mut number = 1;
        while number <= ATTEMPTS {
            state.verifying.store(false, Ordering::Relaxed);
            let may_be_complete = match attempt(url, &part, &state, log, read_timeout) {
                Ok(resumed) => Some((resumed, true)),
                Err(TransferError::ResetPartial(check_size)) => Some((true, check_size)),
                Err(TransferError::Local(error)) => return Err(error),
                Err(TransferError::Source(detail)) => {
                    last_error = detail;
                    break;
                }
                Err(TransferError::Retry(detail)) => {
                    log_line(
                        log,
                        &format!("attempt {number}/{ATTEMPTS} failed: {detail}"),
                    );
                    last_error = format!("download failed after {number} attempts: {detail}");
                    if number < ATTEMPTS {
                        std::thread::sleep(backoff * number);
                    }
                    number += 1;
                    None
                }
            };
            let Some((resumed, check_size)) = may_be_complete else {
                continue;
            };
            if check_size {
                state.verifying.store(true, Ordering::Relaxed);
                log_line(log, &format!("verifying {} …", expected.name()));
                let actual = expected.of_file(&part)?;
                if actual == expected.hex() {
                    check_cancelled(&state)?;
                    fs::rename(&part, destination)?;
                    log_line(
                        log,
                        &format!("download complete, checksum ok: {}", destination.display()),
                    );
                    return Ok(url.clone());
                }
                last_error = format!(
                    "checksum mismatch for {url}: expected {}, got {actual}",
                    expected.hex()
                );
            } else {
                last_error = format!("download server rejected the saved partial range from {url}");
            }
            fs::remove_file(&part)?;
            if resumed && !reset {
                log_line(
                    log,
                    &format!(
                        "{last_error}. Discarding the saved partial and restarting from zero."
                    ),
                );
                reset = true;
                continue;
            }
            break;
        }
        log_line(log, &format!("{url} failed: {last_error}"));
        failures.push(last_error);
    }
    message(failures.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::net::TcpListener;

    /// A tiny HTTP server that honours `Range`, optionally dropping the
    /// connection after `cut` bytes of the first response.
    fn serve(body: Vec<u8>, cut: Option<usize>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let mut first = true;
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut range = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("range: bytes=") {
                        range = value.trim().trim_end_matches('-').parse().unwrap();
                    }
                }
                let slice = &body[range..];
                let status = if range > 0 {
                    "206 Partial Content"
                } else {
                    "200 OK"
                };
                let content_range = if range > 0 {
                    format!(
                        "Content-Range: bytes {range}-{}/{}\r\n",
                        body.len() - 1,
                        body.len()
                    )
                } else {
                    String::new()
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{content_range}Connection: close\r\n\r\n",
                    slice.len()
                );
                stream.write_all(head.as_bytes()).unwrap();
                let send = match (first, cut) {
                    (true, Some(cut)) => &slice[..cut],
                    _ => slice,
                };
                first = false;
                let _ = stream.write_all(send);
            }
        });
        format!("http://{address}/image.img")
    }

    fn log_file(dir: &Path) -> Arc<Mutex<File>> {
        Arc::new(Mutex::new(File::create(dir.join("log")).unwrap()))
    }

    fn sha(bytes: &[u8]) -> Checksum {
        use sha2::{Digest, Sha256};
        Checksum::Sha256(format!("{:x}", Sha256::digest(bytes)))
    }

    #[test]
    fn verifies_sha512_downloads() {
        use sha2::{Digest, Sha512};
        let dir = std::env::temp_dir().join(format!("dl-512-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let body: Vec<u8> = (0..50_000u32).map(|i| (i % 13) as u8).collect();
        let expected = Checksum::Sha512(format!("{:x}", Sha512::digest(&body)));
        assert!(expected.is_well_formed());
        let url = serve(body.clone(), None);
        let destination = dir.join("image.qcow2");
        download_verified(&url, &destination, &expected, &log_file(&dir)).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), body);
        let log = fs::read_to_string(dir.join("log")).unwrap();
        assert!(log.contains("verifying SHA-512"), "{log}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn downloads_and_verifies() {
        let dir = std::env::temp_dir().join(format!("dl-ok-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let body: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        let url = serve(body.clone(), None);
        let destination = dir.join("image.img");
        download_verified(&url, &destination, &sha(&body), &log_file(&dir)).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), body);
        assert!(
            progress(&destination).is_none(),
            "progress entry is removed"
        );
        let log = fs::read_to_string(dir.join("log")).unwrap();
        assert!(log.contains("100% ·"), "{log}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn resumes_after_a_dropped_connection() {
        let dir = std::env::temp_dir().join(format!("dl-resume-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let body: Vec<u8> = (0..600_000u32).map(|i| (i % 241) as u8).collect();
        let url = serve(body.clone(), Some(200_000));
        let destination = dir.join("image.img");
        download_sources(
            &[url],
            &destination,
            &sha(&body),
            &log_file(&dir),
            READ_TIMEOUT,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(fs::read(&destination).unwrap(), body);
        let log = fs::read_to_string(dir.join("log")).unwrap();
        assert!(log.contains("attempt 1/5 failed"), "{log}");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn wrong_checksum_removes_the_download() {
        let dir = std::env::temp_dir().join(format!("dl-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let url = serve(vec![7u8; 1000], None);
        let destination = dir.join("image.img");
        let error = download_verified(
            &url,
            &destination,
            &Checksum::Sha256("0".repeat(64)),
            &log_file(&dir),
        )
        .unwrap_err();
        assert!(error.to_string().contains("checksum mismatch"));
        assert!(!destination.exists() && !dir.join("image.img.part").exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn falls_back_to_the_next_source() {
        let dir = std::env::temp_dir().join(format!("dl-fallback-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let body: Vec<u8> = (0..20_000u32).map(|i| (i % 97) as u8).collect();
        // A dead port first, then a source with different content, then a good one.
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}/image.img", l.local_addr().unwrap())
        };
        let wrong = serve(vec![1; 100], None);
        let good = serve(body.clone(), None);
        let destination = dir.join("image.img");
        let used = download_sources(
            &[dead, wrong, good.clone()],
            &destination,
            &sha(&body),
            &log_file(&dir),
            READ_TIMEOUT,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(used, good);
        assert_eq!(fs::read(&destination).unwrap(), body);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn http_errors_are_reported() {
        let dir = std::env::temp_dir().join(format!("dl-404-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
        let destination = dir.join("image.img");
        let error = download_verified(
            &format!("http://{address}/x"),
            &destination,
            &Checksum::Sha256("0".repeat(64)),
            &log_file(&dir),
        )
        .unwrap_err();
        assert!(error.to_string().contains("HTTP 404"), "{error}");
        let _ = fs::remove_dir_all(dir);
    }

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cabin-download-{name}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn scripted(
        count: usize,
        handler: impl Fn(usize, String, std::net::TcpStream) + Send + 'static,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (index, stream) in listener.incoming().take(count).enumerate() {
                let stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&stream);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line.to_ascii_lowercase());
                }
                handler(index, headers, stream);
            }
        });
        (format!("http://{address}/image.img"), server)
    }

    #[test]
    fn stall_retries_and_resumes_from_the_bytes_already_received() {
        let dir = scratch("stalled");
        let dest = dir.join("image.img");
        let (url, server) = scripted(2, |i, headers, mut stream| {
            if i == 0 {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabc",
                    )
                    .unwrap();
                // Keep the socket open until the client hits its read timeout.
                assert!(matches!(stream.read(&mut [0; 1]), Ok(0) | Err(_)));
            } else {
                assert!(headers.contains("range: bytes=3-"), "{headers}");
                stream.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-7/8\r\nContent-Length: 5\r\nConnection: close\r\n\r\ndefgh").unwrap();
            }
        });
        let log = log_file(&dir);
        download_sources(
            &[url],
            &dest,
            &sha(b"abcdefgh"),
            &log,
            Duration::from_millis(150),
            Duration::ZERO,
        )
        .unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"abcdefgh");
        assert!(
            fs::read_to_string(dir.join("log"))
                .unwrap()
                .contains("download stalled (no bytes received for")
        );
        drop(log);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn handles_a_part_larger_than_the_target_and_servers_that_ignore_range() {
        for (name, sends_416) in [("oversize", true), ("no-range", false)] {
            let dir = scratch(name);
            let dest = dir.join("image.img");
            fs::write(partial_path(&dest), b"obsolete partial").unwrap();
            let (url, server) = scripted(
                if sends_416 { 2 } else { 1 },
                move |i, headers, mut stream| {
                    assert_eq!(headers.contains("range: bytes=16-"), i == 0, "{headers}");
                    let reply: &[u8] = if i == 0 && sends_416 {
                        b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */3\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    } else {
                        b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nnew"
                    };
                    stream.write_all(reply).unwrap();
                },
            );
            download_verified(&url, &dest, &sha(b"new"), &log_file(&dir)).unwrap();
            server.join().unwrap();
            assert_eq!(fs::read(&dest).unwrap(), b"new");
            assert!(!partial_path(&dest).exists());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn rejects_captive_portal_html_before_writing_the_partial() {
        for (name, content_type) in [
            ("html-type", "text/html; charset=utf-8"),
            ("html-sniff", "application/octet-stream"),
        ] {
            let dir = scratch(name);
            let dest = dir.join("image.img");
            let (url, server) = scripted(1, move |_, _, mut stream| {
                let body = b" \r\n<!DOCTYPE html><title>Sign in</title>";
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                let _ = stream.write_all(body);
            });
            let error =
                download_verified(&url, &dest, &sha(b"image"), &log_file(&dir)).unwrap_err();
            server.join().unwrap();
            assert!(error.to_string().contains("captive portal"), "{error}");
            assert!(!dest.exists() && !partial_path(&dest).exists());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn a_mirror_with_a_different_prefix_restarts_cleanly() {
        let dir = scratch("mirror-switch");
        let dest = dir.join("image.img");
        let (first, first_server) = scripted(ATTEMPTS as usize, |i, headers, mut stream| {
            if i == 0 {
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nxyz",
                    )
                    .unwrap();
            } else {
                assert!(headers.contains("range: bytes=3-"), "{headers}");
                stream.write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            }
        });
        let (second, second_server) = scripted(2, |i, headers, mut stream| {
            if i == 0 {
                assert!(headers.contains("range: bytes=3-"), "{headers}");
                stream.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-7/8\r\nContent-Length: 5\r\nConnection: close\r\n\r\ndefgh").unwrap();
            } else {
                assert!(!headers.contains("range:"), "{headers}");
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nabcdefgh").unwrap();
            }
        });
        let log = log_file(&dir);
        let source = download_sources(
            &[first, second.clone()],
            &dest,
            &sha(b"abcdefgh"),
            &log,
            READ_TIMEOUT,
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(source, second);
        first_server.join().unwrap();
        second_server.join().unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"abcdefgh");
        drop(log);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_a_range_with_a_conflicting_content_length() {
        let dir = scratch("range-length");
        let dest = dir.join("image.img");
        fs::write(partial_path(&dest), b"abc").unwrap();
        let (url, server) = scripted(1, |_, _, mut stream| {
            stream.write_all(b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-7/8\r\nContent-Length: 1\r\nConnection: close\r\n\r\nd").unwrap();
        });
        let error = download_verified(&url, &dest, &sha(b"abcdefgh"), &log_file(&dir)).unwrap_err();
        server.join().unwrap();
        assert!(
            error
                .to_string()
                .contains("Content-Length does not match Content-Range"),
            "{error}"
        );
        assert_eq!(fs::read(partial_path(&dest)).unwrap(), b"abc");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn second_install_cannot_mutate_the_same_home() {
        use crate::machine::{InstallOptions, Machine};
        let dir = scratch("install-lock");
        let lock =
            crate::lock::FileLock::acquire(&crate::paths::StatePaths::new(&dir).operation_lock())
                .unwrap();
        let machine = Machine::new(&dir, crate::config::default_project_root());
        let error = machine.install(&InstallOptions::default()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("another vmctl operation is in progress"),
            "{error}"
        );
        drop(lock);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn scheme_proxy_overrides_all_proxy_and_no_proxy_matches_host_and_port() {
        let get = |key: &str| match key {
            "HTTPS_PROXY" => Some("proxy-for-https:8080".into()),
            "ALL_PROXY" => Some("socks5://proxy-fallback:1080".into()),
            _ => None,
        };
        let https = url::Url::parse("https://cdn.example.test/image.img").unwrap();
        let http = url::Url::parse("http://cdn.example.test/image.img").unwrap();
        assert_eq!(
            env_proxy(&https, get).as_deref(),
            Some("proxy-for-https:8080")
        );
        assert_eq!(
            env_proxy(&http, get).as_deref(),
            Some("socks5://proxy-fallback:1080")
        );
        assert!(bypasses_proxy(&https, "localhost, .example.test:443"));
        assert!(!bypasses_proxy(&http, "localhost, .example.test:443"));
        assert!(!bypasses_proxy(&https, "ample.test"));
        assert!(bypasses_proxy(&https, "*"));
        let servers = "http=proxy:8080;https=secure:8081";
        assert_eq!(
            manual_proxy(&https, servers, "<local>;*.internal").as_deref(),
            Some("secure:8081")
        );
        assert_eq!(
            manual_proxy(&https, servers, "<local>;*.example.test"),
            None
        );
    }

    #[test]
    fn tls_failure_points_to_the_clock_and_proxy_certificates() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("https://{}/image.img", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            // Reply to the TLS ClientHello with invalid TLS data.
            let _ = stream.read(&mut [0; 4096]).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\n\r\n").unwrap();
        });
        let error = request(
            "GET",
            &url,
            None,
            &HttpOptions {
                read_timeout: Duration::from_secs(3),
                timeout: Some(Duration::from_secs(3)),
            },
        )
        .unwrap_err();
        server.join().unwrap();
        let detail = http_error(&error);
        assert!(
            detail.contains("TLS verification or handshake failed"),
            "{detail}"
        );
        assert!(
            detail.contains("date and time") && detail.contains("proxy certificate"),
            "{detail}"
        );
    }
}
