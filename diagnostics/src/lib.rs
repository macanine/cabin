//! Shared local logging policy for Cabin's host, tools, and guest.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

mod files;

use files::open_regular;

const SEGMENT_BYTES: u64 = 2 * 1024 * 1024;
const ARCHIVES: usize = 3;
const MESSAGE_CHARS: usize = 1024;
pub const EVENT_FILE: &str = "events.jsonl";
pub const RETENTION_DAYS: u64 = 30;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

struct Logger {
    path: PathBuf,
    source: &'static str,
    level: Level,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn init(directory: &Path, source: &'static str) {
    let level = match std::env::var("CABIN_LOG_LEVEL").as_deref() {
        Ok("debug") => Level::Debug,
        Ok("warn") => Level::Warn,
        Ok("error") => Level::Error,
        _ => Level::Info,
    };
    let _ = LOGGER.set(Logger {
        path: directory.join(EVENT_FILE),
        source,
        level,
    });
}

/// Keep application messages short and pass only diagnostic context. Never pass
/// request bodies, credentials, chat content, or environment dumps here.
pub fn event(
    level: Level,
    component: &'static str,
    name: &'static str,
    message: fmt::Arguments<'_>,
) {
    let Some(logger) = LOGGER.get() else { return };
    if level < logger.level {
        return;
    }
    let mut line = serde_json::json!({
        "ts": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
        "level": level.name(),
        "source": logger.source,
        "pid": std::process::id(),
        "component": component,
        "event": name,
        "message": message.to_string().chars().take(MESSAGE_CHARS).collect::<String>(),
    })
    .to_string();
    line.push('\n');
    if append_record(&logger.path, &line, false).is_err() {
        let _ = io::stderr().lock().write_all(line.as_bytes());
    }
}

#[derive(Clone, Copy)]
pub enum Capture {
    Append,
    Replace,
    New,
}

fn create_parent(path: &Path) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    Ok(())
}

/// Open a scoped raw capture. The same file can be inherited by a subprocess.
pub fn open_capture(path: &Path, mode: Capture) -> io::Result<File> {
    create_parent(path)?;
    let mut options = OpenOptions::new();
    match mode {
        Capture::Append => options.create(true).append(true),
        Capture::Replace => options.create(true).write(true),
        Capture::New => options.create_new(true).append(true),
    };
    let file = open_regular(path, &mut options)?;
    if matches!(mode, Capture::Replace) {
        file.set_len(0)?;
    }
    Ok(file)
}

fn archive(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{index}"));
    name.into()
}

/// Serialize writes and rotation across processes using a stable lock file.
/// Both the VM operation journal and the application event stream use this
/// writer. The operation journal requests a durable write before reporting success.
pub fn append_record(path: &Path, line: &str, durable: bool) -> io::Result<()> {
    append(path, line, durable, SEGMENT_BYTES)
}

fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".lock");
    name.into()
}

fn append(path: &Path, line: &str, durable: bool, limit: u64) -> io::Result<()> {
    let line_bytes = line.len() as u64;
    if line_bytes > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostic record exceeds the segment size",
        ));
    }
    create_parent(path)?;
    let lock = open_regular(
        &lock_path(path),
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false),
    )?;
    lock.lock()?;
    let open = || {
        open_regular(
            path,
            OpenOptions::new().create(true).read(true).append(true),
        )
    };
    let mut file = open()?;
    let length = file.metadata()?.len();
    let needs_separator = length > 0 && !starts_on_line(&mut file, length)?;
    if length
        .saturating_add(line_bytes)
        .saturating_add(u64::from(needs_separator))
        > limit
    {
        // Close before renaming, including on Windows. The stable lock remains held.
        drop(file);
        for index in (1..=ARCHIVES).rev() {
            let from = if index == 1 {
                path.to_path_buf()
            } else {
                archive(path, index - 1)
            };
            let to = archive(path, index);
            match fs::rename(from, to) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        file = open()?;
    } else if needs_separator {
        // Isolate a partial record left by an interrupted write.
        file.write_all(b"\n")?;
    }
    file.write_all(line.as_bytes())?;
    if durable {
        file.sync_all()?;
    }
    Ok(())
}

fn starts_on_line(file: &mut File, offset: u64) -> io::Result<bool> {
    if offset == 0 {
        return Ok(true);
    }
    file.seek(SeekFrom::Start(offset - 1))?;
    let mut byte = [0];
    file.read_exact(&mut byte)?;
    Ok(byte[0] == b'\n')
}

/// Read up to `max_bytes` from the end of a regular capture. `offset` excludes
/// earlier output from a live stream that is shared by successive attempts.
pub fn tail(path: &Path, max_bytes: u64, offset: u64) -> String {
    read_tail(path, max_bytes, offset).unwrap_or_default().0
}

fn read_tail(path: &Path, max_bytes: u64, offset: u64) -> io::Result<(String, u64)> {
    let mut file = open_regular(path, OpenOptions::new().read(true))?;
    let length = file.metadata()?.len();
    let offset = if length >= offset { offset } else { 0 };
    let start = length.saturating_sub(max_bytes).max(offset);
    let partial = start > offset && !starts_on_line(&mut file, start)?;
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(length - start).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = if partial {
        text.split_once('\n')
            .map_or("", |(_, rest)| rest)
            .to_owned()
    } else {
        text.into_owned()
    };
    Ok((text, bytes.len() as u64))
}

/// Read recent JSON records in chronological order, across one or more segments.
pub fn recent_records(path: &Path, mut bytes: u64) -> Vec<serde_json::Value> {
    // Legacy logs may have no lock. Hold a shared lock when present so a
    // concurrent rotation cannot present the same segment twice.
    let _lock = match open_regular(&lock_path(path), OpenOptions::new().read(true)) {
        Ok(lock) if lock.lock_shared().is_ok() => Some(lock),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        _ => return Vec::new(),
    };
    let mut segments = Vec::new();
    for index in 0..=ARCHIVES {
        if bytes == 0 {
            break;
        }
        let path = if index == 0 {
            path.to_path_buf()
        } else {
            archive(path, index)
        };
        let (text, read) = read_tail(&path, bytes, 0).unwrap_or_default();
        bytes -= read;
        segments.push(text);
    }
    segments
        .iter()
        .rev()
        .flat_map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_concurrent_records_without_losing_or_splitting_retained_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(EVENT_FILE);
        std::thread::scope(|scope| {
            for thread in 0..4 {
                let path = &path;
                scope.spawn(move || {
                    for index in 0..32 {
                        append(path, &format!("[{},{}]\n", thread, index), false, 128).unwrap();
                    }
                });
            }
        });
        let records = recent_records(&path, 1024);
        assert!(records.len() > 32 && records.len() < 128);
        let mut seen = std::collections::HashSet::new();
        for record in records {
            assert!(seen.insert(record.to_string()));
        }
        for index in 0..=ARCHIVES {
            let segment = if index == 0 {
                path.clone()
            } else {
                archive(&path, index)
            };
            assert!(segment.metadata().unwrap().len() <= 128);
            assert!(
                fs::read_to_string(segment)
                    .unwrap()
                    .lines()
                    .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
            );
        }
    }

    #[test]
    fn raw_tails_are_bounded_and_skip_partial_lines_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.log");
        let mut capture = open_capture(&path, Capture::New).unwrap();
        capture
            .write_all(b"first long line\nsecond\nlast\n")
            .unwrap();
        assert_eq!(tail(&path, 15, 0), "second\nlast\n");
        assert_eq!(tail(&path, 12, 0), "second\nlast\n");
        assert_eq!(tail(&path, 9, 100), "last\n");
        assert_eq!(tail(&path, 64, 16), "second\nlast\n");
        assert!(open_capture(&path, Capture::New).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{PermissionsExt, symlink};
            assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            let link = dir.path().join("link.log");
            symlink(&path, &link).unwrap();
            assert!(tail(&link, 64, 0).is_empty());
            assert!(open_capture(&link, Capture::Replace).is_err());
        }
    }

    #[test]
    fn keeps_the_full_segment_and_rejects_records_larger_than_the_limit() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(EVENT_FILE);
        append(&path, "[1]\n", false, 8)?;
        append(&path, "[2]\n", false, 8)?;
        assert_eq!(fs::read_to_string(&path)?, "[1]\n[2]\n");
        assert!(!archive(&path, 1).exists());
        assert_eq!(
            append(&path, "[123456]\n", false, 8)
                .expect_err("oversized record should be rejected")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(fs::read_to_string(path)?, "[1]\n[2]\n");
        Ok(())
    }

    #[test]
    fn recent_record_budget_does_not_skip_into_an_older_segment() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(EVENT_FILE);
        append(&path, "[0]\n", false, 8)?;
        append(&path, "[10,20]\n", false, 8)?;
        assert!(recent_records(&path, 5).is_empty());
        assert_eq!(recent_records(&path, 8), vec![serde_json::json!([10, 20])]);
        Ok(())
    }

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn file_checks_precede_truncation_and_locking() -> io::Result<()> {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir()?;
        let path = dir.path().join(EVENT_FILE);
        let other = dir.path().join("other.log");
        fs::write(&other, "[1]\n")?;
        fs::hard_link(&other, &path)?;
        assert_eq!(
            open_capture(&path, Capture::Replace).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(tail(&path, 64, 0).is_empty());
        assert_eq!(fs::read_to_string(&other)?, "[1]\n");
        fs::remove_file(&path)?;
        fs::write(&path, "[2]\n")?;
        symlink(&other, lock_path(&path))?;
        assert!(append_record(&path, "[3]\n", false).is_err());
        assert!(recent_records(&path, 64).is_empty());
        assert_eq!(fs::read_to_string(path)?, "[2]\n");
        assert_eq!(fs::read_to_string(other)?, "[1]\n");
        Ok(())
    }

    #[test]
    #[cfg(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "aarch64", target_arch = "x86_64")
        ),
    ))]
    fn rejects_fifo_without_waiting_for_the_other_end() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("capture.log");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&path)
                .status()?
                .success()
        );
        let (send, receive) = std::sync::mpsc::channel();
        let read = std::thread::Builder::new().spawn(move || {
            let _ = send.send(read_tail(&path, 64, 0));
        })?;
        let result = receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("diagnostic read blocked on a FIFO");
        read.join().expect("diagnostic reader panicked");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        Ok(())
    }
}
