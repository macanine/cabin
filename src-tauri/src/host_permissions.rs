//! Effective access to Mac folders selected for project sync.

use std::io;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
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
            cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EPERM);
        let (state, message) = match error.kind() {
            io::ErrorKind::NotFound => (
                FolderAccessState::Missing,
                "The folder could not be found on this Mac.".into(),
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

#[cfg(unix)]
fn access(path: &std::ffi::CString, mode: libc::c_int) -> io::Result<()> {
    // SAFETY: the path is NUL-terminated and lives through the call.
    if unsafe { libc::access(path.as_ptr(), mode) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// A per-path preflight using POSIX access, which does not trigger consent.
/// The real scan or write must still handle errors after this check.
/// Windows has no access(): reads are checked through the metadata and writes
/// through a small probe file inside the folder.
pub(crate) fn check_folder(path: &Path, write: bool) -> FolderAccess {
    #[cfg(not(unix))]
    if !path.as_os_str().is_empty() && path.as_os_str().to_string_lossy().contains('\0') {
        return FolderAccess::failed(
            io::Error::new(io::ErrorKind::InvalidInput, "Invalid folder path"),
            false,
        );
    }
    #[cfg(unix)]
    if let Err(error) = (|| {
        let native = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid folder path"))?;
        access(&native, libc::R_OK | libc::X_OK)
    })() {
        return FolderAccess::failed(error, false);
    }
    match path.metadata() {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return FolderAccess::failed(
                io::Error::new(io::ErrorKind::NotADirectory, "Choose a folder on this Mac."),
                false,
            );
        }
        Err(error) => return FolderAccess::failed(error, false),
    }
    if write {
        #[cfg(unix)]
        if let Err(error) = (|| {
            let native = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Invalid folder path"))?;
            access(&native, libc::W_OK)
        })() {
            return FolderAccess::failed(error, true);
        }
        #[cfg(not(unix))]
        {
            let probe = path.join(".cabin-access-probe");
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&probe)
            {
                Ok(_) => {
                    let _ = std::fs::remove_file(&probe);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return FolderAccess::failed(error, true),
            }
        }
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
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn checks_the_selected_folder_and_mode_without_creating_any_files() {
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

    #[test]
    fn privacy_and_filesystem_denials_have_distinct_guidance() {
        #[cfg(unix)]
        let privacy = FolderAccess::failed(io::Error::from_raw_os_error(libc::EPERM), false);
        #[cfg(not(unix))]
        let privacy = FolderAccess::failed(io::Error::other("consent prompt"), false);
        #[cfg(unix)]
        let filesystem = FolderAccess::failed(io::Error::from_raw_os_error(libc::EACCES), false);
        #[cfg(not(unix))]
        let filesystem = FolderAccess::failed(
            io::Error::from(io::ErrorKind::PermissionDenied),
            false,
        );
        assert_eq!(privacy.privacy_settings, cfg!(target_os = "macos"));
        assert!(!filesystem.privacy_settings);
        assert_eq!(filesystem.state, FolderAccessState::ReadDenied);
    }
}
