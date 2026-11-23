//! Shared local logging policy for Cabin's host, tools, and guest.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

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

fn options(path: &Path) -> io::Result<OpenOptions> {
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
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(io::Error::other("expected a regular diagnostic file"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let options = OpenOptions::new();
    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = options;
        options.mode(0o600);
        options
    };
    Ok(options)
}

/// Open a scoped raw capture. The same file can be inherited by a subprocess.
pub fn open_capture(path: &Path, mode: Capture) -> io::Result<File> {
    let mut options = options(path)?;
    match mode {
        Capture::Append => options.create(true).append(true),
        Capture::Replace => options.create(true).write(true).truncate(true),
        Capture::New => options.create_new(true).append(true),
    };
    options.open(path)
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
    let lock_path = lock_path(path);
    let lock = options(&lock_path)?
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)?;
    lock.lock()?;
    let open = || {
        options(path)?
            .create(true)
            .read(true)
            .append(true)
            .open(path)
    };
    let mut file = open()?;
    let length = file.metadata()?.len();
    if length > 0 && length.saturating_add(line.len() as u64 + 1) > limit {
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
    } else if length > 0 {
        // Isolate a partial record left by an interrupted write.
        file.seek(SeekFrom::End(-1))?;
        let mut byte = [0];
        file.read_exact(&mut byte)?;
        if byte[0] != b'\n' {
            file.write_all(b"\n")?;
        }
    }
    file.write_all(line.as_bytes())?;
    if durable {
        file.sync_all()?;
    }
    Ok(())
}

/// Read up to `max_bytes` from the end of a regular capture. `offset` excludes
/// earlier output from a live stream that is shared by successive attempts.
pub fn tail(path: &Path, max_bytes: u64, offset: u64) -> String {
    let read = || -> io::Result<String> {
        if !fs::symlink_metadata(path)?.is_file() {
            return Ok(String::new());
        }
        let mut file = File::open(path)?;
        let length = file.metadata()?.len();
        let start = length
            .saturating_sub(max_bytes)
            .max(if length >= offset { offset } else { 0 });
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(max_bytes).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        if start > offset {
            Ok(text
                .split_once('\n')
                .map_or("", |(_, rest)| rest)
                .to_owned())
        } else {
            Ok(text.into_owned())
        }
    };
    read().unwrap_or_default()
}

/// Read recent JSON records in chronological order, across one or more segments.
pub fn recent_records(path: &Path, mut bytes: u64) -> Vec<serde_json::Value> {
    // Legacy logs may have no lock. Hold a shared lock when present so a
    // concurrent rotation cannot present the same segment twice.
    let lock_path = lock_path(path);
    let _lock = fs::symlink_metadata(&lock_path)
        .ok()
        .filter(|meta| meta.is_file())
        .and_then(|_| File::open(lock_path).ok())
        .and_then(|lock| lock.lock_shared().ok().map(|()| lock));
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
        let text = tail(&path, bytes, 0);
        bytes = bytes.saturating_sub(text.len() as u64);
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
}
