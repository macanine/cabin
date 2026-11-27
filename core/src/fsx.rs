//! Crash-safe file writes and backups.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub fn ensure_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| Error::path("create directory", path, error))
}

fn sync_dir(path: &Path) {
    if let Ok(dir) = File::open(path) {
        let _ = dir.sync_all();
    }
}

/// Write `bytes` to `path` atomically: temp file in the same directory,
/// `fsync`, rename, then `fsync` the directory. Existing permissions are kept
/// unless `mode` is given.
pub fn atomic_write(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| Error::invalid(format!("{} has no parent directory", path.display())))?;
    ensure_dir(parent)?;
    let mode = mode
        .or_else(|| fs::metadata(path).ok().map(|meta| mode_of(&meta)))
        .unwrap_or(0o644);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let temp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = options
            .open(&temp)
            .map_err(|error| Error::path("create", &temp, error))?;
        set_mode(&temp, mode).map_err(|error| Error::path("chmod", &temp, error))?;
        file.write_all(bytes)
            .map_err(|error| Error::path("write", &temp, error))?;
        file.sync_all()
            .map_err(|error| Error::path("sync", &temp, error))?;
        drop(file);
        fs::rename(&temp, path).map_err(|error| Error::path("replace", path, error))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    } else {
        sync_dir(parent);
    }
    result
}

pub fn atomic_write_json(path: &Path, value: &serde_json::Value, mode: Option<u32>) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    atomic_write(path, text.as_bytes(), mode)
}

/// Copy `path` into `dir` with a timestamped name. Returns `None` when the
/// source does not exist (there is nothing to back up).
pub fn backup(path: &Path, dir: &Path) -> Result<Option<PathBuf>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(Error::path("read", path, error)),
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let stamp = crate::util::now_millis();
    let target = dir.join(format!("{stamp}-{name}"));
    atomic_write(&target, &bytes, Some(0o600))?;
    Ok(Some(target))
}

/// Restore `path` from `backup`, or remove it when `backup` is `None`
/// (the file did not exist before).
pub fn restore(path: &Path, backup: Option<&Path>) -> Result<()> {
    match backup {
        Some(source) => {
            let bytes = fs::read(source).map_err(|error| Error::path("read", source, error))?;
            atomic_write(path, &bytes, None)
        }
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::path("remove", path, error)),
        },
    }
}

// ---- the few places Unix and Windows files differ ---------------------------

/// Unix mode bits of `meta`. Windows derives them from the attributes; the
/// guest applies them as-is, so Windows-hosted syncs send plain read-only or
/// normal files.
pub fn mode_of(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        if meta.is_dir() {
            0o755
        } else if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

/// Apply Unix mode bits. Windows stores none, so this does nothing there.
pub fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
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

/// Stage a symbolic link, using its final location to identify relative targets.
pub fn symlink(target: &Path, link: &Path, installed_at: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let _ = installed_at;
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let resolved = installed_at
            .parent()
            .map(|parent| parent.join(target))
            .unwrap_or_else(|| target.to_path_buf());
        if resolved.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
}

/// Modified time since the epoch (zero when unknown or earlier).
fn mtime_of(meta: &fs::Metadata) -> std::time::Duration {
    meta.modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .unwrap_or_default()
}

/// Modified time in Unix seconds (floored).
pub fn mtime_secs_of(meta: &fs::Metadata) -> u64 {
    mtime_of(meta).as_secs()
}

/// Modified time in nanoseconds since the epoch.
pub fn mtime_nanos_of(meta: &fs::Metadata) -> u64 {
    u64::try_from(mtime_of(meta).as_nanos()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn atomic_write_replaces_and_keeps_mode() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("nested/config.json");
        atomic_write(&path, b"one", Some(0o600))?;
        atomic_write(&path, b"two", None)?;
        assert_eq!(fs::read_to_string(&path)?, "two");
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        let leftovers: Vec<_> = fs::read_dir(path.parent().unwrap())?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        Ok(())
    }

    #[test]
    fn backup_and_restore_round_trip() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.json");
        let backups = dir.path().join("backups");
        assert_eq!(backup(&path, &backups)?, None);
        fs::write(&path, "original")?;
        let saved = backup(&path, &backups)?.expect("backup written");
        fs::write(&path, "changed")?;
        restore(&path, Some(&saved))?;
        assert_eq!(fs::read_to_string(&path)?, "original");
        restore(&path, None)?;
        assert!(!path.exists());
        Ok(())
    }
}
