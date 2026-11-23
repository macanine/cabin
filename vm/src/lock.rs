//! Single-writer lock so two vmctl operations cannot mutate state at once.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::Result;
use crate::util::{ensure_dir, message};

pub struct FileLock {
    path: PathBuf,
    file: File,
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
        match if shared {
            file.try_lock_shared()
        } else {
            file.try_lock()
        } {
            Ok(()) => {}
            Err(TryLockError::Error(error)) => return Err(error.into()),
            Err(TryLockError::WouldBlock) => {
                let mut owner = String::new();
                let _ = file.read_to_string(&mut owner);
                return message(format!(
                    "another vmctl operation is in progress (last writer: {})",
                    if owner.trim().is_empty() {
                        "unknown"
                    } else {
                        owner.trim()
                    }
                ));
            }
        }
        if !shared {
            file.set_len(0)?;
            file.seek(SeekFrom::Start(0))?;
            writeln!(file, "pid={}", std::process::id())?;
            file.flush()?;
        }
        Ok(Self {
            path: path.to_path_buf(),
            file,
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
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_crash_helper() {
        let Some(path) = std::env::var_os("CABIN_R2_TEST_CRASH_LOCK") else {
            return;
        };
        let _lock = FileLock::acquire(Path::new(&path)).unwrap();
        // Skip destructors to model a crash. The operating system releases
        // the advisory lock while the old PID remains in the file.
        std::process::exit(0);
    }

    #[test]
    fn crash_releases_lock_even_though_the_owner_record_remains() {
        let dir = crate::sockets::test_dir();
        let path = dir.join("lock");
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "lock::tests::lock_crash_helper"])
            .env("CABIN_R2_TEST_CRASH_LOCK", &path)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("pid="));
        let lock = FileLock::acquire(&path).unwrap();
        assert!(FileLock::acquire(&path).is_err());
        drop(lock);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
