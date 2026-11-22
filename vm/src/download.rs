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
}

fn active() -> &'static Mutex<HashMap<PathBuf, Arc<Active>>> {
    static ACTIVE: OnceLock<Mutex<HashMap<PathBuf, Arc<Active>>>> = OnceLock::new();
    ACTIVE.get_or_init(Default::default)
}

static CANCEL: AtomicBool = AtomicBool::new(false);

/// Stop every running download; each fails with "cancelled".
pub fn cancel_all() {
    CANCEL.store(true, Ordering::SeqCst);
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

pub(crate) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .resolver(crate::ipv4::resolve)
        .timeout_connect(Duration::from_secs(20))
        // Per read: a stalled connection fails and is resumed.
        .timeout_read(Duration::from_secs(60))
        .try_proxy_from_env(true)
        .user_agent(concat!("cabin/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// One attempt: continue `part` from its current size. `Ok(())` when the
/// server sent everything it promised.
fn attempt(url: &str, part: &Path, state: &Active, log: &Arc<Mutex<File>>) -> Result<()> {
    let offset = fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let mut request = agent().get(url);
    if offset > 0 {
        request = request.set("Range", &format!("bytes={offset}-"));
    }
    let response = match request.call() {
        // The part already holds the whole file.
        Err(ureq::Error::Status(416, _)) if offset > 0 => return Ok(()),
        Err(ureq::Error::Status(code, response)) => {
            return message(format!("HTTP {code} {} for {url}", response.status_text()));
        }
        Err(error) => return message(format!("could not connect: {error}")),
        Ok(response) => response,
    };
    let resumed = offset > 0 && response.status() == 206;
    if offset > 0 && !resumed {
        log_line(
            log,
            "the server does not support resuming; starting from zero",
        );
    }
    let length: Option<u64> = response
        .header("Content-Length")
        .and_then(|v| v.parse().ok());
    let start = if resumed { offset } else { 0 };
    if let Some(length) = length {
        state.total.store(start + length, Ordering::Relaxed);
    }
    state.done.store(start, Ordering::Relaxed);
    *state.attempt.locked() = (Instant::now(), start);

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(resumed)
        .truncate(!resumed)
        .open(part)?;
    let mut reader = response.into_reader();
    let mut buffer = vec![0u8; CHUNK];
    let mut done = start;
    let mut next_report = 10u64;
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            return Err(Error::Message("download cancelled".into()));
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|error| Error::Message(format!("connection dropped: {error}")))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        done += read as u64;
        state.done.store(done, Ordering::Relaxed);
        if let Some(total) = length.map(|l| start + l) {
            let percent = done * 100 / total.max(1);
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
    file.flush()?;
    match length {
        Some(length) if done < start + length => message(format!(
            "connection closed early at {}",
            format_bytes(Some(done))
        )),
        _ => Ok(()),
    }
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

/// Download `url` to `destination` and verify its checksum. An existing,
/// verified file is reused; a partial `.part` file is resumed.
pub fn download_verified(
    url: &str,
    destination: &Path,
    expected: &Checksum,
    log: &Arc<Mutex<File>>,
) -> Result<()> {
    if let Some(parent) = destination.parent() {
        crate::util::ensure_dir(parent)?;
    }
    let part = partial_path(destination);
    log_line(
        log,
        &format!("download {url}\n  -> {}", destination.display()),
    );
    if destination.exists() {
        let actual = expected.of_file(destination)?;
        if actual == expected.hex() {
            log_line(
                log,
                &format!("already present and verified: {}", destination.display()),
            );
            return Ok(());
        }
        log_line(
            log,
            &format!("existing file has the wrong checksum ({actual}); downloading again"),
        );
        let _ = fs::remove_file(destination);
    }

    CANCEL.store(false, Ordering::SeqCst);
    let state = Arc::new(Active {
        done: AtomicU64::new(fs::metadata(&part).map(|m| m.len()).unwrap_or(0)),
        total: AtomicU64::new(0),
        verifying: AtomicBool::new(false),
        attempt: Mutex::new((Instant::now(), 0)),
    });
    active().locked()
        .insert(destination.to_path_buf(), state.clone());
    let _registered = Registered(destination.to_path_buf());

    let mut last_error = None;
    for number in 1..=ATTEMPTS {
        match attempt(url, &part, &state, log) {
            Ok(()) => {
                last_error = None;
                break;
            }
            // Cancelled, or a client error (404, 403…) that a retry cannot fix.
            Err(error)
                if error.to_string().contains("cancelled")
                    || error.to_string().starts_with("HTTP 4") =>
            {
                return Err(error);
            }
            Err(error) => {
                log_line(log, &format!("attempt {number}/{ATTEMPTS} failed: {error}"));
                last_error = Some(error);
                if number < ATTEMPTS {
                    std::thread::sleep(Duration::from_secs(2 * number as u64));
                }
            }
        }
    }
    if let Some(error) = last_error {
        return message(format!(
            "download failed after {ATTEMPTS} attempts: {error}"
        ));
    }

    state.verifying.store(true, Ordering::Relaxed);
    log_line(log, &format!("verifying {} …", expected.name()));
    let actual = expected.of_file(&part)?;
    if actual != expected.hex() {
        let _ = fs::remove_file(&part);
        return message(format!(
            "checksum mismatch for {url}: expected {}, got {actual}",
            expected.hex()
        ));
    }
    fs::rename(&part, destination)?;
    log_line(
        log,
        &format!("download complete, checksum ok: {}", destination.display()),
    );
    Ok(())
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
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        download_verified(&url, &destination, &sha(&body), &log_file(&dir)).unwrap();
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

    #[test]
    #[ignore = "requires outbound network"]
    fn real_https_download() {
        let response = agent()
            .get("https://cloud-images.ubuntu.com/minimal/releases/noble/release/SHA256SUMS")
            .call()
            .expect("HTTPS request");
        let text = response.into_string().unwrap();
        assert!(text.contains("minimal-cloudimg-arm64.img"), "{text}");
    }
}
