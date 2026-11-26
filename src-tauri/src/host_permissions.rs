//! Effective access to host folders selected for project sync.

use std::io;
use std::path::Path;

use agent_core::sync::SyncMode;
use serde::Serialize;
use tauri::State;

use crate::app_error::AppError;
use crate::task::{Shared, blocking};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub(crate) enum FolderAccessState {
    Available,
    ReadDenied,
    WriteDenied,
    Missing,
    Unavailable,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub(crate) struct FolderAccess {
    pub state: FolderAccessState,
    pub message: Option<String>,
    pub privacy_settings: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub(crate) struct MacProjectAccess {
    pub id: String,
    pub name: String,
    pub path: String,
    pub needs_write: bool,
    pub access: FolderAccess,
}

impl FolderAccess {
    fn failed(error: io::Error, write: bool) -> Self {
        let privacy_settings =
            cfg!(target_os = "macos") && error.raw_os_error() == Some(1 /* EPERM */);
        let (state, message) = match error.kind() {
            io::ErrorKind::NotFound => (
                FolderAccessState::Missing,
                "The folder could not be found on this computer.".into(),
            ),
            io::ErrorKind::NotADirectory => (
                FolderAccessState::Unavailable,
                "Choose a folder on this computer.".into(),
            ),
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem if write => (
                FolderAccessState::WriteDenied,
                "Two-way sync needs permission to write to this folder.".into(),
            ),
            io::ErrorKind::PermissionDenied => (
                FolderAccessState::ReadDenied,
                "Cabin needs permission to read this folder.".into(),
            ),
            _ => (FolderAccessState::Unavailable, error.to_string()),
        };
        Self {
            state,
            message: Some(message),
            privacy_settings,
        }
    }

    pub fn require(self, path: &Path) -> Result<(), AppError> {
        let Some(message) = self.message else {
            return Ok(());
        };
        let code = if self.privacy_settings {
            "mac_folder_privacy"
        } else {
            "mac_folder_access"
        };
        Err(AppError {
            code: code.into(),
            message,
            details: Some(path.display().to_string()),
        })
    }
}

fn require_directory(path: &Path) -> io::Result<()> {
    if path.metadata()?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::from(io::ErrorKind::NotADirectory))
    }
}

/// POSIX `access` checks permissions before inspecting the folder, which could
/// bring up the macOS consent dialog.
#[cfg(unix)]
mod probe {
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    fn access(path: &Path, mode: libc::c_int) -> io::Result<()> {
        let native = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid folder path"))?;
        // SAFETY: the path is NUL-terminated and lives through the call.
        if unsafe { libc::access(native.as_ptr(), mode) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub fn read(path: &Path) -> io::Result<()> {
        access(path, libc::R_OK | libc::X_OK)?;
        super::require_directory(path)
    }

    pub fn write(path: &Path) -> io::Result<()> {
        access(path, libc::W_OK)
    }
}

#[cfg(not(unix))]
mod probe {
    use std::io;
    use std::path::Path;

    pub fn read(path: &Path) -> io::Result<()> {
        super::require_directory(path)?;
        std::fs::read_dir(path)?.next().transpose().map(|_| ())
    }

    pub fn write(path: &Path) -> io::Result<()> {
        tempfile::Builder::new()
            .prefix(agent_core::sync::ACCESS_PROBE_PREFIX)
            .suffix(".tmp")
            .tempfile_in(path)?
            .close()
    }
}

/// A per-path preflight. The real scan or write must still handle errors
/// after this check.
pub(crate) fn check_folder(path: &Path, write: bool) -> FolderAccess {
    if let Err(error) = probe::read(path) {
        return FolderAccess::failed(error, false);
    }
    if write && let Err(error) = probe::write(path) {
        return FolderAccess::failed(error, true);
    }
    FolderAccess {
        state: FolderAccessState::Available,
        message: None,
        privacy_settings: false,
    }
}

#[tauri::command]
pub(crate) async fn mac_permissions(
    host: State<'_, Shared>,
) -> Result<Vec<MacProjectAccess>, AppError> {
    blocking(&host, |host| {
        Ok(host
            .sync
            .list()
            .into_iter()
            .map(|view| {
                let project = view.project;
                let needs_write = project.mode == SyncMode::TwoWay;
                let access = check_folder(Path::new(&project.host_path), needs_write);
                MacProjectAccess {
                    id: project.id,
                    name: project.name,
                    path: project.host_path,
                    needs_write,
                    access,
                }
            })
            .collect())
    })
    .await
}

/// A real read may bring up the macOS consent dialog. Only do this from the
/// explicit request action, after a fresh check says system privacy may apply.
#[tauri::command]
pub(crate) async fn mac_folder_access(
    host: State<'_, Shared>,
    path: String,
    needs_write: bool,
    request: bool,
) -> Result<FolderAccess, AppError> {
    blocking(&host, move |_| {
        let path = agent_core::sync::expand_home(&path);
        let before = check_folder(&path, needs_write);
        if request && before.privacy_settings {
            if let Err(error) = std::fs::read_dir(&path) {
                return Ok(FolderAccess::failed(error, false));
            }
            return Ok(check_folder(&path, needs_write));
        }
        Ok(before)
    })
    .await
}

#[tauri::command]
pub(crate) async fn mac_open_file_settings() -> Result<(), AppError> {
    #[cfg(target_os = "macos")]
    {
        const FILES_AND_FOLDERS: &str =
            "x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders";
        let status = tauri::async_runtime::spawn_blocking(|| {
            std::process::Command::new("/usr/bin/open")
                .arg(FILES_AND_FOLDERS)
                .status()
        })
        .await
        .map_err(|error| AppError::internal(error.to_string()))?
        .map_err(|error| AppError::internal(error.to_string()))?;
        if status.success() {
            return Ok(());
        }
    }
    Err(AppError::internal(
        "Open System Settings → Privacy & Security → Files and Folders.",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn checks_the_selected_folder_and_mode_without_creating_any_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        assert_eq!(check_folder(path, true).state, FolderAccessState::Available);
        let missing = path.join("missing");
        assert_eq!(
            check_folder(&missing, false).state,
            FolderAccessState::Missing
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(
            check_folder(path, false).state,
            FolderAccessState::Available
        );
        // The superuser can write to a 0500 directory.
        if unsafe { libc::getuid() } != 0 {
            assert_eq!(
                check_folder(path, true).state,
                FolderAccessState::WriteDenied
            );
        }
        assert_eq!(path.read_dir().unwrap().count(), 0);
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn checks_write_access_with_an_independent_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join(".cabin-access-probe");
        std::fs::write(&existing, b"user file").unwrap();

        assert_eq!(
            check_folder(dir.path(), true).state,
            FolderAccessState::Available
        );
        assert_eq!(std::fs::read(&existing).unwrap(), b"user file");
        assert_eq!(dir.path().read_dir().unwrap().count(), 1);
        assert_eq!(
            check_folder(&existing, false).state,
            FolderAccessState::Unavailable
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_write_probe_does_not_schedule_a_sync() {
        use agent_core::sync::{SyncRules, watch};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let watched = watch::watch(dir.path(), &SyncRules::default()).unwrap();
        assert_eq!(
            check_folder(dir.path(), true).state,
            FolderAccessState::Available
        );
        assert_eq!(
            watched.batches.recv_timeout(Duration::from_millis(750)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );
        std::fs::write(dir.path().join("real-change.tmp"), b"user edit").unwrap();
        assert_eq!(
            watched
                .batches
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            [""]
        );
    }

    #[test]
    fn privacy_and_filesystem_denials_have_distinct_guidance() {
        let privacy = FolderAccess::failed(io::Error::from_raw_os_error(1 /* EPERM */), false);
        let filesystem =
            FolderAccess::failed(io::Error::from(io::ErrorKind::PermissionDenied), false);
        assert_eq!(privacy.privacy_settings, cfg!(target_os = "macos"));
        assert!(!filesystem.privacy_settings);
        assert_eq!(filesystem.state, FolderAccessState::ReadDenied);
    }
}
