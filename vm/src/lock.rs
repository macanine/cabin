//! Single-writer lock so two vmctl operations cannot mutate state at once.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::Result;
use crate::util::{ensure_dir, message};

pub struct FileLock {
    path: PathBuf,
    file: Option<std::fs::File>,
    exclusive: bool,
}

impl FileLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        Self::acquire_mode(path, false)
    }

    /// Allow concurrent readers while excluding mutating operations.
    pub fn acquire_shared(path: &Path) -> Result<Self> {
        Self::acquire_mode(path, true)
    }

    fn acquire_mode(path: &Path, shared: bool) -> Result<Self> {
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let locked = if shared { file.try_lock_shared() } else { file.try_lock() }.is_ok();
        if !locked {
            let mut owner = String::new();
            let _ = file.seek(SeekFrom::Start(0));
            let _ = file.read_to_string(&mut owner);
            let owner = if owner.trim().is_empty() {
                "unknown".to_string()
            } else {
                owner.trim().to_string()
            };
            return message(format!(
                "another vmctl operation is in progress (lock owner: {owner})"
            ));
        }
        if !shared {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            writeln!(file, "pid={}", std::process::id())?;
            file.flush()?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            file: Some(file),
            exclusive: !shared,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_exclusive(&self) -> bool {
        self.exclusive
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}
