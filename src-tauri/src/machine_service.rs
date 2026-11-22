//! Window subscription, guest forwarding and VM lifecycle commands.

use agent_core::model::{
    AppEvent, BrowserEnvironmentView, BrowserSettings, HostCheck, HostSnapshot, InstallPlan, InstallStep, MachineInstallOptions,
    MachineStatus, MirrorResult, SyncPreview, SyncProjectInput, SyncView,
};
use agent_core::sync::{SyncMode, SyncRules};
use serde_json::Value;
use tauri::{AppHandle, State};
use tauri::ipc::Channel;

use crate::app_error::AppError;
use crate::host_permissions::check_folder;
use crate::task::{Shared, TaskEvent, blocking, spawn_task};

/// Subscribe the window to machine + guest events; returns the current state.
#[tauri::command]
pub(crate) fn app_subscribe(host: State<'_, Shared>, channel: Channel<AppEvent>) -> HostSnapshot {
    host.set_sink(move |event| {
        let _ = channel.send(event);
    });
    host.snapshot()
}

/// Forward a workspace/session/agent/environment command to cabind.
#[tauri::command]
pub(crate) async fn guest_invoke(
    host: State<'_, Shared>,
    method: String,
    params: Value,
) -> Result<Value, AppError> {
    blocking(&host, move |host| host.call(&method, params)).await
}

#[tauri::command]
pub(crate) async fn machine_checks(host: State<'_, Shared>) -> Result<Vec<HostCheck>, AppError> {
    blocking(&host, |host| Ok(host.checks())).await
}

#[tauri::command]
pub(crate) async fn machine_details(
    host: State<'_, Shared>,
) -> Result<Option<vm_core::machine_details::MachineDetails>, AppError> {
    blocking(&host, |host| host.machine_details()).await
}

#[tauri::command]
pub(crate) async fn storage_status(
    app: AppHandle,
    host: State<'_, Shared>,
) -> Result<crate::storage::StorageReport, AppError> {
    let identifier = app.config().identifier.clone();
    blocking(&host, move |host| Ok(host.storage(&identifier))).await
}

#[tauri::command]
pub(crate) async fn storage_clean(
    host: State<'_, Shared>,
    target: vm_core::storage::CleanupTarget,
) -> Result<vm_core::storage::CleanupResult, AppError> {
    blocking(&host, move |host| host.clean_storage(target)).await
}

#[tauri::command]
pub(crate) async fn machine_plan(
    host: State<'_, Shared>,
    options: MachineInstallOptions,
) -> Result<InstallPlan, AppError> {
    blocking(&host, move |host| host.plan(options)).await
}

#[tauri::command]
pub(crate) fn machine_install(
    host: State<'_, Shared>,
    channel: Channel<TaskEvent<MachineStatus>>,
    options: MachineInstallOptions,
) {
    let shared = host.inner().clone();
    spawn_task(&host, channel, move |_| shared.install(options));
}

#[tauri::command]
pub(crate) fn machine_start(host: State<'_, Shared>, channel: Channel<TaskEvent<MachineStatus>>) {
    let shared = host.inner().clone();
    spawn_task(&host, channel, move |_| shared.start());
}

#[tauri::command]
pub(crate) fn machine_stop(host: State<'_, Shared>, channel: Channel<TaskEvent<MachineStatus>>) {
    spawn_task(&host, channel, |host| host.stop());
}

#[tauri::command]
pub(crate) fn machine_destroy(host: State<'_, Shared>, channel: Channel<TaskEvent<MachineStatus>>) {
    spawn_task(&host, channel, |host| host.destroy());
}

#[tauri::command]
pub(crate) async fn chrome_status(host: State<'_, Shared>) -> Result<Value, AppError> {
    blocking(&host, |host| host.chrome_status()).await
}

#[tauri::command]
pub(crate) async fn browser_environment(
    host: State<'_, Shared>,
    check: bool,
) -> Result<BrowserEnvironmentView, AppError> {
    blocking(&host, move |host| host.browser_environment_view(check)).await
}

#[tauri::command]
pub(crate) async fn browser_settings_save(
    host: State<'_, Shared>,
    settings: BrowserSettings,
) -> Result<BrowserEnvironmentView, AppError> {
    blocking(&host, move |host| host.browser_settings_save(settings)).await
}

#[tauri::command]
pub(crate) fn chrome_install(host: State<'_, Shared>, channel: Channel<TaskEvent<()>>) {
    spawn_task(&host, channel, |host| host.install_chrome());
}

#[tauri::command]
pub(crate) async fn desktop_apps(host: State<'_, Shared>) -> Result<Value, AppError> {
    blocking(&host, |host| host.desktop_apps()).await
}

#[tauri::command]
pub(crate) async fn desktop_launch(host: State<'_, Shared>, id: String) -> Result<(), AppError> {
    blocking(&host, move |host| host.desktop_launch(&id)).await
}

#[tauri::command]
pub(crate) async fn chrome_open(host: State<'_, Shared>, url: String) -> Result<(), AppError> {
    blocking(&host, move |host| host.chrome_open(&url)).await
}

#[tauri::command]
pub(crate) async fn chrome_stop(host: State<'_, Shared>) -> Result<(), AppError> {
    blocking(&host, |host| host.chrome_stop()).await
}

#[tauri::command]
pub(crate) async fn chrome_paste(host: State<'_, Shared>, text: String) -> Result<(), AppError> {
    blocking(&host, move |host| host.chrome_paste(&text)).await
}

#[tauri::command]
pub(crate) async fn chrome_native_display(
    host: State<'_, Shared>,
    request: Value,
) -> Result<Value, AppError> {
    blocking(&host, move |host| host.chrome_native_display(request)).await
}

#[tauri::command]
pub(crate) fn chrome_enable_spice(host: State<'_, Shared>, channel: Channel<TaskEvent<()>>) {
    spawn_task(&host, channel, |host| host.chrome_enable_spice());
}

#[tauri::command]
pub(crate) async fn chrome_display(
    host: State<'_, Shared>,
    protocol: Option<String>,
    parent_origin: Option<String>,
) -> Result<Value, AppError> {
    blocking(&host, move |host| {
        host.chrome_display(protocol.as_deref(), parent_origin.as_deref())
    })
    .await
}

#[tauri::command]
pub(crate) async fn preview_forward(host: State<'_, Shared>, port: u16) -> Result<Value, AppError> {
    blocking(&host, move |host| host.preview_forward(port)).await
}

#[tauri::command]
pub(crate) async fn preview_close(host: State<'_, Shared>, port: u16) -> Result<(), AppError> {
    blocking(&host, move |host| host.preview_close(port)).await
}

/// Read a small text file the user picked with the Mac file dialog.
#[tauri::command]
pub(crate) async fn read_text_file(path: String) -> Result<String, AppError> {
    let meta = std::fs::metadata(&path).map_err(|error| AppError {
        code: "io".into(),
        message: format!("Could not read {path}: {error}"),
        details: None,
    })?;
    if !meta.is_file() || meta.len() > 2 * 1024 * 1024 {
        return Err(AppError {
            code: "invalid".into(),
            message: "Choose a text file smaller than 2 MB".into(),
            details: None,
        });
    }
    std::fs::read_to_string(&path).map_err(|error| AppError {
        code: "io".into(),
        message: format!("Could not read {path}: {error}"),
        details: None,
    })
}

/// Write a credential export without replacing another file. The folder comes
/// from the native picker; the file is private even when the folder is shared.
#[tauri::command]
pub(crate) async fn write_codex_export(
    directory: String,
    contents: String,
) -> Result<String, AppError> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    if contents.len() > 2 * 1024 * 1024 {
        return Err(AppError {
            code: "invalid".into(),
            message: "The credential export is too large".into(),
            details: None,
        });
    }
    for number in 1..=1000 {
        let name = if number == 1 {
            "codex-account.json".to_string()
        } else {
            format!("codex-account-{number}.json")
        };
        let path = std::path::Path::new(&directory).join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(&path)
        {
            Ok(mut file) => {
                if let Err(error) = file
                    .write_all(contents.as_bytes())
                    .and_then(|()| file.sync_all())
                {
                    drop(file);
                    let _ = std::fs::remove_file(&path);
                    return Err(AppError {
                        code: "io".into(),
                        message: format!("Could not write credential export: {error}"),
                        details: None,
                    });
                }
                return Ok(path.to_string_lossy().into_owned());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(AppError {
                    code: "io".into(),
                    message: format!("Could not create credential export: {error}"),
                    details: None,
                });
            }
        }
    }
    Err(AppError {
        code: "conflict".into(),
        message: "Too many Codex exports in this folder".into(),
        details: None,
    })
}

#[cfg(test)]
mod export_tests {
    use super::write_codex_export;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[cfg(unix)]
    #[test]
    fn exports_are_private_and_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().to_string_lossy().into_owned();
        let first =
            tauri::async_runtime::block_on(write_codex_export(folder.clone(), "first".into()))
                .unwrap();
        let second =
            tauri::async_runtime::block_on(write_codex_export(folder, "second".into())).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "first");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "second");
        assert_eq!(
            std::fs::metadata(first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(second).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tauri::command]
pub(crate) async fn machine_cancel_install(host: State<'_, Shared>) -> Result<(), AppError> {
    blocking(&host, |host| host.cancel_install()).await
}

#[tauri::command]
pub(crate) async fn machine_test_mirrors(
    host: State<'_, Shared>,
    os: Option<String>,
    extra: Vec<String>,
) -> Result<Vec<MirrorResult>, AppError> {
    blocking(&host, move |host| host.test_mirrors(os, extra)).await
}

#[tauri::command]
pub(crate) async fn machine_verify(host: State<'_, Shared>) -> Result<Vec<InstallStep>, AppError> {
    blocking(&host, |host| host.verify()).await
}

#[tauri::command]
pub(crate) fn machine_resume(
    host: State<'_, Shared>,
    channel: Channel<TaskEvent<MachineStatus>>,
    change_mirror: bool,
    apt_mirror: Option<String>,
) {
    let shared = host.inner().clone();
    spawn_task(&host, channel, move |_| {
        shared.resume(change_mirror, apt_mirror)
    });
}

// ------------------------------------------------------------ project sync

fn sync_error(message: String) -> AppError {
    AppError {
        code: "sync".into(),
        message,
        details: None,
    }
}

#[tauri::command]
pub(crate) async fn sync_list(host: State<'_, Shared>) -> Result<Vec<SyncView>, AppError> {
    blocking(&host, |host| Ok(host.sync.list())).await
}

/// What a Mac folder would sync under `rules` (top level, with counts).
#[tauri::command]
pub(crate) async fn sync_preview(
    host: State<'_, Shared>,
    host_path: String,
    rules: SyncRules,
    mode: Option<SyncMode>,
) -> Result<SyncPreview, AppError> {
    blocking(&host, move |_| {
        let folder = agent_core::sync::expand_home(&host_path);
        check_folder(&folder, mode == Some(SyncMode::TwoWay)).require(&folder)?;
        agent_core::sync::preview(&folder, &rules).map_err(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied {
                return AppError {
                    code: "mac_folder_access".into(),
                    message: format!("Could not read the project files: {error}"),
                    details: Some(host_path),
                };
            }
            sync_error(error.to_string())
        })
    })
    .await
}

#[tauri::command]
pub(crate) async fn sync_create(
    host: State<'_, Shared>,
    input: SyncProjectInput,
) -> Result<SyncView, AppError> {
    blocking(&host, move |host| {
        let folder = agent_core::sync::expand_home(input.host_path.trim());
        check_folder(&folder, input.mode == SyncMode::TwoWay).require(&folder)?;
        host.sync.create(input).map_err(sync_error)
    })
    .await
}

#[tauri::command]
pub(crate) async fn sync_update(
    host: State<'_, Shared>,
    id: String,
    mode: Option<SyncMode>,
    rules: Option<SyncRules>,
    paused: Option<bool>,
) -> Result<SyncView, AppError> {
    blocking(&host, move |host| {
        if mode.is_some() || paused == Some(false) {
            let project = host.sync.project(&id).map_err(sync_error)?;
            let folder = std::path::Path::new(&project.host_path);
            check_folder(folder, mode.unwrap_or(project.mode) == SyncMode::TwoWay).require(folder)?;
        }
        host.sync.update(&id, mode, rules, paused).map_err(sync_error)
    })
    .await
}

/// Stop syncing a project; files stay on both sides.
#[tauri::command]
pub(crate) async fn sync_remove(host: State<'_, Shared>, id: String) -> Result<(), AppError> {
    blocking(&host, move |host| host.sync.remove(&id).map_err(sync_error)).await
}

#[tauri::command]
pub(crate) async fn sync_now(host: State<'_, Shared>, id: String) -> Result<(), AppError> {
    blocking(&host, move |host| {
        host.sync.sync_now(&id).map_err(sync_error)
    })
    .await
}

/// Settle a conflict: `keep` is `mac` or `machine`.
#[tauri::command]
pub(crate) async fn sync_resolve(
    host: State<'_, Shared>,
    id: String,
    path: String,
    keep: String,
) -> Result<(), AppError> {
    blocking(&host, move |host| {
        host.sync
            .resolve(&id, &path, keep == "mac")
            .map_err(sync_error)
    })
    .await
}
