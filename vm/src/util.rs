//! Shared utilities: atomic filesystem operations, hashing, Python-compatible
//! command execution, downloads, time stamps and small parsers.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::py_json;
use crate::{Error, Result};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn message<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::Message(text.into()))
}

// ---- time ---------------------------------------------------------------

fn format_time(format: &str, utc: bool) -> String {
    format_time_at(unsafe { libc::time(std::ptr::null_mut()) }, format, utc)
}

fn format_time_at(now: libc::time_t, format: &str, utc: bool) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        if utc {
            libc::gmtime_r(&now, &mut tm)
        } else {
            libc::localtime_r(&now, &mut tm)
        }
    };
    if ok.is_null() {
        return String::new();
    }
    let mut buffer: [libc::c_char; 64] = [0; 64];
    let fmt = std::ffi::CString::new(format).unwrap_or_default();
    let length = unsafe { libc::strftime(buffer.as_mut_ptr(), buffer.len(), fmt.as_ptr(), &tm) };
    if length == 0 {
        return String::new();
    }
    // `c_char` is u8 on Linux/ARM64 but i8 on macOS, so the cast is needed there.
    #[allow(clippy::unnecessary_cast)]
    let bytes: Vec<u8> = buffer[..length].iter().map(|value| *value as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The standard persisted timestamp: Unix milliseconds.
pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// Normalize a persisted timestamp value to Unix milliseconds. New state stores
/// a number; older state files carry a `utcnow()` ISO string.
pub fn timestamp_millis(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number.as_i64(),
        serde_json::Value::String(text) => parse_utc_stamp(text)
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_millis() as i64),
        _ => None,
    }
}

/// Python `utcnow()`: `datetime.now(timezone.utc).isoformat(timespec="seconds")`.
pub fn utcnow() -> String {
    format_time("%Y-%m-%dT%H:%M:%S+00:00", true)
}

/// `utcnow()` format for an arbitrary instant (for comparing journal times).
pub fn utcstamp_of(at: std::time::SystemTime) -> String {
    let secs = at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or_default();
    format_time_at(secs, "%Y-%m-%dT%H:%M:%S+00:00", true)
}

/// `utcnow()` display text for a stored millisecond timestamp.
pub fn utcstamp_from_millis(ms: i64) -> String {
    format_time_at(
        ms.div_euclid(1000) as libc::time_t,
        "%Y-%m-%dT%H:%M:%S+00:00",
        true,
    )
}

/// Parse a `utcnow()` stamp (`2026-11-15T13:26:45+00:00`).
pub fn parse_utc_stamp(text: &str) -> Option<std::time::SystemTime> {
    let text = text.trim().trim_end_matches("+00:00");
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (t.next()??, t.next()??, t.next()??);
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = days * 86400 + hour * 3600 + minute * 60 + second;
    u64::try_from(secs)
        .ok()
        .map(|secs| std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

/// Python `localstamp()`: local time `%Y%m%d-%H%M%S`.
pub fn localstamp() -> String {
    format_time("%Y%m%d-%H%M%S", false)
}

// ---- filesystem ---------------------------------------------------------

pub fn ensure_dir(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path)?;
    Ok(path.to_path_buf())
}

pub fn sha256_bytes(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex_lower(&hasher.finalize())
}

pub fn sha256_text(text: &str) -> String {
    sha256_bytes(text.as_bytes())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    hash_file::<Sha256>(path)
}

pub fn sha512_file(path: &Path) -> Result<String> {
    hash_file::<sha2::Sha512>(path)
}

fn hash_file<D: sha2::Digest>(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = D::new();
    let mut buffer = vec![0u8; 8 * 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

pub fn hex_lower(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

fn temp_path(parent: &Path, name: &str) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.subsec_nanos())
        .unwrap_or(0);
    parent.join(format!(".{name}.{pid}.{counter}.{nanos}.tmp"))
}

pub fn atomic_write_bytes(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("artifact");
    let temp = temp_path(parent, name);
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)?;
        file.write_all(data)?;
        file.flush()?;
        file.sync_all()?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))?;
        fs::rename(&temp, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

pub fn atomic_write_text(path: &Path, text: &str, mode: u32) -> Result<()> {
    atomic_write_bytes(path, text.as_bytes(), mode)
}

pub fn atomic_write_json(path: &Path, value: &Value, mode: u32) -> Result<()> {
    atomic_write_text(path, &(py_json::dumps_pretty(value) + "\n"), mode)
}

pub fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

pub fn remove_path(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            fs::remove_file(path)
        }
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn copy_dir(source: &Path, destination: &Path) -> std::io::Result<()> {
    ensure_dir(destination).map_err(|error| match error {
        Error::Io(inner) => inner,
        _ => std::io::Error::other("cannot create directory"),
    })?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

pub fn chmod(path: &Path, mode: u32) -> std::io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

// ---- formatting ---------------------------------------------------------

pub fn format_bytes(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "?".to_string();
    };
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    for (index, unit) in units.iter().enumerate() {
        if value < 1024.0 || index == units.len() - 1 {
            if *unit == "B" {
                return format!("{} B", bytes);
            }
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{bytes} B")
}

pub fn parse_size_bytes(value: &str) -> Result<u64> {
    let trimmed = value.trim();
    let upper = trimmed.to_uppercase();
    let (number, suffix) = match upper.char_indices().last() {
        Some((index, ch)) if ch.is_ascii_alphabetic() => (&upper[..index], &upper[index..]),
        _ => (upper.as_str(), ""),
    };
    let number: u64 = number
        .trim()
        .parse()
        .map_err(|_| Error::Message(format!("invalid size: {value:?} (expected e.g. 20G)")))?;
    let multiplier: u64 = match suffix.trim() {
        "" => 1,
        "K" => 1024,
        "M" => 1024 * 1024,
        "G" => 1024 * 1024 * 1024,
        "T" => 1024u64.pow(4),
        "P" => 1024u64.pow(5),
        _ => {
            return message(format!("invalid size: {value:?} (expected e.g. 20G)"));
        }
    };
    Ok(number * multiplier)
}

/// Python's `format(float, "g")` for the 1..4 compositor scales used here.
pub fn py_g(value: f64) -> String {
    if value.fract() == 0.0 {
        return format!("{}", value as i64);
    }
    let text = format!("{value:.6}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    trimmed.to_string()
}

/// `shlex.quote`.
pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }
    let safe = value.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-'
            )
    });
    if safe {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn which(name: &str) -> Option<PathBuf> {
    if name.contains('/') {
        let path = PathBuf::from(name);
        return if executable(&path) { Some(path) } else { None };
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(name);
        if executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn executable(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    pid > 1 && unsafe { libc::kill(pid, 0) == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

pub fn send_signal(pid: u32, signal: i32) {
    if let Ok(pid) = libc::pid_t::try_from(pid)
        && pid > 1
    {
        unsafe { libc::kill(pid, signal) };
    }
}

pub fn random_password(length: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut random = [0u8; 64];
    let mut filled = 0;
    if let Ok(mut file) = File::open("/dev/urandom") {
        while filled < random.len() {
            match file.read(&mut random[filled..]) {
                Ok(0) | Err(_) => break,
                Ok(read) => filled += read,
            }
        }
    }
    if filled < random.len() {
        for (index, byte) in random.iter_mut().enumerate() {
            *byte = (std::process::id() as usize + index * 131) as u8;
        }
    }
    (0..length)
        .map(|index| ALPHABET[random[index % random.len()] as usize % ALPHABET.len()] as char)
        .collect()
}

pub fn port_free(port: u16, host: &str) -> bool {
    match std::net::TcpListener::bind((host, port)) {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(_) => false,
    }
}

pub fn disk_free_bytes(path: &Path) -> u64 {
    let c_path = std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return u64::MAX;
    }
    (stat.f_bavail as u64).saturating_mul(stat.f_bsize as u64)
}

pub fn tail_file(path: &Path, lines: usize) -> String {
    let text = read_text(path);
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}

// ---- process execution --------------------------------------------------

/// Result of a captured command: (exit code, combined stdout+stderr).
pub struct CaptureResult {
    pub code: i32,
    pub output: String,
}

pub fn capture(argv: &[String], timeout: Duration, input: Option<&str>) -> CaptureResult {
    capture_env(argv, timeout, input, None, None)
}

pub fn capture_env(
    argv: &[String],
    timeout: Duration,
    input: Option<&str>,
    cwd: Option<&Path>,
    env: Option<&[(String, String)]>,
) -> CaptureResult {
    capture_with_stdin(argv, timeout, input, None, cwd, env)
}

/// Capture a command with an open file as stdin, without buffering large inputs.
pub(crate) fn capture_file(argv: &[String], timeout: Duration, input: File) -> CaptureResult {
    capture_with_stdin(argv, timeout, None, Some(input), None, None)
}

fn capture_with_stdin(
    argv: &[String],
    timeout: Duration,
    input: Option<&str>,
    file: Option<File>,
    cwd: Option<&Path>,
    env: Option<&[(String, String)]>,
) -> CaptureResult {
    if argv.is_empty() {
        return CaptureResult {
            code: 127,
            output: "command not found: <empty>".to_string(),
        };
    }
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.stdin(match file {
        Some(file) => Stdio::from(file),
        None if input.is_some() => Stdio::piped(),
        None => Stdio::null(),
    });
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = env {
        command.envs(env.iter().map(|(key, value)| (key, value)));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CaptureResult {
                code: 127,
                output: format!("command not found: {}", argv[0]),
            };
        }
        Err(error) => {
            return CaptureResult {
                code: 125,
                output: error.to_string(),
            };
        }
    };
    if let (Some(data), Some(mut stdin)) = (input, child.stdin.take()) {
        let data = data.as_bytes().to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_handle = thread::spawn(move || read_stream(stdout));
    let err_handle = thread::spawn(move || read_stream(stderr));
    let started = Instant::now();
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(15));
            }
            Err(_) => break,
        }
    }
    let status = child.wait().ok();
    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    let mut output = String::from_utf8_lossy(&stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&stderr));
    let code = if timed_out {
        124
    } else {
        status.and_then(|value| value.code()).unwrap_or(125)
    };
    CaptureResult { code, output }
}

fn read_stream(stream: Option<impl Read>) -> Vec<u8> {
    let Some(mut stream) = stream else {
        return Vec::new();
    };
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
    buffer
}

pub struct RunOptions<'a> {
    pub cwd: Option<&'a Path>,
    pub env: Option<&'a [(String, String)]>,
    pub timeout: Duration,
    pub check: bool,
    pub input: Option<&'a str>,
}

impl<'a> Default for RunOptions<'a> {
    fn default() -> Self {
        Self {
            cwd: None,
            env: None,
            timeout: Duration::from_secs(120),
            check: true,
            input: None,
        }
    }
}

/// Run a command, stream combined output into a step log and check the status.
pub fn run(argv: &[String], log: &Arc<Mutex<File>>, options: RunOptions<'_>) -> Result<i32> {
    if argv.is_empty() {
        return message("command not found: <empty>");
    }
    if let Ok(mut handle) = log.lock() {
        let rendered: Vec<String> = argv.iter().map(|part| shell_quote(part)).collect();
        let _ = writeln!(handle, "$ {}", rendered.join(" "));
        let _ = handle.flush();
    }
    let started = Instant::now();
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if options.input.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    if let Some(cwd) = options.cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = options.env {
        command.envs(env.iter().map(|(key, value)| (key, value)));
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return message(format!("command not found: {}", argv[0]));
        }
        Err(error) => return Err(Error::Io(error)),
    };
    if let (Some(data), Some(mut stdin)) = (options.input, child.stdin.take()) {
        let data = data.as_bytes().to_vec();
        thread::spawn(move || {
            let _ = stdin.write_all(&data);
        });
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let log_out = log.clone();
    let out_handle = thread::spawn(move || copy_stream(stdout, log_out));
    let log_err = log.clone();
    let err_handle = thread::spawn(move || copy_stream(stderr, log_err));
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= options.timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) => break,
        }
    }
    let status = child.wait().ok();
    let _ = out_handle.join();
    let _ = err_handle.join();
    let code = if timed_out {
        124
    } else {
        status.and_then(|value| value.code()).unwrap_or(125)
    };
    if let Ok(mut handle) = log.lock() {
        if timed_out {
            let _ = writeln!(handle, "[timeout after {}s]", options.timeout.as_secs());
        }
        let _ = writeln!(
            handle,
            "[exit {code}, {:.1}s]",
            started.elapsed().as_secs_f64()
        );
        let _ = handle.flush();
    }
    if timed_out {
        return message(format!("command timed out: {}", argv[0]));
    }
    if options.check && code != 0 {
        let rendered: Vec<String> = argv.iter().map(|part| shell_quote(part)).collect();
        return message(format!(
            "command failed (exit {code}): {}",
            rendered.join(" ")
        ));
    }
    Ok(code)
}

fn copy_stream(stream: Option<impl Read>, log: Arc<Mutex<File>>) {
    let Some(mut stream) = stream else {
        return;
    };
    let mut chunk = [0u8; 65536];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if let Ok(mut handle) = log.lock() {
                    let _ = handle.write_all(&chunk[..read]);
                    let _ = handle.flush();
                }
            }
        }
    }
}

pub fn log_line(log: &Arc<Mutex<File>>, text: &str) {
    if let Ok(mut handle) = log.lock() {
        let _ = handle.write_all(text.as_bytes());
        if !text.ends_with('\n') {
            let _ = handle.write_all(b"\n");
        }
        let _ = handle.flush();
    }
}

pub fn host_ip(interface: &str) -> Option<String> {
    let result = capture(
        &[
            "ipconfig".to_string(),
            "getifaddr".to_string(),
            interface.to_string(),
        ],
        Duration::from_secs(3),
        None,
    );
    if result.code == 0 && !result.output.trim().is_empty() {
        return Some(result.output.trim().to_string());
    }
    let route = capture(
        &[
            "route".to_string(),
            "-n".to_string(),
            "get".to_string(),
            "default".to_string(),
        ],
        Duration::from_secs(3),
        None,
    );
    if route.code == 0 {
        for line in route.output.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("interface:") {
                let iface = rest.trim();
                let ip = capture(
                    &[
                        "ipconfig".to_string(),
                        "getifaddr".to_string(),
                        iface.to_string(),
                    ],
                    Duration::from_secs(3),
                    None,
                );
                if ip.code == 0 && !ip.output.trim().is_empty() {
                    return Some(ip.output.trim().to_string());
                }
            }
        }
    }
    None
}

/// Locks that survive a panic elsewhere. A thread that panicked while
/// holding a lock leaves it poisoned; the data inside is still the last value
/// written, and taking every later caller down with it helps no one.
pub trait LockExt<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockExt<T> for std::sync::Mutex<T> {
    fn locked(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// [`LockExt`] for readers–writer locks.
pub trait RwLockExt<T> {
    fn read_lock(&self) -> std::sync::RwLockReadGuard<'_, T>;
    fn write_lock(&self) -> std::sync::RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for std::sync::RwLock<T> {
    fn read_lock(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_lock(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    #[test]
    fn utc_stamps_round_trip() {
        let now = std::time::SystemTime::now();
        let parsed = parse_utc_stamp(&utcstamp_of(now)).unwrap();
        let diff = now.duration_since(parsed).unwrap_or_default().as_secs();
        assert!(diff <= 1, "{diff}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size_bytes("20G").unwrap(), 20 * 1024 * 1024 * 1024);
        assert_eq!(parse_size_bytes(" 512M ").unwrap(), 512 * 1024 * 1024);
        assert!(parse_size_bytes("wide").is_err());
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(format_bytes(Some(1023)), "1023 B");
        assert_eq!(format_bytes(Some(1536)), "1.5 KiB");
        assert_eq!(format_bytes(None), "?");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("plain-value_1.2"), "plain-value_1.2");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn g_format() {
        assert_eq!(py_g(1.0), "1");
        assert_eq!(py_g(2.0), "2");
        assert_eq!(py_g(1.5), "1.5");
    }
}
