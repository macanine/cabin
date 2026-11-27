//! Host CLI (`cabin`) configuration, launcher management, PATH detection, and
//! Settings IPC commands.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
#[cfg(unix)]
use std::sync::{Mutex, MutexGuard};

use agent_core::model::SyncState;
#[cfg(unix)]
use agent_core::model::{GuestConnection, MachineState};
use agent_core::sync::SyncMode;
#[cfg(unix)]
use agent_core::util::LockExt;
use agent_core::util::RwLockExt;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::app_error::AppError;
use crate::task::{Shared, blocking};

fn default_true() -> bool {
    true
}

/// Persisted user preferences for the `cabin` host CLI passthrough.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HostCliConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub auto_bind_prompt: bool,
    #[serde(default = "default_true")]
    pub forward_terminal_env: bool,
    #[serde(default = "default_true")]
    pub allow_shell: bool,
    #[serde(default)]
    pub launcher_dir: Option<String>,
}

impl Default for HostCliConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            auto_bind_prompt: true,
            forward_terminal_env: true,
            allow_shell: true,
            launcher_dir: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "Keep Unix variants in the shared wire type")
)]
#[serde(rename_all = "camelCase")]
pub enum LauncherState {
    Installed,
    NotInstalled,
    Conflict,
    Outdated,
    // Constructed on non-Unix hosts and exported via ts-rs for shared TS types.
    #[cfg_attr(unix, allow(dead_code))]
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "Keep Unix variants in the shared wire type")
)]
#[serde(rename_all = "camelCase")]
pub enum NativeChannelState {
    Ready,
    NeedsRestart,
    VmStopped,
    Disabled,
    // Constructed on non-Unix hosts and exported via ts-rs for shared TS types.
    #[cfg_attr(unix, allow(dead_code))]
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HostCliBinding {
    pub id: String,
    pub name: String,
    pub host_path: String,
    pub guest_path: String,
    /// `"safe" | "twoWay" | "mirror"`
    pub mode: String,
    pub paused: bool,
    pub active_session: bool,
    pub conflicts: usize,
    /// `"offline" | "scanning" | "syncing" | "watching" | "paused" | "error"`
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HostCliStatus {
    pub supported: bool,
    pub config: HostCliConfig,
    pub broker_running: bool,
    pub launcher_state: LauncherState,
    pub launcher_path: String,
    pub launcher_dir: String,
    pub launcher_detail: Option<String>,
    pub in_path: bool,
    pub shell_name: String,
    pub path_snippet: String,
    pub channel_state: NativeChannelState,
    pub active_project_id: Option<String>,
    pub bindings: Vec<HostCliBinding>,
}

/// Runtime state shared between Settings commands and the Unix CLI broker.
pub struct HostCliState {
    config: RwLock<HostCliConfig>,
    config_path: PathBuf,
    #[cfg(unix)]
    broker_start: Mutex<()>,
    #[cfg(unix)]
    broker_running: AtomicBool,
}

impl HostCliState {
    pub fn open() -> Arc<Self> {
        Self::open_at(config_path())
    }

    fn open_at(path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            config: RwLock::new(load_config(&path)),
            config_path: path,
            #[cfg(unix)]
            broker_start: Mutex::new(()),
            #[cfg(unix)]
            broker_running: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> HostCliConfig {
        self.config.read_lock().clone()
    }

    pub fn set_config(&self, config: HostCliConfig) -> Result<(), String> {
        let normalized = normalize_config(config)?;
        let mut saved = self.config.write_lock();
        save_config(&self.config_path, &normalized)?;
        *saved = normalized;
        Ok(())
    }

    #[cfg(unix)]
    pub(crate) fn lock_broker_start(&self) -> MutexGuard<'_, ()> {
        self.broker_start.locked()
    }

    #[cfg(unix)]
    pub(crate) fn set_broker_running(&self, running: bool) {
        self.broker_running.store(running, Ordering::Release);
    }

    #[cfg(unix)]
    pub(crate) fn broker_running(&self) -> bool {
        self.broker_running.load(Ordering::Acquire)
    }
}

pub(crate) fn cli_state_dir() -> PathBuf {
    crate::data_dir().join("cli")
}

fn config_path() -> PathBuf {
    cli_state_dir().join("config.json")
}

fn load_config(path: &Path) -> HostCliConfig {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_config(path: &Path, config: &HostCliConfig) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or("Host CLI config path has no directory")?;
    create_private_dir(dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    agent_core::fsx::atomic_write(path, &bytes, Some(0o600)).map_err(|e| e.to_string())
}

fn normalize_config(mut config: HostCliConfig) -> Result<HostCliConfig, String> {
    config.launcher_dir = config
        .launcher_dir
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(dir) = &config.launcher_dir {
        let expanded = agent_core::sync::expand_home(dir);
        if !expanded.is_absolute() || expanded.parent().is_none() {
            return Err(
                "Choose an absolute directory for the cabin command, such as ~/.local/bin".into(),
            );
        }
    }
    Ok(config)
}

pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = dir.symlink_metadata()?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(std::io::Error::other(
                "Cabin CLI directory must belong to you and have mode 0700",
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

pub(crate) fn default_launcher_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".local/bin")
}

pub(crate) fn resolved_launcher_dir(config: &HostCliConfig) -> PathBuf {
    match config.launcher_dir.as_deref() {
        Some(dir) if !dir.trim().is_empty() => agent_core::sync::expand_home(dir.trim()),
        _ => default_launcher_dir(),
    }
}

fn format_mode(mode: SyncMode) -> String {
    match mode {
        SyncMode::Safe => "safe",
        SyncMode::TwoWay => "twoWay",
        SyncMode::Mirror => "mirror",
    }
    .to_string()
}

fn format_state(state: SyncState) -> String {
    match state {
        SyncState::Offline => "offline",
        SyncState::Scanning => "scanning",
        SyncState::Syncing => "syncing",
        SyncState::Watching => "watching",
        SyncState::Paused => "paused",
        SyncState::Error => "error",
    }
    .to_string()
}

#[cfg(unix)]
mod unix_launcher {
    use super::*;
    use std::io::{self, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::symlink;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    pub(super) fn managed_target() -> PathBuf {
        cli_state_dir().join("desktop-cli")
    }

    pub(super) fn refresh_managed_target() -> io::Result<PathBuf> {
        let dir = cli_state_dir();
        create_private_dir(&dir)?;
        let dir = dir.canonicalize()?;
        let exe = std::env::current_exe()?.canonicalize()?;
        let target = dir.join("desktop-cli");
        match target.symlink_metadata() {
            Ok(meta) if !meta.file_type().is_symlink() => {
                return Err(io::Error::other(
                    "Cabin's managed launcher path is occupied",
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        replace_symlink(&exe, &target, &dir)?;
        Ok(target)
    }

    fn replace_symlink(target: &Path, link: &Path, parent: &Path) -> io::Result<()> {
        let temp = parent.join(agent_core::util::new_id(".cabin-link"));
        symlink(target, &temp)?;
        let result = std::fs::rename(&temp, link);
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result
    }

    pub(super) fn inspect_launcher(
        bin_dir: &Path,
        target: &Path,
        current_exe: Option<&Path>,
    ) -> (LauncherState, Option<String>) {
        let command = bin_dir.join("cabin");
        let meta = match command.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return (LauncherState::NotInstalled, None);
            }
            Err(e) => return (LauncherState::Conflict, Some(e.to_string())),
        };
        if !meta.file_type().is_symlink() {
            return (
                LauncherState::Conflict,
                Some(format!(
                    "{} is an existing file not managed by Cabin",
                    command.display()
                )),
            );
        }
        let Ok(link) = std::fs::read_link(&command) else {
            return (
                LauncherState::Conflict,
                Some("Unable to read existing symlink".into()),
            );
        };
        if link != target {
            // Also compare canonicalized target in case data_dir symlinked (e.g. /var -> /private/var).
            let canonical_target = target
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .map(|p| p.join("desktop-cli"));
            if canonical_target.as_deref() != Some(link.as_path()) {
                return (
                    LauncherState::Conflict,
                    Some(format!("Points to {}", link.display())),
                );
            }
        }
        let Ok(target_exe) = std::fs::read_link(&link) else {
            return (
                LauncherState::Outdated,
                Some("Managed launcher target is missing".into()),
            );
        };
        if !target_exe.exists() {
            return (
                LauncherState::Outdated,
                Some(format!(
                    "Target executable no longer exists: {}",
                    target_exe.display()
                )),
            );
        }
        if let Some(expected) = current_exe {
            let actual = target_exe.canonicalize().unwrap_or(target_exe);
            let expected = expected
                .canonicalize()
                .unwrap_or_else(|_| expected.to_path_buf());
            if actual != expected {
                return (
                    LauncherState::Outdated,
                    Some("Points to a different Cabin build".into()),
                );
            }
        }
        (LauncherState::Installed, None)
    }

    pub(super) fn install_launcher(bin_dir: &Path, replace: bool) -> io::Result<()> {
        let target = refresh_managed_target()?;
        std::fs::create_dir_all(bin_dir)?;
        let command = bin_dir.join("cabin");
        match symlink(&target, &command) {
            Ok(()) => Ok(()),
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && std::fs::read_link(&command).is_ok_and(|link| link == target) =>
            {
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && replace => {
                let meta = command.symlink_metadata()?;
                if meta.is_dir() && !meta.file_type().is_symlink() {
                    return Err(io::Error::other(format!(
                        "{} is a directory and cannot be replaced",
                        command.display()
                    )));
                }
                replace_symlink(&target, &command, bin_dir)
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
                e.kind(),
                format!(
                    "{} already exists; Cabin did not replace it",
                    command.display()
                ),
            )),
            Err(e) => Err(e),
        }
    }

    pub(super) fn uninstall_launcher(bin_dir: &Path) -> io::Result<()> {
        let command = bin_dir.join("cabin");
        let meta = match command.symlink_metadata() {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        if !meta.file_type().is_symlink() {
            return Err(io::Error::other(format!(
                "{} is not managed by Cabin",
                command.display()
            )));
        }
        let target = managed_target();
        let canonical_target = target
            .parent()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| p.join("desktop-cli"));
        let link = std::fs::read_link(&command)?;
        if link != target && canonical_target.as_deref() != Some(link.as_path()) {
            return Err(io::Error::other(format!(
                "{} points to {} and is not managed by Cabin",
                command.display(),
                link.display()
            )));
        }
        std::fs::remove_file(&command)
    }

    /// On startup, refresh the private `desktop-cli` pointer whenever Host CLI
    /// is enabled. Create the public `~/.local/bin/cabin` symlink automatically
    /// only on first run (before `config.json` exists) or if it already points
    /// to Cabin, so explicit uninstalls from Settings stay uninstalled.
    pub(super) fn maintain_on_startup(state: &HostCliState) -> io::Result<()> {
        let config = state.config();
        if !config.enabled {
            return Ok(());
        }
        let target = refresh_managed_target()?;
        let bin_dir = resolved_launcher_dir(&config);
        let first_run = !config_path().exists();
        let (launcher_state, _) = inspect_launcher(&bin_dir, &target, None);
        if first_run && launcher_state == LauncherState::NotInstalled {
            install_launcher(&bin_dir, false)?;
        }
        Ok(())
    }

    pub(super) fn detect_shell_and_path(bin_dir: &Path) -> (String, bool, String) {
        let shell_env = std::env::var("SHELL").unwrap_or_else(|_| {
            if cfg!(target_os = "macos") {
                "/bin/zsh".into()
            } else {
                "/bin/bash".into()
            }
        });
        let shell_name = Path::new(&shell_env)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("sh")
            .to_string();
        let snippet = path_snippet_for_shell(&shell_name, bin_dir);
        let in_path = dir_in_env_path(bin_dir)
            || dir_in_login_shell_path(&shell_env, bin_dir)
            || dir_in_shell_rc(&shell_name, bin_dir);
        (shell_name, in_path, snippet)
    }

    pub(super) fn path_snippet_for_shell(shell_name: &str, bin_dir: &Path) -> String {
        let fish = shell_name == "fish";
        let quote = |path: &Path| {
            let path = path.to_string_lossy();
            if fish {
                // Fish recognizes escaped backslashes and quotes inside single quotes.
                format!("'{}'", path.replace('\\', "\\\\").replace('\'', "\\'"))
            } else {
                vm_core::util::shell_quote(&path)
            }
        };
        let path = if let Some(home) = dirs::home_dir()
            && let Ok(relative) = bin_dir.strip_prefix(home)
        {
            if relative.as_os_str().is_empty() {
                "\"$HOME\"".into()
            } else {
                format!("\"$HOME\"/{}", quote(relative))
            }
        } else {
            quote(bin_dir)
        };
        if fish {
            return format!("fish_add_path {path}");
        }

        let export = format!("export PATH={path}:\"$PATH\"");
        let rc = match shell_name {
            "bash" => "\"$HOME/.bashrc\"",
            "zsh" => "\"$HOME/.zshrc\"",
            _ => return export,
        };
        // Quote the complete export separately for the command appending it to the rc file.
        let export = vm_core::util::shell_quote(&export);
        format!("printf '%s\\n' {export} >> {rc} && . {rc}")
    }

    fn path_contains_dir(path_var: &str, bin_dir: &Path) -> bool {
        let canonical = bin_dir.canonicalize().ok();
        for entry in std::env::split_paths(path_var) {
            if entry == bin_dir {
                return true;
            }
            if let (Some(want), Ok(got)) = (&canonical, entry.canonicalize())
                && want == &got
            {
                return true;
            }
        }
        false
    }

    fn dir_in_env_path(bin_dir: &Path) -> bool {
        std::env::var("PATH")
            .ok()
            .is_some_and(|path| path_contains_dir(&path, bin_dir))
    }

    fn dir_in_login_shell_path(shell_env: &str, bin_dir: &Path) -> bool {
        let shell_path = Path::new(shell_env);
        if !shell_path.is_absolute() || !shell_path.is_file() {
            return false;
        }
        login_shell_path(shell_path).is_some_and(|path| path_contains_dir(path.trim(), bin_dir))
    }

    pub(super) fn login_shell_path(shell_path: &Path) -> Option<String> {
        const MAX_OUTPUT: usize = 64 * 1024;
        let mut child = Command::new(shell_path)
            .args(["-lc", "printf '%s' \"$PATH\""])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let path = (|| {
            let mut stdout = child.stdout.take()?;
            let fd = stdout.as_raw_fd();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return None;
            }
            let mut out = Vec::new();
            while Instant::now() < deadline {
                let status = child.try_wait().ok()?;
                // A process launched by shell startup may inherit stdout. Read
                // data available after the shell exits without waiting for EOF.
                match (&mut stdout)
                    .take((MAX_OUTPUT + 1 - out.len()) as u64)
                    .read_to_end(&mut out)
                {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => return None,
                }
                if out.len() > MAX_OUTPUT {
                    return None;
                }
                if let Some(status) = status {
                    return status
                        .success()
                        .then_some(out)
                        .and_then(|out| String::from_utf8(out).ok());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            None
        })();
        if path.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        path
    }

    /// Fallback for macOS GUI apps where `~/.zshrc` sets PATH for interactive
    /// terminals rather than `~/.zprofile`.
    fn dir_in_shell_rc(shell_name: &str, bin_dir: &Path) -> bool {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        let files: &[&str] = match shell_name {
            "zsh" => &[".zshrc", ".zprofile", ".zshenv"],
            "bash" => &[".bashrc", ".bash_profile", ".profile"],
            "fish" => &[".config/fish/config.fish", ".config/fish/fish_variables"],
            _ => &[".profile"],
        };
        let needle_abs = bin_dir.display().to_string();
        let needle_rel = bin_dir
            .strip_prefix(&home)
            .ok()
            .map(|r| r.display().to_string());
        for rel in files {
            let Ok(text) = std::fs::read_to_string(home.join(rel)) else {
                continue;
            };
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                if trimmed.contains(&needle_abs)
                    || needle_rel
                        .as_deref()
                        .is_some_and(|r| !r.is_empty() && trimmed.contains(r))
                {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(unix)]
pub(crate) fn maintain_launcher_on_startup(state: &HostCliState) -> std::io::Result<()> {
    unix_launcher::maintain_on_startup(state)
}

#[cfg(any(not(unix), test))]
fn unsupported_status(
    config: HostCliConfig,
    active_project_id: Option<String>,
    bindings: Vec<HostCliBinding>,
) -> HostCliStatus {
    let bin_dir = resolved_launcher_dir(&config);
    HostCliStatus {
        supported: false,
        config,
        broker_running: false,
        launcher_state: LauncherState::Unsupported,
        launcher_path: bin_dir.join("cabin").display().to_string(),
        launcher_dir: bin_dir.display().to_string(),
        launcher_detail: Some("Host CLI passthrough currently requires macOS or Linux.".into()),
        in_path: false,
        shell_name: "powershell".into(),
        path_snippet: String::new(),
        channel_state: NativeChannelState::Unsupported,
        active_project_id,
        bindings,
    }
}

pub(crate) fn collect_status(host: &crate::host::Host, state: &HostCliState) -> HostCliStatus {
    let config = state.config();
    let active_project_id = host.sync.active_native_project();
    let bindings: Vec<HostCliBinding> = host
        .sync
        .list()
        .into_iter()
        .map(|view| HostCliBinding {
            active_session: active_project_id.as_deref() == Some(view.project.id.as_str()),
            id: view.project.id,
            name: view.project.name,
            host_path: view.project.host_path,
            guest_path: view.project.guest_path,
            mode: format_mode(view.project.mode),
            paused: view.project.paused,
            conflicts: view.status.conflicts.len(),
            state: format_state(view.status.state),
        })
        .collect();

    #[cfg(unix)]
    {
        let bin_dir = resolved_launcher_dir(&config);
        let launcher_path = bin_dir.join("cabin").display().to_string();
        let launcher_dir = bin_dir.display().to_string();
        let current_exe = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
        let target = unix_launcher::managed_target();
        let (launcher_state, launcher_detail) =
            unix_launcher::inspect_launcher(&bin_dir, &target, current_exe.as_deref());
        let (shell_name, in_path, path_snippet) = unix_launcher::detect_shell_and_path(&bin_dir);
        let connected = host.machine_state() == (MachineState::Running, GuestConnection::Connected);
        let channels_ready = config.enabled
            && connected
            && state.broker_running()
            && host.ready_native_channels().is_ok();
        let broker_running = state.broker_running();
        let channel_state = if !config.enabled {
            NativeChannelState::Disabled
        } else if !connected {
            NativeChannelState::VmStopped
        } else if broker_running && channels_ready {
            NativeChannelState::Ready
        } else {
            NativeChannelState::NeedsRestart
        };
        HostCliStatus {
            supported: true,
            config,
            broker_running,
            launcher_state,
            launcher_path,
            launcher_dir,
            launcher_detail,
            in_path,
            shell_name,
            path_snippet,
            channel_state,
            active_project_id,
            bindings,
        }
    }

    #[cfg(not(unix))]
    {
        unsupported_status(config, active_project_id, bindings)
    }
}

#[tauri::command]
pub(crate) async fn host_cli_status(
    host: State<'_, Shared>,
    cli: State<'_, Arc<HostCliState>>,
) -> Result<HostCliStatus, AppError> {
    let cli = cli.inner().clone();
    blocking(&host, move |host| Ok(collect_status(host, &cli))).await
}

#[tauri::command]
pub(crate) async fn host_cli_save_config(
    host: State<'_, Shared>,
    cli: State<'_, Arc<HostCliState>>,
    config: HostCliConfig,
) -> Result<HostCliStatus, AppError> {
    let cli = cli.inner().clone();
    #[cfg(unix)]
    let broker_host = host.inner().clone();
    blocking(&host, move |host| {
        cli.set_config(config).map_err(AppError::invalid)?;
        #[cfg(unix)]
        if cli.config().enabled
            && !cli.broker_running()
            && let Err(error) = crate::native_cli::start_broker(&broker_host, cli.clone())
        {
            cabin_diagnostics::event(
                cabin_diagnostics::Level::Warn,
                "cli",
                "broker_start_failed",
                format_args!("{error}"),
            );
        }
        Ok(collect_status(host, &cli))
    })
    .await
}

#[tauri::command]
pub(crate) async fn host_cli_install_launcher(
    host: State<'_, Shared>,
    cli: State<'_, Arc<HostCliState>>,
    replace: bool,
) -> Result<HostCliStatus, AppError> {
    #[cfg(not(unix))]
    let _ = cli;
    #[cfg(unix)]
    let cli = cli.inner().clone();
    blocking(&host, move |host| {
        #[cfg(unix)]
        {
            let dir = resolved_launcher_dir(&cli.config());
            unix_launcher::install_launcher(&dir, replace)
                .map_err(|e| AppError::internal(e.to_string()))?;
            Ok(collect_status(host, &cli))
        }
        #[cfg(not(unix))]
        {
            let _ = (host, replace);
            Err(AppError::invalid(
                "Host CLI launcher is currently supported on macOS and Linux.",
            ))
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn host_cli_uninstall_launcher(
    host: State<'_, Shared>,
    cli: State<'_, Arc<HostCliState>>,
) -> Result<HostCliStatus, AppError> {
    #[cfg(not(unix))]
    let _ = cli;
    #[cfg(unix)]
    let cli = cli.inner().clone();
    blocking(&host, move |host| {
        #[cfg(unix)]
        {
            let dir = resolved_launcher_dir(&cli.config());
            unix_launcher::uninstall_launcher(&dir)
                .map_err(|e| AppError::internal(e.to_string()))?;
            Ok(collect_status(host, &cli))
        }
        #[cfg(not(unix))]
        {
            let _ = host;
            Err(AppError::invalid(
                "Host CLI launcher is currently supported on macOS and Linux.",
            ))
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn host_cli_repair_channels(
    host: State<'_, Shared>,
    cli: State<'_, Arc<HostCliState>>,
) -> Result<HostCliStatus, AppError> {
    let cli = cli.inner().clone();
    blocking(&host, move |host| {
        host.repair_native_channels()?;
        Ok(collect_status(host, &cli))
    })
    .await
}

#[tauri::command]
pub(crate) async fn host_cli_bind_folder(
    host: State<'_, Shared>,
    cli: State<'_, Arc<HostCliState>>,
    host_path: String,
    guest_path: Option<String>,
) -> Result<HostCliStatus, AppError> {
    #[cfg(not(unix))]
    let _ = cli;
    #[cfg(unix)]
    let cli = cli.inner().clone();
    blocking(&host, move |host| {
        #[cfg(unix)]
        {
            let path = agent_core::sync::expand_home(host_path.trim())
                .canonicalize()
                .map_err(|e| AppError::invalid(format!("{host_path}: {e}")))?;
            let guest = guest_path
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            crate::native_cli::bind_from_desktop(&host.sync, path, guest)
                .map_err(AppError::invalid)?;
            Ok(collect_status(host, &cli))
        }
        #[cfg(not(unix))]
        {
            let _ = (host, host_path, guest_path);
            Err(AppError::invalid(
                "Host CLI binding is currently supported on macOS and Linux.",
            ))
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn host_cli_unbind_folder(
    host: State<'_, Shared>,
    cli: State<'_, Arc<HostCliState>>,
    id: String,
) -> Result<HostCliStatus, AppError> {
    let cli = cli.inner().clone();
    blocking(&host, move |host| {
        host.sync.remove(&id).map_err(AppError::invalid)?;
        Ok(collect_status(host, &cli))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_saves_keep_memory_and_disk_in_agreement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli/config.json");
        let state = HostCliState::open_at(path.clone());
        let ready = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for n in 0..8 {
                let state = &state;
                let ready = &ready;
                scope.spawn(move || {
                    ready.wait();
                    for repeat in 0..4 {
                        state
                            .set_config(HostCliConfig {
                                launcher_dir: Some(dir_for_test(n, repeat)),
                                ..Default::default()
                            })
                            .unwrap();
                    }
                });
            }
        });
        assert_eq!(state.config(), load_config(&path));
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );

        let saved = state.config();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(state.set_config(HostCliConfig::default()).is_err());
        assert_eq!(state.config(), saved);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
    }

    fn dir_for_test(worker: usize, repetition: usize) -> String {
        let root = if cfg!(windows) { "C:/" } else { "/" };
        format!("{root}cabin-test-{worker}-{repetition}")
    }

    #[cfg(unix)]
    #[test]
    fn shell_path_probe_does_not_wait_for_a_background_process_to_close_stdout() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let shell = dir.path().join("shell");
        std::fs::write(&shell, "#!/bin/sh\nsleep 4 &\nprintf '/usr/bin:/bin'\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = Instant::now();
        assert_eq!(
            unix_launcher::login_shell_path(&shell).as_deref(),
            Some("/usr/bin:/bin")
        );
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn normalize_config_rejects_relative_or_root_launcher_dir() {
        let root = if cfg!(windows) { "C:/" } else { "/" };
        let absolute = format!("{root}usr/local/bin");
        assert!(normalize_config(HostCliConfig::default()).is_ok());
        assert!(
            normalize_config(HostCliConfig {
                launcher_dir: Some("relative/bin".into()),
                ..Default::default()
            })
            .is_err()
        );
        assert!(
            normalize_config(HostCliConfig {
                launcher_dir: Some(root.into()),
                ..Default::default()
            })
            .is_err()
        );
        let ok = normalize_config(HostCliConfig {
            launcher_dir: Some(format!("  {absolute}  ")),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(ok.launcher_dir.as_deref(), Some(absolute.as_str()));
    }

    #[cfg(unix)]
    #[test]
    fn inspect_install_conflict_and_uninstall_launcher() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let target = dir.path().join("desktop-cli");
        let fake_exe = dir.path().join("cabin-desktop");
        std::fs::write(&fake_exe, b"bin").unwrap();
        symlink(&fake_exe, &target).unwrap();

        assert_eq!(
            unix_launcher::inspect_launcher(&bin, &target, Some(&fake_exe)).0,
            LauncherState::NotInstalled
        );

        // Regular file occupying `cabin` is detected as Conflict.
        std::fs::write(bin.join("cabin"), b"other").unwrap();
        assert_eq!(
            unix_launcher::inspect_launcher(&bin, &target, Some(&fake_exe)).0,
            LauncherState::Conflict
        );

        // Symlink pointing to our managed target is Installed.
        std::fs::remove_file(bin.join("cabin")).unwrap();
        symlink(&target, bin.join("cabin")).unwrap();
        assert_eq!(
            unix_launcher::inspect_launcher(&bin, &target, Some(&fake_exe)).0,
            LauncherState::Installed
        );

        // Different current_exe reports Outdated.
        let other_exe = dir.path().join("other-build");
        std::fs::write(&other_exe, b"bin2").unwrap();
        assert_eq!(
            unix_launcher::inspect_launcher(&bin, &target, Some(&other_exe)).0,
            LauncherState::Outdated
        );
    }

    #[cfg(unix)]
    #[test]
    fn shell_path_snippets_preserve_literal_paths_and_expand_home() {
        use std::process::Command;

        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join(r#"shell home with ' " \ $ `"#);
        std::fs::create_dir(&home).unwrap();
        let actual_home = dirs::home_dir().unwrap();
        let remainder = r#"bin with ' " \ $VAR $(touch injected) `touch injected`"#;
        let absolute = Path::new("/opt").join(remainder);
        let home_relative = actual_home.join(remainder);
        for shell in ["sh", "bash", "zsh"] {
            let binary = PathBuf::from(format!("/bin/{shell}"));
            let binary = if binary.is_file() {
                binary.as_path()
            } else {
                Path::new("/bin/sh")
            };
            for (dir, expected) in [
                (&absolute, absolute.clone()),
                (&home_relative, home.join(remainder)),
            ] {
                let rc = home.join(format!(".{shell}rc"));
                if rc.is_file() {
                    std::fs::remove_file(rc).unwrap();
                }
                let snippet = unix_launcher::path_snippet_for_shell(shell, dir);
                let output = Command::new(binary)
                    .args(["-c", &format!("{snippet} && printf '%s' \"$PATH\"")])
                    .env_clear()
                    .env("HOME", &home)
                    .env("PATH", "/usr/bin:/bin")
                    .current_dir(temp.path())
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{shell}: {:?}", output.stderr);
                assert_eq!(
                    output.stdout,
                    format!("{}:/usr/bin:/bin", expected.display()).as_bytes()
                );
                assert!(!temp.path().join("injected").exists());
            }
        }

        assert_eq!(
            unix_launcher::path_snippet_for_shell("fish", &absolute),
            r#"fish_add_path '/opt/bin with \' " \\ $VAR $(touch injected) `touch injected`'"#
        );
        assert_eq!(
            unix_launcher::path_snippet_for_shell("fish", &home_relative),
            r#"fish_add_path "$HOME"/'bin with \' " \\ $VAR $(touch injected) `touch injected`'"#
        );
    }

    #[test]
    fn unsupported_status_reports_unsupported_states() {
        let status = unsupported_status(HostCliConfig::default(), None, Vec::new());
        assert!(!status.supported);
        assert_eq!(status.launcher_state, LauncherState::Unsupported);
        assert_eq!(status.channel_state, NativeChannelState::Unsupported);
    }
}
