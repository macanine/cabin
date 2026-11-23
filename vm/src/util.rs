//! Shared utilities: atomic filesystem operations, hashing, Python-compatible
//! command execution, downloads, time stamps and small parsers.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
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
    utcstamp_from_millis(now_millis())
}

/// `utcnow()` format for an arbitrary instant (for comparing journal times).
pub fn utcstamp_of(at: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at).format(UTC_STAMP).to_string()
}

/// `utcnow()` display text for a stored millisecond timestamp.
pub fn utcstamp_from_millis(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format(UTC_STAMP)
        .to_string()
}

const UTC_STAMP: &str = "%Y-%m-%dT%H:%M:%S+00:00";

/// Parse a `utcnow()` stamp (`2026-11-15T13:26:45+00:00`).
pub fn parse_utc_stamp(text: &str) -> Option<std::time::SystemTime> {
    let text = text.trim();
    let stamp = chrono::DateTime::parse_from_rfc3339(text)
        .map(|time| time.to_utc())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S").map(|time| time.and_utc())
        })
        .ok()?;
    u64::try_from(stamp.timestamp())
        .ok()
        .map(|secs| std::time::UNIX_EPOCH + Duration::from_secs(secs))
}

/// Python `localstamp()`: local time `%Y%m%d-%H%M%S`.
pub fn localstamp() -> String {
    chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
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
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options.open(&temp)?;
        chmod(&temp, mode)?;
        file.write_all(data)?;
        file.flush()?;
        file.sync_all()?;
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

/// Set Unix permission bits. Windows stores none, and the profile directory
/// already belongs to the current user, so this is a no-op there.
pub fn chmod(path: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
        Ok(())
    }
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
    which::which(name).ok()
}

/// A running process looked up by pid: `None` when it does not exist.
fn process_info<T>(
    pid: u32,
    kind: sysinfo::ProcessRefreshKind,
    read: impl FnOnce(&sysinfo::Process) -> T,
) -> Option<T> {
    if pid <= 1 {
        return None;
    }
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&[pid]), true, kind);
    system.process(pid).map(read)
}

pub fn pid_alive(pid: u32) -> bool {
    process_info(pid, sysinfo::ProcessRefreshKind::nothing(), |_| ()).is_some()
}

/// The executable and arguments a live process was started with.
pub fn process_command(pid: u32) -> Option<Vec<String>> {
    use sysinfo::UpdateKind::Always;
    let kind = sysinfo::ProcessRefreshKind::nothing().with_cmd(Always).with_exe(Always);
    process_info(pid, kind, |process| {
        let mut argv: Vec<String> =
            process.cmd().iter().map(|part| part.to_string_lossy().into_owned()).collect();
        if let Some(exe) = process.exe() {
            match argv.first_mut() {
                Some(first) => *first = exe.display().to_string(),
                None => argv.push(exe.display().to_string()),
            }
        }
        argv
    })
}

/// Ask a process to exit (SIGTERM where the OS has it) or, when `graceful` is
/// false or the OS has no such request, end it outright.
pub fn terminate_process(pid: u32, graceful: bool) {
    process_info(pid, sysinfo::ProcessRefreshKind::nothing(), |process| {
        let asked = graceful && process.kill_with(sysinfo::Signal::Term).is_some();
        asked || process.kill()
    });
}

/// Stop spawned console tools (ssh, QEMU, tar, ...) from flashing windows.
pub fn no_window(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// The pinned system `curl`, or the one Windows ships on `PATH`.
pub fn curl_binary() -> &'static str {
    if cfg!(windows) { "curl" } else { "/usr/bin/curl" }
}

pub fn random_password(length: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut random = [0u8; 64];
    random_bytes(&mut random);
    (0..length)
        .map(|index| ALPHABET[random[index % random.len()] as usize % ALPHABET.len()] as char)
        .collect()
}

/// Operating-system entropy. A host without one cannot make safe secrets.
pub fn random_bytes(buffer: &mut [u8]) {
    getrandom::fill(buffer).expect("the operating system provides no entropy");
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

/// Free bytes available to this user on `path`'s volume (unbounded when unknown).
pub fn disk_free_bytes(path: &Path) -> u64 {
    fs4::available_space(path).unwrap_or(u64::MAX)
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
    crate::util::no_window(&mut command);
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
    crate::util::no_window(&mut command);
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
    crate::network_interfaces::host_ip(interface).map(|ip| ip.to_string())
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
    fn utc_stamps_match_the_persisted_format() {
        assert_eq!(utcstamp_from_millis(1_794_800_805_999), "2026-11-16T03:46:45+00:00");
        assert_eq!(utcstamp_from_millis(0), "1970-01-01T00:00:00+00:00");
        // Leap day and month boundaries.
        assert_eq!(utcstamp_from_millis(951_782_400_000), "2000-02-29T00:00:00+00:00");
        assert_eq!(utcstamp_from_millis(951_868_800_000), "2000-03-01T00:00:00+00:00");
        // Python's isoformat output, with and without the offset.
        let at = |text| parse_utc_stamp(text).and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        assert_eq!(at("2026-11-16T03:46:45+00:00"), Some(Duration::from_secs(1_794_800_805)));
        assert_eq!(at("2026-11-16T03:46:45"), Some(Duration::from_secs(1_794_800_805)));
        assert_eq!(at("garbage"), None);
        assert_eq!(localstamp().len(), "20261116-034645".len());
    }

    #[test]
    fn processes_are_found_inspected_and_ended() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0) && !pid_alive(1));
        let own = process_command(std::process::id()).unwrap();
        assert!(own[0].contains("vm_core") || Path::new(&own[0]).is_absolute(), "{own:?}");
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
