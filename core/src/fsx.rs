//! Crash-safe file writes and backups.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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
        .or_else(|| {
            fs::metadata(path)
                .ok()
                .map(|meta| meta.permissions().mode() & 0o7777)
        })
        .unwrap_or(0o644);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let temp = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&temp)
            .map_err(|error| Error::path("create", &temp, error))?;
        file.write_all(bytes)
            .map_err(|error| Error::path("write", &temp, error))?;
        file.sync_all()
            .map_err(|error| Error::path("sync", &temp, error))?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(mode))
            .map_err(|error| Error::path("chmod", &temp, error))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_and_keeps_mode() -> Result<()> {
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
