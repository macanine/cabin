//! Shared utilities: atomic filesystem operations, hashing, Python-compatible
//! command execution, downloads, time stamps and small parsers.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
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
    chrono::DateTime::<chrono::Utc>::from(at)
        .format(UTC_STAMP)
        .to_string()
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
            chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S")
                .map(|time| time.and_utc())
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
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("artifact");
    let temp = temp_path(parent, name);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(mode);
    #[cfg(not(unix))]
    let _ = mode;
    let file = options.open(&temp)?;
    let result = (|| -> Result<()> {
        let mut file = file;
        #[cfg(unix)]
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.write_all(data)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
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
        Ok(metadata) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::FileTypeExt;
                if metadata.permissions().readonly() {
                    let mut perms = metadata.permissions();
                    // On Windows this clears FILE_ATTRIBUTE_READONLY only.
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);
                    let _ = fs::set_permissions(path, perms);
                }
                if metadata.file_type().is_symlink_dir() {
                    return fs::remove_dir(path);
                }
            }
            if metadata.file_type().is_symlink() || metadata.is_file() {
                fs::remove_file(path)
            } else if metadata.is_dir() {
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            }
        }
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
    number
        .checked_mul(multiplier)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| Error::Message(format!("size is out of range: {value:?}")))
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
    let kind = sysinfo::ProcessRefreshKind::nothing()
        .with_cmd(Always)
        .with_exe(Always);
    process_info(pid, kind, |process| {
        let mut argv: Vec<String> = process
            .cmd()
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
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
    if cfg!(windows) {
        "curl"
    } else {
        "/usr/bin/curl"
    }
}

pub fn random_password(length: usize) -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    // Reject the bytes beyond the largest multiple of the alphabet size to
    // give each character the same probability.
    let limit = 256 - 256 % ALPHABET.len();
    let mut password = String::with_capacity(length);
    let mut random = [0u8; 64];
    while password.len() < length {
        random_bytes(&mut random)?;
        let remaining = length - password.len();
        password.extend(
            random
                .iter()
                .map(|&byte| usize::from(byte))
                .filter(|&index| index < limit)
                .take(remaining)
                .map(|index| char::from(ALPHABET[index % ALPHABET.len()])),
        );
    }
    Ok(password)
}

pub fn random_bytes(buffer: &mut [u8]) -> std::io::Result<()> {
    getrandom::fill(buffer).map_err(|error| {
        std::io::Error::other(format!("Could not get operating-system entropy: {error}"))
    })
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
    let text = cabin_diagnostics::tail(path, 128 * 1024, 0);
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
    let Some((program, args)) = argv.split_first() else {
        return CaptureResult {
            code: 127,
            output: "command not found: <empty>".to_string(),
        };
    };
    let command = prepare_command(program, args, cwd, env);
    let mut output = [Vec::new(), Vec::new()];
    let result = execute_command(command, timeout, input, file, |index, chunk| {
        output[index].extend_from_slice(chunk);
    });
    let [stdout, stderr] = output;
    let mut output = String::from_utf8_lossy(&stdout).into_owned();
    output.push_str(&String::from_utf8_lossy(&stderr));
    let code = match result {
        Ok(Some(status)) => status.code().unwrap_or(125),
        Ok(None) => 124,
        Err(error) if error.kind() == io::ErrorKind::NotFound && output.is_empty() => {
            output = format!("command not found: {program}");
            127
        }
        Err(error) => {
            if !output.is_empty() && !output.ends_with('\n') {
                output.push('\n');
            }
            output.push_str(&error.to_string());
            125
        }
    };
    CaptureResult { code, output }
}

fn prepare_command(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    env: Option<&[(String, String)]>,
) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    if let Some(env) = env {
        command.envs(env.iter().map(|(key, value)| (key, value)));
    }
    command
}

/// Returns `None` on timeout. Keep the command deadline active until every
/// pipe is closed, including any copies that the child passed on.
fn execute_command(
    mut command: Command,
    timeout: Duration,
    input: Option<&str>,
    file: Option<File>,
    mut output: impl FnMut(usize, &[u8]),
) -> io::Result<Option<ExitStatus>> {
    use crate::platform::command_pipe;

    let started = Instant::now();
    let (stdout, stdout_writer) = command_pipe::output()?;
    let (stderr, stderr_writer) = command_pipe::output()?;
    command.stdout(stdout_writer).stderr(stderr_writer);
    let mut stdin = match (file, input) {
        (Some(file), _) => {
            command.stdin(file);
            None
        }
        (None, Some(input)) => {
            let (reader, writer) = command_pipe::input()?;
            command.stdin(reader);
            Some((writer, input.as_bytes()))
        }
        (None, None) => {
            command.stdin(Stdio::null());
            None
        }
    };
    let mut readers = [Some(stdout), Some(stderr)];
    let mut child = crate::platform::CommandTree::spawn(&mut command)?;
    // Command owns the child ends of explicit pipes and can otherwise keep
    // them open after the subprocess exits.
    drop(command);
    let mut chunk = [0; 64 * 1024];
    let result = (|| loop {
        if started.elapsed() >= timeout {
            break Ok(None);
        }
        let mut progressed = false;
        for (index, pipe) in readers.iter_mut().enumerate() {
            if let Some(bytes) = read_command_output(pipe, &mut chunk)? {
                output(index, bytes);
                progressed = true;
            }
        }
        progressed |= write_command_input(&mut stdin)?;
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break Ok(None);
        }
        // Reap the leader only once the pipes close. Until then its PID
        // still identifies the isolated Unix process group on timeout.
        if stdin.is_none()
            && readers.iter().all(Option::is_none)
            && let Some(status) = child.try_wait()?
        {
            break Ok(Some(status));
        }
        if !progressed {
            thread::sleep(remaining.min(Duration::from_millis(10)));
        }
    })();
    if !matches!(result, Ok(Some(_))) {
        child.terminate();
        drop(stdin);
        drop(readers);
    }
    result
}

fn read_command_output<'a>(
    pipe: &mut Option<PipeReader>,
    chunk: &'a mut [u8],
) -> io::Result<Option<&'a [u8]>> {
    if let Some(reader) = pipe {
        match crate::platform::command_pipe::read(reader, chunk) {
            Ok(0) => *pipe = None,
            Ok(read) => return Ok(Some(&chunk[..read])),
            Err(error) if pipe_pending(&error) => (),
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

fn write_command_input(stdin: &mut Option<(PipeWriter, &[u8])>) -> io::Result<bool> {
    if let Some((writer, pending)) = stdin {
        match writer.write(pending) {
            Ok(written) => {
                *pending = &pending[written..];
                if pending.is_empty() {
                    *stdin = None;
                }
                return Ok(written > 0);
            }
            Err(error) if pipe_pending(&error) => (),
            // Tools can exit or deliberately close stdin before reading it all.
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => *stdin = None,
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn pipe_pending(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
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

/// Stream logs off the command thread. Keep at most 16 chunks (1 MiB) queued
/// and give the logger a short grace period after the child has been reaped.
struct CommandLog {
    chunks: SyncSender<Vec<u8>>,
    footer: SyncSender<String>,
    finished: Receiver<()>,
    cancelled: Arc<AtomicBool>,
    skipped: usize,
}

impl CommandLog {
    fn start(log: &Arc<Mutex<File>>, program: &str) -> io::Result<Self> {
        let (chunks, pending) = mpsc::sync_channel::<Vec<u8>>(16);
        let (footer, summary) = mpsc::sync_channel::<String>(1);
        let (complete, finished) = mpsc::sync_channel::<()>(0);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = cancelled.clone();
        let log = log.clone();
        let header = format!("$ {program}\n");
        thread::Builder::new()
            .name("vm-command-log".into())
            .spawn(move || {
                let _complete = complete;
                let write = |bytes: &[u8]| loop {
                    if stop.load(Ordering::Acquire) {
                        return false;
                    }
                    match log.try_lock() {
                        Ok(mut file) => {
                            return file.write_all(bytes).and_then(|()| file.flush()).is_ok();
                        }
                        Err(std::sync::TryLockError::WouldBlock) => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(std::sync::TryLockError::Poisoned(_)) => return false,
                    }
                };
                if !write(header.as_bytes()) {
                    return;
                }
                for chunk in pending {
                    if !write(&chunk) {
                        return;
                    }
                }
                if let Ok(summary) = summary.try_recv() {
                    write(summary.as_bytes());
                }
            })?;
        Ok(Self {
            chunks,
            footer,
            finished,
            cancelled,
            skipped: 0,
        })
    }

    fn write(&mut self, bytes: &[u8]) {
        if matches!(
            self.chunks.try_send(bytes.to_vec()),
            Err(TrySendError::Full(_))
        ) {
            self.skipped = self.skipped.saturating_add(bytes.len());
        }
    }

    fn finish(self, mut footer: String) {
        if self.skipped > 0 {
            footer = format!(
                "\n[omitted {} bytes while log was busy]\n{footer}",
                self.skipped
            );
        }
        let _ = self.footer.try_send(footer);
        drop(self.chunks);
        let _ = self.finished.recv_timeout(Duration::from_millis(100));
        // A filesystem write can remain blocked in the OS. The command has
        // already finished and never joins that worker.
        self.cancelled.store(true, Ordering::Release);
    }
}

/// Run a command, stream combined output into a step log and check the status.
pub fn run(argv: &[String], log: &Arc<Mutex<File>>, options: RunOptions<'_>) -> Result<i32> {
    let Some((command, args)) = argv.split_first() else {
        return message("command not found: <empty>");
    };
    // Command arguments can carry scripts, environment values, and secrets.
    let program = Path::new(command)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let started = Instant::now();
    let mut log = CommandLog::start(log, &program).ok();
    let process = prepare_command(command, args, options.cwd, options.env);
    let status = execute_command(process, options.timeout, options.input, None, |_, chunk| {
        if let Some(log) = &mut log {
            log.write(chunk);
        }
    })
    .map(|status| {
        (
            status.is_none(),
            status.map_or(124, |status| status.code().unwrap_or(125)),
        )
    });
    if let Some(log) = log {
        let mut footer = String::new();
        if let Ok((timed_out, code)) = status {
            if timed_out {
                footer = format!("[timeout after {}s]\n", options.timeout.as_secs());
            }
            footer.push_str(&format!(
                "[exit {code}, {:.1}s]\n",
                started.elapsed().as_secs_f64()
            ));
        }
        log.finish(footer);
    }
    let (timed_out, code) = match status {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return message(format!("command not found: {command}"));
        }
        other => other?,
    };
    if timed_out {
        return message(format!("command timed out: {program}"));
    }
    if options.check && code != 0 {
        return message(format!("{program} failed (exit {code})"));
    }
    Ok(code)
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
        self.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// [`LockExt`] for readers–writer locks.
pub trait RwLockExt<T> {
    fn read_lock(&self) -> std::sync::RwLockReadGuard<'_, T>;
    fn write_lock(&self) -> std::sync::RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for std::sync::RwLock<T> {
    fn read_lock(&self) -> std::sync::RwLockReadGuard<'_, T> {
        self.read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn write_lock(&self) -> std::sync::RwLockWriteGuard<'_, T> {
        self.write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod stamp_tests {
    use super::*;

    #[test]
    fn utc_stamps_match_the_persisted_format() {
        assert_eq!(
            utcstamp_from_millis(1_794_800_805_999),
            "2026-11-16T03:46:45+00:00"
        );
        assert_eq!(utcstamp_from_millis(0), "1970-01-01T00:00:00+00:00");
        // Leap day and month boundaries.
        assert_eq!(
            utcstamp_from_millis(951_782_400_000),
            "2000-02-29T00:00:00+00:00"
        );
        assert_eq!(
            utcstamp_from_millis(951_868_800_000),
            "2000-03-01T00:00:00+00:00"
        );
        // Python's isoformat output, with and without the offset.
        let at = |text| {
            parse_utc_stamp(text).and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        };
        assert_eq!(
            at("2026-11-16T03:46:45+00:00"),
            Some(Duration::from_secs(1_794_800_805))
        );
        assert_eq!(
            at("2026-11-16T03:46:45"),
            Some(Duration::from_secs(1_794_800_805))
        );
        assert_eq!(at("garbage"), None);
        assert_eq!(localstamp().len(), "20261116-034645".len());
    }

    #[test]
    fn processes_are_found_inspected_and_ended() {
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(0) && !pid_alive(1));
        let own = process_command(std::process::id()).unwrap();
        assert!(
            own[0].contains("vm_core") || Path::new(&own[0]).is_absolute(),
            "{own:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIPE_TEST_ROLE: &str = "CABIN_TEST_COMMAND_PIPE_ROLE";
    const PIPE_TEST_DIR: &str = "CABIN_TEST_COMMAND_PIPE_DIR";

    fn command_fixture() -> Vec<String> {
        vec![
            std::env::current_exe()
                .unwrap()
                .into_os_string()
                .into_string()
                .unwrap(),
            "--exact".into(),
            "util::tests::command_pipe_fixture".into(),
            "--nocapture".into(),
        ]
    }

    #[test]
    // The fixture is a short-lived subprocess whose descendant must outlive it.
    #[allow(clippy::zombie_processes)]
    fn command_pipe_fixture() {
        match std::env::var(PIPE_TEST_ROLE).as_deref() {
            Ok("parent") => {
                let argv = command_fixture();
                let mut command = Command::new(&argv[0]);
                command.args(&argv[1..]).env(PIPE_TEST_ROLE, "descendant");
                no_window(&mut command);
                // The descendant inherits stdin, stdout and stderr. This
                // parent exits immediately without waiting for it.
                let _descendant = command.spawn().unwrap();
                println!("parent exiting");
            }
            Ok("descendant") => {
                eprintln!("descendant holding pipes");
                thread::sleep(Duration::from_secs(8));
            }
            Ok("roundtrip") => {
                let mut input = Vec::new();
                io::stdin().read_to_end(&mut input).unwrap();
                io::stdout().write_all(&input).unwrap();
                eprintln!("input complete");
            }
            Ok("blocked-log") => {
                let dir = PathBuf::from(std::env::var_os(PIPE_TEST_DIR).unwrap());
                fs::write(dir.join("pid"), std::process::id().to_string()).unwrap();
                let started = Instant::now();
                while !dir.join("write-output").exists() {
                    assert!(started.elapsed() < Duration::from_secs(5));
                    thread::sleep(Duration::from_millis(10));
                }
                eprintln!("output while log is blocked");
                fs::write(dir.join("output-written"), b"").unwrap();
                thread::sleep(Duration::from_secs(8));
            }
            _ => {}
        }
    }

    #[test]
    fn inherited_pipes_cannot_extend_capture_or_run_deadline() {
        let argv = command_fixture();
        let env = [(PIPE_TEST_ROLE.into(), "parent".into())];
        let input = "*".repeat(256 * 1024);
        let timeout = Duration::from_millis(1500);

        let start = Instant::now();
        let result = capture_env(&argv, timeout, Some(&input), None, Some(&env));
        assert_eq!(result.code, 124, "{}", result.output);
        assert!(
            result.output.contains("parent exiting"),
            "{}",
            result.output
        );
        assert!(
            result.output.contains("descendant holding pipes"),
            "{}",
            result.output
        );
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "{:?}",
            start.elapsed()
        );

        let dir = crate::sockets::test_dir();
        let path = dir.join("command.log");
        let log = Arc::new(Mutex::new(File::create(&path).unwrap()));
        let start = Instant::now();
        let error = run(
            &argv,
            &log,
            RunOptions {
                env: Some(&env),
                timeout,
                input: Some(&input),
                ..RunOptions::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("command timed out"), "{error}");
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "{:?}",
            start.elapsed()
        );
        drop(log);
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("parent exiting") && text.contains("descendant holding pipes"),
            "{text}"
        );
        assert!(text.contains("[exit 124,"), "{text}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn command_completes_large_pipe_io_before_deadline() {
        let env = [(PIPE_TEST_ROLE.into(), "roundtrip".into())];
        let input = "#".repeat(256 * 1024);
        let timeout = Duration::from_secs(5);
        let result = capture_env(&command_fixture(), timeout, Some(&input), None, Some(&env));
        assert_eq!(result.code, 0);
        assert!(result.output.contains(&input));
        assert!(
            result.output.ends_with("input complete\n")
                || result.output.ends_with("input complete\r\n")
        );
    }

    #[test]
    fn blocked_log_cannot_extend_run_deadline() {
        let dir = crate::sockets::test_dir();
        let log = Arc::new(Mutex::new(File::create(dir.join("command.log")).unwrap()));
        let env = [
            (PIPE_TEST_ROLE.into(), "blocked-log".into()),
            (PIPE_TEST_DIR.into(), dir.to_str().unwrap().into()),
        ];
        let (send, result) = mpsc::channel();
        let run_log = log.clone();
        let runner = thread::spawn(move || {
            let result = run(
                &command_fixture(),
                &run_log,
                RunOptions {
                    env: Some(&env),
                    timeout: Duration::from_millis(1500),
                    ..RunOptions::default()
                },
            );
            let _ = send.send(result);
        });

        let started = Instant::now();
        let pid = loop {
            if let Some(pid) = fs::read_to_string(dir.join("pid"))
                .ok()
                .and_then(|text| text.parse().ok())
            {
                break pid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "child did not start"
            );
            thread::sleep(Duration::from_millis(10));
        };
        // Take the lock after the child starts so the old inline sink stalls
        // on its output, with the child still running past the deadline.
        let guard = log.lock().unwrap();
        fs::write(dir.join("write-output"), b"").unwrap();
        let outcome = result.recv_timeout(Duration::from_secs(4));
        let reaped = !pid_alive(pid);
        drop(guard);
        runner.join().unwrap();
        assert!(
            dir.join("output-written").is_file(),
            "child did not write output"
        );
        drop(log);
        fs::remove_dir_all(dir).unwrap();
        let error = outcome
            .expect("run should return while the log is locked")
            .unwrap_err();
        assert!(error.to_string().contains("command timed out"), "{error}");
        assert!(
            reaped,
            "child survived the deadline while the log was locked"
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(parse_size_bytes("20G").unwrap(), 20 * 1024 * 1024 * 1024);
        assert_eq!(parse_size_bytes(" 512M ").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_size_bytes("16383P").unwrap(), 16383 * 1024u64.pow(5));
        assert!(parse_size_bytes("16384P").is_err());
        assert!(parse_size_bytes("0G").is_err());
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
