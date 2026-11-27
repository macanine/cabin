//! One app, one machine: how the app opens, hides to the tray and quits.
//!
//! - The red close button hides the window; the app and the VM keep running
//!   (the tray icon is the way back).
//! - Quit (⌘Q, the tray's Quit, or the Dock menu) never ends the VM silently:
//!   the window comes forward and asks (`lifecycle://quit-requested`): quit
//!   fully (stop the machine), keep running in the background, or cancel.
//! - A lock file in the data folder makes a second process (another build, a
//!   copy with the same home) refuse to start instead of fighting over the VM.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use agent_core::model::MachineState;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

use crate::task::Shared;

/// Set once the user (or the tray) has decided to really leave.
static QUITTING: AtomicBool = AtomicBool::new(false);
pub const QUIT_REQUESTED: &str = "lifecycle://quit-requested";

/// The exclusive lock on the data folder; held for the life of the process.
pub struct OwnerLock(#[allow(dead_code)] File);

/// Take the folder's owner lock, or say who has it.
pub fn acquire_owner_lock(dir: &Path) -> Result<OwnerLock, String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("app.lock"))
        .map_err(|error| error.to_string())?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => {
            "Another Cabin already owns this machine. Only one app can run it at a time.".into()
        }
        TryLockError::Error(error) => format!("Could not lock the Cabin data folder: {error}"),
    })?;
    Ok(OwnerLock(file))
}

pub fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Bring the window forward and let the UI ask what to do.
pub fn request_quit(app: &AppHandle) {
    show_main(app);
    let _ = app.emit(QUIT_REQUESTED, ());
}

pub fn is_quitting() -> bool {
    QUITTING.load(Ordering::SeqCst)
}

/// The red close button hides instead of closing.
pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    if let WindowEvent::CloseRequested { api, .. } = event
        && !is_quitting()
    {
        api.prevent_close();
        let _ = window.hide();
    }
}

fn label(state: MachineState, busy: bool) -> &'static str {
    match (state, busy) {
        (_, true) => "Machine: working…",
        (MachineState::Running, _) => "Machine: running",
        (MachineState::Stopped, _) => "Machine: stopped",
        (MachineState::NotInstalled, _) => "Machine: not installed",
    }
}

/// The tray icon and its menu, kept in step with the machine.
pub fn install_tray(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(app, "status", "Machine: …", false, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "Show cabin", true, None::<&str>)?;
    let toggle = MenuItem::with_id(app, "toggle", "Start machine", false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Cabin…", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &status,
            &PredefinedMenuItem::separator(app)?,
            &show,
            &toggle,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;
    let mut builder = TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip("Cabin")
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => show_main(app),
            "quit" => request_quit(app),
            "toggle" => {
                let host = app.state::<Shared>().inner().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    let running = host.machine_status().state == MachineState::Running;
                    if let Err(error) = if running { host.stop() } else { host.start() } {
                        cabin_diagnostics::event(
                            cabin_diagnostics::Level::Warn,
                            "lifecycle",
                            "tray_machine_toggle_failed",
                            format_args!("{}", error.message),
                        );
                    }
                });
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .icon(tauri::include_image!("icons/tray.png"))
            .icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    {
        if let Some(icon) = app.default_window_icon() {
            builder = builder.icon(icon.clone());
        }
    }
    builder.build(app)?;

    let host = std::sync::Arc::downgrade(app.state::<Shared>().inner());
    std::thread::Builder::new()
        .name("machine-tray".into())
        .spawn(move || {
            while let Some(host) = host.upgrade() {
                if is_quitting() {
                    break;
                }
                let machine = host.machine_status();
                drop(host);
                let busy = machine.busy.is_some();
                let _ = status.set_text(label(machine.state, busy));
                let _ = toggle.set_enabled(!busy && machine.state != MachineState::NotInstalled);
                let _ = toggle.set_text(if machine.state == MachineState::Running {
                    "Stop machine"
                } else {
                    "Start machine"
                });
                std::thread::sleep(Duration::from_secs(2));
            }
        })?;
    Ok(())
}

/// Leave the app. With `stop_machine` the VM is stopped first, so nothing is
/// left running; without it, an idle VM can continue running on its own.
#[tauri::command]
pub(crate) async fn app_quit(
    app: AppHandle,
    host: tauri::State<'_, Shared>,
    stop_machine: bool,
) -> Result<(), crate::app_error::AppError> {
    let shared = host.inner().clone();
    crate::task::blocking(&shared, move |host| host.prepare_quit(stop_machine)).await?;
    QUITTING.store(true, Ordering::SeqCst);
    app.exit(0);
    Ok(())
}

/// Hide the window and keep everything running (the tray brings it back).
#[tauri::command]
pub(crate) fn app_background(app: AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.hide();
    }
}

#[cfg(test)]
mod tests {
    use super::acquire_owner_lock;

    #[test]
    fn owner_lock_rejects_a_second_handle_until_the_first_is_released() -> Result<(), String> {
        let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
        let owner = acquire_owner_lock(dir.path())?;
        assert!(
            matches!(acquire_owner_lock(dir.path()), Err(message) if message.contains("already owns"))
        );
        drop(owner);
        assert!(acquire_owner_lock(dir.path()).is_ok());
        Ok(())
    }
}
