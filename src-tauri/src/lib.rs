//! Tauri shell for the Mac app: the VM (via `vm_core`) and a forwarding link
//! to `cabind` inside it (see `host/`).

mod app_error;
mod global_network;
mod host;
mod host_permissions;
mod lifecycle;
mod machine_service;
mod network_gate;
mod network_health;
mod storage;
mod sync;
mod system_route;
mod task;
mod window_chrome;

use std::path::PathBuf;

use host_permissions::{mac_folder_access, mac_open_file_settings, mac_permissions};
use lifecycle::{app_background, app_quit};
use machine_service::*;
use tauri::{Manager, RunEvent};
use window_chrome::window_chrome;

/// Every IPC command, listed once for the handler and the TS command union.
/// Guest commands (workspaces, sessions, agents, environment…) are not Tauri
/// commands: the UI sends them through `guest_invoke`.
macro_rules! with_commands {
    ($callback:ident) => {
        $callback!(
            app_subscribe,
            app_quit,
            app_background,
            guest_invoke,
            frontend_diagnostic,
            machine_checks,
            machine_diagnostics,
            host_fix,
            machine_details,
            machine_save_resources,
            storage_status,
            storage_clean,
            machine_plan,
            machine_install,
            machine_cancel_install,
            machine_test_mirrors,
            machine_verify,
            machine_resume,
            machine_start,
            machine_stop,
            machine_destroy,
            chrome_status,
            browser_environment,
            browser_settings_save,
            chrome_install,
            chrome_open,
            chrome_stop,
            chrome_paste,
            chrome_native_display,
            chrome_enable_spice,
            chrome_display,
            desktop_apps,
            desktop_launch,
            preview_forward,
            preview_close,
            read_text_file,
            write_codex_export,
            window_chrome,
            mac_permissions,
            mac_folder_access,
            mac_open_file_settings,
            sync_list,
            sync_preview,
            sync_create,
            sync_update,
            sync_remove,
            sync_now,
            sync_resolve
        )
    };
}

macro_rules! handler {
    ($($name:ident),*) => {
        tauri::generate_handler![$($name),*]
    };
}

#[cfg(test)]
macro_rules! names {
    ($($name:ident),*) => {
        &[$(stringify!($name)),*]
    };
}

/// Mutable state for the desktop app (the VM, its disk, logs).
fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CABIN_HOME") {
        return PathBuf::from(dir);
    }
    // Application Support on macOS, %APPDATA% on Windows, ~/.local/share on Linux.
    let base = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    base.join(if cfg!(target_os = "linux") { "cabin-host" } else { "cabin" })
}

/// The bundled QEMU and OpenSSH: the app's `vendor/` resource folder, or
/// `build/vendor/<platform>/` when running from the repository.
fn vendor_dir(resources: Option<&std::path::Path>) -> Option<PathBuf> {
    let platform = if cfg!(target_os = "windows") {
        "windows-x64"
    } else {
        "macos-arm64"
    };
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../build/vendor")
        .join(platform);
    resources
        .map(|dir| dir.join("vendor"))
        .into_iter()
        .chain([dev])
        .find(|dir| dir.join("manifest.json").is_file())
}

/// The native Linux `cabind` copied into the VM.
fn agentd_binary(resources: Option<PathBuf>) -> PathBuf {
    if let Some(path) = std::env::var_os("CABIND_BIN") {
        return PathBuf::from(path);
    }
    let core = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../core/target");
    let guest = vm_core::arch::GuestArch::current().rust_musl_triple();
    let mut candidates: Vec<PathBuf> = resources
        .into_iter()
        .map(|dir| dir.join("cabind"))
        .collect();
    // A static musl build runs on any Ubuntu release regardless of glibc.
    candidates.push(core.join(format!("{guest}/release/cabind")));
    candidates
        .iter()
        .find(|path| path.is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// The renderer sends a fixed event identifier, without any exception text,
/// stack, page URL, or user data. Rate-limit each identifier at the receiver.
#[tauri::command]
fn frontend_diagnostic(kind: &str) {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    static LAST: Mutex<[Option<Instant>; 4]> = Mutex::new([None; 4]);
    let (index, event) = match kind {
        "viewCrash" => (0, "view_crash"),
        "subscriberCrash" => (1, "subscriber_crash"),
        "scriptError" => (2, "script_error"),
        "unhandledRejection" => (3, "unhandled_rejection"),
        _ => return,
    };
    let Ok(mut last) = LAST.lock() else { return };
    if last[index].is_some_and(|at| at.elapsed() < Duration::from_secs(10)) {
        return;
    }
    last[index] = Some(Instant::now());
    drop(last);
    cabin_diagnostics::event(cabin_diagnostics::Level::Error, "frontend", event, format_args!(""));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let home = data_dir().join("vm");
    cabin_diagnostics::init(&vm_core::paths::StatePaths::new(&home).logs(), "desktop");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            lifecycle::show_main(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .on_window_event(lifecycle::on_window_event)
        .setup(move |app| {
            let lock = lifecycle::acquire_owner_lock(&data_dir()).map_err(std::io::Error::other)?;
            app.manage(lock);
            let resources = app.path().resource_dir().ok();
            if let Some(dir) = resources
                .as_ref()
                .map(|dir| dir.join("vm"))
                .filter(|dir| dir.join("config").is_dir())
            {
                vm_core::paths::set_project_root(dir);
            }
            if let Some(dir) = vendor_dir(resources.as_deref()) {
                vm_core::vendor::set_vendor_dir(dir);
            }
            let agentd = agentd_binary(resources);
            let machine = vm_core::machine::Machine::new(
                home,
                vm_core::paths::default_project_root(),
            )
            .with_agentd(&agentd);
            let host = host::Host::new(machine, agentd)
                .map_err(|error| std::io::Error::other(error.message))?;
            app.manage(host.clone());
            lifecycle::install_tray(app.handle())?;
            #[cfg(target_os = "macos")]
            host.install_network_power_watch();
            host.run_connection_loop()
                .map_err(|error| std::io::Error::other(error.message))?;
            Ok(())
        })
        .invoke_handler(with_commands!(handler))
        .build(tauri::generate_context!());
    let app = match app {
        Ok(app) => app,
        Err(error) => {
            cabin_diagnostics::event(cabin_diagnostics::Level::Error, "lifecycle", "startup_failed", format_args!("{error}"));
            std::process::exit(1);
        }
    };
    cabin_diagnostics::event(cabin_diagnostics::Level::Info, "lifecycle", "desktop_ready", format_args!("{}", env!("CARGO_PKG_VERSION")));
    app.run(|handle, event| match event {
        // ⌘Q and the Dock's Quit ask first; only the dialog's answer leaves.
        RunEvent::ExitRequested { api, code, .. } if code.is_none() && !lifecycle::is_quitting() => {
            api.prevent_exit();
            lifecycle::request_quit(handle);
        }
        // A Dock click brings the hidden window back.
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => lifecycle::show_main(handle),
        RunEvent::Exit => {
            if let Some(host) = handle.try_state::<task::Shared>() {
                host.shutdown();
            }
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use crate::app_error::AppError;
    use crate::task::TaskEvent;
    use ts_rs::{Config, TS};

    /// One release number for the app and the guest: the Mac shell, the
    /// window (`package.json`), the bundle (`tauri.conf.json`) and the
    /// cabind it ships. The exact guest build is still checked at runtime
    /// by the hello fingerprint; this keeps the numbers people see honest.
    #[test]
    fn app_and_guest_versions_agree() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let read = |path: &str| -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            Ok(serde_json::from_str(&std::fs::read_to_string(root.join(path))?)?)
        };
        let app = env!("CARGO_PKG_VERSION");
        assert_eq!(agent_core::hello::VERSION, app, "core/Cargo.toml");
        assert_eq!(read("package.json")?["version"], app, "package.json");
        assert_eq!(read("src-tauri/tauri.conf.json")?["version"], app, "tauri.conf.json");
        Ok(())
    }

    /// Regenerate with `CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings`.
    #[test]
    fn ipc_bindings_are_current() -> Result<(), Box<dyn std::error::Error>> {
        let config = Config::default();
        let commands: &[&str] = with_commands!(names);
        let mut generated = String::from(
            "// Generated from src-tauri; regenerate with\n// CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings (in src-tauri/).\n\n",
        );
        generated.push_str(&format!("export {}\n", AppError::decl(&config)));
        generated.push_str(&format!("export {}\n", TaskEvent::<()>::decl(&config)));
        generated.push_str(&format!(
            "export {}\n",
            crate::window_chrome::WindowChrome::decl(&config)
        ));
        use vm_core::machine_details::{DeviceDetails, DeviceFinding, DeviceReportState, MachineDetails};
        for declaration in [
            DeviceReportState::decl(&config),
            DeviceFinding::decl(&config),
            DeviceDetails::decl(&config),
            vm_core::resources::ResourceRequest::decl(&config),
            MachineDetails::decl(&config),
            vm_core::readiness::Fix::decl(&config),
            vm_core::readiness::Readiness::decl(&config),
            vm_core::readiness::HostFacts::decl(&config),
            vm_core::readiness::ReadinessReport::decl(&config),
            vm_core::readiness::FixOutcome::decl(&config),
            vm_core::storage::StorageKind::decl(&config),
            vm_core::storage::StorageEntry::decl(&config),
            vm_core::storage::StorageVolume::decl(&config),
            vm_core::storage::CleanupTarget::decl(&config),
            vm_core::storage::CleanupResult::decl(&config),
            crate::storage::StorageReport::decl(&config),
            crate::host_permissions::FolderAccessState::decl(&config),
            crate::host_permissions::FolderAccess::decl(&config),
            crate::host_permissions::MacProjectAccess::decl(&config),
            crate::system_route::SystemRoute::decl(&config),
        ] {
            generated.push_str(&format!("export {declaration}\n"));
        }
        generated.push_str("export type IpcCommand =\n");
        for (index, name) in commands.iter().enumerate() {
            let end = if index + 1 == commands.len() { ";" } else { "" };
            generated.push_str(&format!("  | \"{name}\"{end}\n"));
        }
        let path = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../src/services/ipc/generated/common.ts"
        ));
        if std::env::var("CABIN_UPDATE_IPC_BINDINGS").as_deref() == Ok("1") {
            std::fs::write(path, &generated)?;
        }
        // Checkouts may carry CRLF line endings; compare content only.
        assert_eq!(
            std::fs::read_to_string(path)
                .unwrap_or_default()
                .replace('\r', ""),
            generated,
            "IPC bindings are stale; run CABIN_UPDATE_IPC_BINDINGS=1 cargo test ipc_bindings"
        );
        Ok(())
    }
}
