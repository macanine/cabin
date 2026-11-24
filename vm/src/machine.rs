//! The single VM that Cabin manages.
//!
//! A thin facade over `Config` + `Runner` with a fixed instance name, so the
//! Mac app never deals with instance lists.

use crate::util::LockExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{build_config, load_instance_config};
use crate::lock::FileLock;
use crate::paths::{InstancePaths, StatePaths};
use crate::runner::Runner;
use crate::startup::{Attempt, Phase};
use crate::vm::{Lifecycle, Runtime, recorded_qemu_pid};
use crate::{Config, Result};

pub const NAME: &str = "agent";
/// Host-side SSH forwarding port for the desktop app's single VM.
pub const SSH_PORT: u16 = 12233;

/// Choices offered by the installer. Everything else uses `config/default.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstallOptions {
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_size: Option<String>,
    pub username: Option<String>,
    pub hostname: Option<String>,
    /// Apply the device profile.
    pub device: bool,
    /// Guest proxy URL used while provisioning (e.g. http://10.0.2.2:18080).
    pub proxy: Option<String>,
    /// Archive mirror for apt inside the VM (matching the OS family).
    pub apt_mirror: Option<String>,
    /// Guest operating system (`os::RELEASES`; `None`: the default).
    pub os: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineInfo {
    pub installed: bool,
    pub os: Option<String>,
    pub running: bool,
    pub ssh_port: Option<u16>,
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_size: Option<String>,
    pub desktop: bool,
    pub browser: bool,
    pub device: bool,
    pub username: Option<String>,
    pub hostname: Option<String>,
}

/// Live state of one pipeline step during an install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepProgress {
    pub id: String,
    pub title: String,
    /// pending | running | done | cached | failed | rolledBack
    pub state: String,
    pub detail: Option<String>,
    pub log_tail: Vec<String>,
    /// Full log for this step (for "show the whole log").
    pub log_path: Option<String>,
    /// Seconds spent so far (running) or in total (finished).
    pub seconds: Option<u64>,
    /// Recorded metadata: when the step was last applied, in Unix milliseconds …
    pub applied_at: Option<i64>,
    /// … and the configuration fingerprint it was applied for.
    pub fingerprint: Option<String>,
    /// Bytes, total and speed while the step downloads (the cloud image).
    #[serde(default)]
    pub download: Option<crate::download::DownloadProgress>,
}

/// An operating system the installer offers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsChoice {
    pub id: String,
    pub name: String,
    pub testing: bool,
    pub mirrors: Vec<String>,
}

pub fn os_choices() -> Vec<OsChoice> {
    crate::os::RELEASES
        .iter()
        .map(|os| OsChoice {
            id: os.id.to_string(),
            name: os.name.to_string(),
            testing: os.testing,
            mirrors: os.mirrors().iter().map(|m| m.to_string()).collect(),
        })
        .collect()
}

/// What the install will do, shown by the wizard before it starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    pub os: String,
    pub os_name: String,
    pub os_choices: Vec<OsChoice>,
    /// The newest build is pinned when the install starts.
    pub image_index: String,
    pub qemu_release: String,
    pub qemu_installed: bool,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_size: String,
    pub network: String,
    pub accel: String,
    pub firmware: Option<String>,
    pub disk_path: String,
    pub seed_path: String,
    pub username: String,
    pub hostname: String,
    pub ssh_port: u16,
    pub apt_mirror: Option<String>,
    pub steps: Vec<StepProgress>,
}

fn tail(path: &Path, lines: usize) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| {
            let all: Vec<&str> = text.lines().filter(|l| !l.starts_with("# step=")).collect();
            all[all.len().saturating_sub(lines)..]
                .iter()
                .map(|line| {
                    // Full command lines stay in the log file; keep the tail readable.
                    if line.chars().count() > 160 {
                        let short: String = line.chars().take(157).collect();
                        format!("{short}…")
                    } else {
                        line.to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Seconds since the `at=` stamp in a running step's log header.
fn log_age_seconds(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let header = text.lines().next()?;
    let started = header
        .split_whitespace()
        .find_map(|part| part.strip_prefix("at="))?;
    if let Ok(started) = started.parse::<i64>() {
        return Some((crate::util::now_millis().saturating_sub(started) / 1000).max(0) as u64);
    }
    // Step logs written before millisecond normalization carry an ISO stamp.
    let started = crate::util::parse_utc_stamp(started)?;
    SystemTime::now()
        .duration_since(started)
        .ok()
        .map(|d| d.as_secs())
}

fn meta_string(state: Option<&Value>, key: &str) -> Option<String> {
    state
        .filter(|s| s.get("status").and_then(Value::as_str) == Some("applied"))
        .and_then(|s| s.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A step-state timestamp as Unix milliseconds. New state stores a number;
/// state written before normalization carries a `utcnow()` string.
fn state_millis(state: Option<&Value>, key: &str) -> Option<i64> {
    crate::util::timestamp_millis(state?.get(key)?)
}

fn newest_log(dir: &Path, step: &str, since: SystemTime) -> Option<PathBuf> {
    let prefix = format!("{step}-apply-");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .filter(|(modified, _)| *modified >= since)
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path)
}

/// Step states for `cfg`, relative to an install that began at `since`.
pub fn progress(cfg: &Config, since: SystemTime) -> Vec<StepProgress> {
    let Ok(runner) = Runner::new(cfg.clone()) else {
        return Vec::new();
    };
    let visible = runner.visible_step_ids();
    let since_ms = since
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default();
    runner
        .steps
        .iter()
        .filter(|step| visible.contains(&step.id().to_string()))
        .map(|step| {
            let id = step.id().to_string();
            let scope = if matches!(id.as_str(), "qemu" | "image") {
                "global".to_string()
            } else {
                cfg.name.clone()
            };
            let state = crate::journal::load_state(&cfg.home, &scope, &id);
            let status = state
                .as_ref()
                .and_then(|s| s.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("new");
            let finished = state_millis(state.as_ref(), "finished_at");
            let fresh = finished.is_some_and(|finished| finished >= since_ms);
            let log_dir = if scope == "global" {
                cfg.logs_dir().join("global")
            } else {
                cfg.instance_logs_dir()
            };
            let log = newest_log(&log_dir, &id, since);
            // A satisfied step is skipped without saving state; its log says so.
            let satisfied = log.as_deref().and_then(|path| {
                std::fs::read_to_string(path)
                    .ok()?
                    .lines()
                    .find_map(|line| line.strip_prefix("already satisfied: ").map(str::to_string))
            });
            if let Some(reason) = satisfied {
                return StepProgress {
                    id,
                    title: step.title().to_string(),
                    state: "cached".into(),
                    detail: Some(format!("Already satisfied: {reason}")),
                    log_tail: Vec::new(),
                    log_path: log.as_deref().map(|p| p.display().to_string()),
                    seconds: None,
                    applied_at: None,
                    fingerprint: None,
                    download: None,
                };
            }
            let (label, detail) = match (status, fresh, &log) {
                ("failed", true, _) => (
                    "failed",
                    state
                        .as_ref()
                        .and_then(|s| s.get("error"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ),
                // Rolled back during *this* run (auto-rollback after a failure).
                // An older rollback followed by a fresh apply log means the
                // step is running again.
                ("rolled_back" | "rollback_failed", _, _)
                    if state_millis(state.as_ref(), "rolled_back_at")
                        .is_some_and(|at| at >= since_ms) =>
                {
                    ("rolledBack", None)
                }
                ("applied" | "ok", true, _) => ("done", None),
                ("applied" | "ok", false, None) => {
                    ("cached", Some("Already prepared earlier".to_string()))
                }
                (_, _, Some(_)) => ("running", None),
                _ => ("pending", None),
            };
            let download = match (label, id.as_str()) {
                ("running", "image") => crate::download::progress(&cfg.image_path()),
                ("running", "qemu") => {
                    crate::download::progress(&crate::qemu_engine::archive_path(&cfg.home))
                }
                _ => None,
            };
            StepProgress {
                id,
                title: step.title().to_string(),
                state: label.to_string(),
                detail,
                log_tail: log.as_deref().map(|path| tail(path, 8)).unwrap_or_default(),
                log_path: log.as_deref().map(|p| p.display().to_string()),
                seconds: state
                    .as_ref()
                    .filter(|_| fresh)
                    .and_then(|s| s.get("duration_s"))
                    .and_then(Value::as_f64)
                    .map(|d| d.round() as u64)
                    .or_else(|| log.as_deref().and_then(log_age_seconds)),
                applied_at: state_millis(state.as_ref(), "finished_at"),
                fingerprint: meta_string(state.as_ref(), "config_fingerprint")
                    .map(|f| f.chars().take(12).collect()),
                download,
            }
        })
        .collect()
}

pub struct Machine {
    home: PathBuf,
    root: PathBuf,
    /// Bundled Linux guest binary used by native helpers before the daemon is available.
    agentd_binary: PathBuf,
    /// Configuration of the install in progress (for cancelling it).
    installing: std::sync::Mutex<Option<Config>>,
    lifecycle: Arc<Lifecycle>,
}

impl Machine {
    pub fn new(home: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let lifecycle = Lifecycle::for_home(&home);
        Self {
            home,
            lifecycle,
            root: root.into(),
            agentd_binary: crate::agentd::default_binary(),
            installing: std::sync::Mutex::new(None),
        }
    }

    pub fn with_agentd(mut self, binary: impl Into<PathBuf>) -> Self {
        self.agentd_binary = binary.into();
        self
    }

    pub fn agentd_binary(&self) -> &Path {
        &self.agentd_binary
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    fn lock(&self) -> Result<FileLock> {
        crate::util::ensure_dir(&self.home)?;
        FileLock::acquire(&StatePaths::new(&self.home).operation_lock())
    }

    pub fn installed(&self) -> bool {
        let instance = StatePaths::new(&self.home).instance(NAME);
        [instance.meta(), instance.disk()].iter().any(|path| {
            !std::fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        }) || crate::vm::pid_alive(crate::vm::read_pid(&instance.pidfile()))
    }

    pub fn config(&self) -> Result<Config> {
        load_instance_config(&self.root, &self.home, NAME)
    }

    pub fn runtime(&self) -> Result<Runtime> {
        Ok(Runtime::new(&self.config()?))
    }

    /// Install (or repair) Chrome against the current VM state. Chrome is not
    /// an installer option: it is added afterwards from the Chrome page.
    pub fn install_chrome(&self) -> Result<()> {
        let _lock = self.lock()?;
        crate::chrome_feature::install(&self.config()?, &self.agentd_binary)
    }

    pub fn enable_spice(&self) -> Result<()> {
        let _lock = self.lock()?;
        crate::chrome_feature::enable_spice(&self.config()?, &self.agentd_binary)
    }

    /// The managed browser's settings, as configured on the Mac.
    pub fn browser_settings(&self) -> Result<crate::browser::Settings> {
        crate::browser::settings(&self.config()?)
    }

    /// Change the browser's settings on the running machine. A time zone or
    /// language change is applied live (the guest profile is patched in
    /// place); anything else writes the profile again and restarts Chrome.
    /// Neither needs a reinstall or a reboot.
    pub fn set_browser_settings(&self, settings: &crate::browser::Settings) -> Result<()> {
        let _lock = self.lock()?;
        let old = self.config()?;
        let updated = crate::browser::with_settings(&old, settings)?;
        if old.browser == updated.browser {
            return Ok(());
        }
        if crate::browser::identity_fingerprint(&old)? == crate::browser::identity_fingerprint(&updated)? {
            let runtime = Runtime::new(&old);
            if !runtime.running() {
                return crate::util::message("Start the machine to change the browser settings");
            }
            let _browser = FileLock::acquire(&crate::paths::InstancePaths::new(old.instance_dir()).browser_lock())?;
            crate::shell::apply_browser_region(&runtime, &old, &updated).map(|_| ())
        } else {
            crate::chrome_feature::apply_profile(&old, &updated, &self.agentd_binary)
        }
    }

    pub fn browser_native_display(&self, request: Value) -> Result<Value> {
        let _lock = if request["action"] == "status" {
            None
        } else {
            Some(self.lock()?)
        };
        crate::shell::browser_native_display(&self.home, NAME, request)
    }

    pub fn desktop_viewer(&self, protocol: &str, parent_origin: Option<&str>) -> Result<String> {
        let _lock = self.lock()?;
        let cfg = self.config()?;
        if !crate::desktop::enabled(&cfg) {
            return crate::util::message("The machine does not have a Sway desktop");
        }
        match protocol {
            "vnc" => crate::chrome_viewer::start(&cfg),
            "spice" => crate::chrome_viewer::start_spice(&cfg, parent_origin),
            _ => crate::util::message("Unknown desktop display protocol"),
        }
    }

    /// A local URL that reaches the guest's `localhost:<port>` (browser preview).
    pub fn preview_url(&self, port: u16) -> Result<String> {
        let cfg = self.config()?;
        crate::preview_forward::start(&cfg, port)
    }

    pub fn preview_close(&self, port: u16) -> Result<()> {
        let cfg = self.config()?;
        crate::preview_forward::close(&cfg, port)
    }

    pub fn storage_in_use(&self) -> bool {
        let instance = StatePaths::new(&self.home).instance(NAME);
        // Keep storage maintenance conservative while the pidfile is present.
        crate::vm::pid_alive(crate::vm::read_pid(&instance.pidfile()))
            || self.recorded_runtime_exists()
    }

    fn recorded_runtime_exists(&self) -> bool {
        let instance = StatePaths::new(&self.home).instance(NAME);
        !matches!(recorded_qemu_pid(&instance, NAME), Ok(None))
    }

    pub fn clean_storage(&self, target: crate::storage::CleanupTarget) -> Result<crate::storage::CleanupResult> {
        reject_symlink(&self.home)?;
        let _lock = self.lock()?;
        let paths = StatePaths::new(&self.home);
        reject_symlink(&paths.instances())?;
        reject_symlink(paths.instance(NAME).root())?;
        if self.storage_in_use() {
            return crate::util::message("Stop the machine before cleaning up its storage");
        }
        crate::storage::clean(&self.home, target)
    }

    pub fn save_resources(&self, request: crate::resources::ResourceRequest, cancel_disk: bool) -> Result<Option<crate::resources::ResourceRequest>> {
        let _lock = self.lock()?;
        self.lifecycle.check()?;
        let cfg = self.config()?;
        if cancel_disk {
            if self.storage_in_use() || Runtime::new(&cfg).alive() {
                return crate::message("Stop the machine before cancelling pending disk expansion");
            }
            crate::resources::cancel_disk(&cfg)
        } else {
            crate::resources::save(&cfg, request)
        }
    }

    pub fn details(&self) -> Result<Option<crate::machine_details::MachineDetails>> {
        self.installed()
            .then(|| crate::machine_details::MachineDetails::read(&self.config()?))
            .transpose()
    }

    pub fn info(&self) -> MachineInfo {
        self.machine_info(None)
    }

    /// Use the runtime state just established by startup without probing QMP again.
    pub fn configured_info(&self, running: bool) -> MachineInfo {
        self.machine_info(Some(running))
    }

    fn machine_info(&self, running: Option<bool>) -> MachineInfo {
        let installed = self.installed();
        match installed.then(|| self.config()).transpose() {
            Ok(Some(cfg)) => {
                let running = running.unwrap_or_else(|| Runtime::new(&cfg).alive());
                MachineInfo {
                    installed: true,
                    os: Some(cfg.os.clone()),
                    running,
                    ssh_port: Some(cfg.ssh_connect_port()),
                    cpus: Some(cfg.cpus),
                    memory_mb: Some(cfg.memory_mb),
                    disk_size: Some(cfg.disk_size.clone()),
                    desktop: cfg.desktop_env.is_some(),
                    browser: cfg.browser_enabled(),
                    device: cfg.device_enabled(),
                    username: Some(cfg.username.clone()),
                    hostname: Some(cfg.hostname.clone()),
                }
            }
            _ => MachineInfo {
                installed,
                os: None,
                running: installed && self.recorded_runtime_exists(),
                ssh_port: None,
                cpus: None,
                memory_mb: None,
                disk_size: None,
                desktop: false,
                browser: false,
                device: false,
                username: None,
                hostname: None,
            },
        }
    }

    fn install_overrides(&self, options: &InstallOptions) -> Value {
        let mut map = serde_json::Map::from_iter([
            ("home".into(), json!(self.home.display().to_string())),
            ("name".into(), json!(NAME)),
            ("ssh_port".into(), json!(SSH_PORT)),
        ]);
        // The app's VM is isolated: its internet goes only through the Mac
        // gateway (see `netgw`). Device brings its own network profile.
        map.insert("network_mode".into(), json!("gateway"));
        if let Some(cpus) = options.cpus {
            map.insert("cpus".into(), json!(cpus));
        }
        if let Some(memory) = options.memory_mb {
            map.insert("memory_mb".into(), json!(memory));
        }
        if let Some(disk) = &options.disk_size {
            map.insert("disk_size".into(), json!(disk));
        }
        if let Some(user) = options.username.as_deref().filter(|u| !u.trim().is_empty()) {
            map.insert("username".into(), json!(user.trim()));
        }
        if let Some(host) = options.hostname.as_deref().filter(|h| !h.trim().is_empty()) {
            map.insert("hostname".into(), json!(host.trim()));
        }
        let slowdown = crate::platform::slowdown();
        if slowdown > 1 {
            map.insert("provision_timeout".into(), json!(900 * slowdown));
        }
        if let Some(mirror) = options
            .apt_mirror
            .as_deref()
            .filter(|m| !m.trim().is_empty())
        {
            map.insert("apt_mirror".into(), json!(mirror));
        }
        if let Some(proxy) = &options.proxy {
            map.insert("proxy".into(), json!(proxy));
        }
        if let Some(os) = options.os.as_deref().filter(|o| !o.trim().is_empty()) {
            map.insert("os".into(), json!(os.trim()));
        }
        if options.device {
            // Device keeps the isolated gateway network: the VM never joins
            // the LAN (no socket_vmnet bridge).
            map.insert("device".into(), json!({ "enabled": true }));
        }
        Value::Object(map)
    }

    /// Configuration used by a new install, including its network socket.
    pub fn install_config(&self, options: &InstallOptions) -> Result<Config> {
        Self::validate(options)?;
        build_config(&self.install_overrides(options), &self.root)
    }

    /// Create, provision and start the VM. A failure destroys the partial
    /// instance so the next attempt starts clean.
    pub fn validate(options: &InstallOptions) -> Result<()> {
        if options.device && !crate::arch::GuestArch::current().device_profile_supported() {
            return crate::util::message(
                "The device profile is currently built for ARM machines; disable it to install on this host",
            );
        }
        if let Some(user) = options
            .username
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            && let Some(problem) = username_problem(user)
        {
            return crate::util::message(format!("Username “{user}”: {problem}"));
        }
        if let Some(host) = options
            .hostname
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            && let Some(problem) = hostname_problem(host)
        {
            return crate::util::message(format!("Hostname “{host}”: {problem}"));
        }
        crate::os::get(options.os.as_deref().unwrap_or(crate::os::DEFAULT))?;
        Ok(())
    }

    /// The configuration an install with `options` would use.
    pub fn plan(&self, options: &InstallOptions) -> Result<InstallPlan> {
        Self::validate(options)?;
        let cfg = build_config(&self.install_overrides(options), &self.root)?;
        let engine = crate::qemu_engine::installed(&cfg.home);
        let firmware = engine
            .as_ref()
            .map(|engine| engine.firmware_code.display().to_string());
        let accel = crate::platform::accel();
        let os = cfg.guest_os();
        Ok(InstallPlan {
            os: os.id.to_string(),
            os_name: os.name.to_string(),
            os_choices: os_choices(),
            image_index: os.index_url().to_string(),
            qemu_release: if cfg!(target_os = "macos") {
                crate::qemu_engine::RELEASE.to_string()
            } else {
                crate::arch::GuestArch::current().qemu_system_binary().to_string()
            },
            qemu_installed: engine.is_some(),
            cpus: cfg.cpus,
            memory_mb: cfg.memory_mb,
            disk_size: cfg.disk_size.clone(),
            network: cfg.effective_network_mode(),
            accel: format!("{} ({})", accel.accel, accel.cpu),
            firmware,
            disk_path: cfg.disk_path().display().to_string(),
            seed_path: cfg.seed_path().display().to_string(),
            username: cfg.username.clone(),
            hostname: cfg.hostname.clone(),
            ssh_port: cfg.ssh_port,
            apt_mirror: cfg.apt_mirror.clone(),
            steps: progress(&cfg, SystemTime::now()),
        })
    }

    pub fn install(&self, options: &InstallOptions) -> Result<MachineInfo> {
        self.install_with_progress(options, |_| {})
    }

    /// Install, reporting step progress about once a second.
    pub fn install_with_progress(
        &self,
        options: &InstallOptions,
        on_progress: impl Fn(Vec<StepProgress>) + Send + Sync + 'static,
    ) -> Result<MachineInfo> {
        Self::validate(options)?;
        let lock = self.lock()?;
        if self.installed() {
            return crate::util::message("The machine is already installed or has files to recover");
        }
        let mut cfg = build_config(&self.install_overrides(options), &self.root)?;
        crate::vm::validate_socket_paths(&cfg)?;
        let _install = self.begin_install(&cfg)?;
        // Pin the newest build of the chosen OS before anything is saved.
        cfg.pin_image()?;
        let runner = Runner::new(cfg.clone())?.with_agentd(&self.agentd_binary);
        let result = self.with_progress(&cfg, on_progress, || {
            runner
                .apply_locked(&lock)
                .and_then(|()| runner.start_vm_locked(&lock).map(|_| ()))
                .and_then(|()| {
                    if cfg.browser_enabled() {
                        crate::chrome_feature::verify_started(&cfg, &self.agentd_binary)
                    } else {
                        Ok(())
                    }
                })
        });
        if let Err(error) = result {
            return match runner.destroy_locked(&lock, false) {
                Ok(()) => Err(error),
                Err(cleanup) => crate::util::message(format!(
                    "{error}; cleaning up the partial install also failed: {cleanup}"
                )),
            };
        }
        drop(lock);
        Ok(self.info())
    }

    /// Run `work` while reporting step progress once a second.
    fn with_progress<T>(
        &self,
        cfg: &Config,
        on_progress: impl Fn(Vec<StepProgress>) + Send + Sync + 'static,
        work: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let since = SystemTime::now() - Duration::from_secs(1);
        let done = Arc::new(AtomicBool::new(false));
        let on_progress = Arc::new(on_progress);
        let poller = {
            let cfg = cfg.clone();
            let done = done.clone();
            let on_progress = on_progress.clone();
            std::thread::Builder::new().name("install-progress".into()).spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    on_progress(progress(&cfg, since));
                    std::thread::sleep(Duration::from_secs(1));
                }
            }).map_err(|error| crate::Error::Message(format!("Could not start reporting installation progress: {error}")))?
        };
        let _stop = StopPoller(done.clone());
        let result = self.lifecycle.check().and_then(|()| work());
        done.store(true, Ordering::Relaxed);
        let _ = poller.join();
        on_progress(progress(cfg, since));
        result
    }

    /// Check every step without changing anything. On a healthy install all
    /// steps report `satisfied`, i.e. re-applying would be a no-op.
    pub fn verify(&self) -> Result<Vec<StepProgress>> {
        let lock = self.lock()?;
        let cfg = self.config()?;
        let rows = Runner::new(cfg.clone())?.with_agentd(&self.agentd_binary).plan_locked(&lock)?;
        let recorded = progress(&cfg, SystemTime::now());
        Ok(rows
            .iter()
            .map(|row| {
                let id = row["id"].as_str().unwrap_or_default().to_string();
                let meta = recorded.iter().find(|p| p.id == id);
                StepProgress {
                    title: row["title"].as_str().unwrap_or_default().to_string(),
                    state: if row["ok"].as_bool().unwrap_or(false) {
                        "satisfied"
                    } else {
                        "needed"
                    }
                    .into(),
                    detail: row["detail"].as_str().map(str::to_string),
                    log_tail: Vec::new(),
                    log_path: row["log"].as_str().map(str::to_string),
                    seconds: None,
                    applied_at: meta.and_then(|m| m.applied_at),
                    fingerprint: meta.and_then(|m| m.fingerprint.clone()),
                    download: None,
                    id,
                }
            })
            .collect())
    }

    /// Re-apply whatever is missing (after a failure) and start the VM.
    /// Optionally switch the apt mirror first (it is part of the cloud-init
    /// seed, so it only takes effect while provisioning has not finished).
    pub fn resume_with_progress(
        &self,
        apt_mirror: Option<Option<String>>,
        on_progress: impl Fn(Vec<StepProgress>) + Send + Sync + 'static,
    ) -> Result<MachineInfo> {
        let lock = self.lock()?;
        let mut cfg = self.config()?;
        let _install = self.begin_install(&cfg)?;
        if let Some(mirror) = apt_mirror {
            cfg.apt_mirror = mirror
                .map(|m| m.trim().trim_end_matches('/').to_string())
                .filter(|m| !m.is_empty());
            cfg.validate()?;
            cfg.save_instance_meta()?;
        }
        let runner = Runner::new(cfg.clone())?.with_agentd(&self.agentd_binary);
        self.with_progress(&cfg, on_progress, || {
            runner
                .apply_locked(&lock)
                .and_then(|()| runner.start_vm_locked(&lock).map(|_| ()))
        })?;
        drop(lock);
        Ok(self.info())
    }

    fn begin_install(&self, cfg: &Config) -> Result<ClearInstalling<'_>> {
        let mut installing = self.installing.locked();
        self.lifecycle.check()?;
        *installing = Some(cfg.clone());
        Ok(ClearInstalling(self))
    }

    /// Cancel the install and stop its VM. Any later launch from that same
    /// install observes the cancellation before creating a QEMU process.
    pub fn abort_install(&self) -> bool {
        let cfg = {
            let installing = self.installing.locked();
            let Some(cfg) = installing.as_ref() else {
                return false;
            };
            self.lifecycle.set_cancelled(true);
            cfg.clone()
        };
        crate::download::cancel_all();
        let _ = self.stop_for_exit(&cfg, Duration::from_secs(12));
        true
    }

    pub fn begin_quit(&self) {
        self.lifecycle.begin_quit();
        if self.installing.locked().is_some() {
            crate::download::cancel_all();
        }
    }

    /// Stop at exit even when the work that launched QEMU still holds the
    /// operation lock. The runtime's separate barrier serializes launch/stop.
    pub fn shutdown(&self, timeout: Duration) -> Result<()> {
        self.begin_quit();
        let installing = self.installing.locked().clone();
        if let Some(cfg) = installing.or_else(|| self.config().ok()) {
            return self.stop_for_exit(&cfg, timeout);
        }
        let _gate = self.lifecycle.lock();
        let paths = StatePaths::new(&self.home);
        let instance = paths.instance(NAME);
        reject_symlink(&paths.instances())?;
        reject_symlink(instance.root())?;
        self.stop_without_config(&instance, timeout)
    }

    fn stop_for_exit(&self, cfg: &Config, timeout: Duration) -> Result<()> {
        let runtime = Runtime::new(cfg);
        let path = cfg.instance_logs_dir().join("shutdown.log");
        if let Some(parent) = path.parent() {
            let _ = crate::util::ensure_dir(parent);
        }
        // Logging failures must not leave QEMU running.
        let log = cabin_diagnostics::open_capture(&path, cabin_diagnostics::Capture::Replace);
        match log {
            Ok(log) => {
                runtime.stop(&Arc::new(std::sync::Mutex::new(log)), timeout);
                if runtime.alive() {
                    return crate::util::message(format!(
                        "The machine could not be stopped. See {}",
                        path.display()
                    ));
                }
                Ok(())
            }
            Err(_) => {
                let _gate = self.lifecycle.lock();
                self.stop_without_config(&InstancePaths::new(cfg.instance_dir()), timeout)
            }
        }
    }

    /// Re-apply the configuration (repairs drift) and start the VM.
    pub fn start(&self) -> Result<MachineInfo> {
        let attempt = Attempt::default();
        self.open_start_log(&attempt)?;
        let result = self.start_for_app(&attempt);
        attempt.finish(result.as_ref().err().map(ToString::to_string));
        result.map(|()| self.info())
    }

    pub fn open_start_log(&self, attempt: &Attempt) -> Result<()> {
        attempt.open_log(&StatePaths::new(&self.home).instance_logs(NAME))
    }

    /// Attach to a live VM or check, launch and prepare a stopped one.
    pub fn start_for_app(&self, attempt: &Attempt) -> Result<()> {
        attempt.begin(Phase::Preflight);
        let lock = self.lock()?;
        self.lifecycle.check()?;
        let cfg = self.config()?;
        let runtime = Runtime::new(&cfg);
        if runtime.running() {
            if cfg!(windows)
                && let Ok(qemu) = crate::vm::resolve_qemu(&cfg)
            {
                crate::accel::restore_start_fallback(&cfg.home, Path::new(&qemu.qemu_bin));
            }
            attempt.machine_facts(&cfg);
            attempt.follow_serial(cfg.running_serial_log_path(), true);
            if let Some(pid) = runtime.pid() {
                attempt.add_fact("QEMU pid", pid.to_string());
            }
            attempt.complete(Phase::Preflight, "Found the running machine through QMP");
            for phase in [Phase::Install, Phase::Qemu, Phase::Boot, Phase::Guest] {
                attempt.skip(phase, "Attaching to the running machine");
            }
            return Ok(());
        }
        if runtime.alive() {
            return runtime.previous_vm_error();
        }
        let cfg = if let Some(request) = crate::resources::pending(&cfg)? {
            if self.storage_in_use() {
                return crate::message("Stop the previous QEMU process before applying machine resources");
            }
            attempt.note("Applying saved machine resources before boot");
            crate::resources::apply_stopped(cfg, request)?
        } else {
            cfg
        };
        let runner = Runner::new(cfg.clone())?.with_agentd(&self.agentd_binary);
        runner.apply_for_start_locked(&lock, attempt)?;
        attempt.machine_facts(&cfg);
        runner.start_for_app_locked(&lock, &Runtime::new(&cfg), attempt)
    }

    /// The stable instance paths still identify its QEMU when metadata is unreadable.
    fn stop_without_config(&self, instance: &InstancePaths, timeout: Duration) -> Result<()> {
        let Some(pid) = recorded_qemu_pid(instance, NAME)? else {
            return Ok(());
        };
        if !crate::vm::stop_process(
            pid,
            instance,
            NAME,
            timeout,
            |_| false,
            || {},
        ) {
            return crate::util::message("The machine could not be stopped; its files were kept");
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<MachineInfo> {
        let lock = self.lock()?;
        if let Ok(cfg) = self.config() {
            Runner::new(cfg.clone())?
                .with_agentd(&self.agentd_binary)
                .stop_vm_locked(&lock)?;
            let _ = drop_legacy_snapshots(&cfg);
        } else {
            let paths = StatePaths::new(&self.home);
            let instance = paths.instance(NAME);
            reject_symlink(&paths.instances())?;
            reject_symlink(instance.root())?;
            self.stop_without_config(&instance, Duration::from_secs(12))?;
        }
        drop(lock);
        Ok(self.info())
    }

    /// Stop the machine and remove all app-owned machine data, including
    /// cached images, logs and network settings. Keep the lock inode while
    /// holding it so another process cannot start a concurrent operation.
    pub fn destroy(&self) -> Result<MachineInfo> {
        reject_symlink(&self.home)?;
        if let Ok(root) = self.home.canonicalize()
            && (root.parent().is_none() || std::env::home_dir().is_some_and(|home| {
                home.canonicalize().is_ok_and(|home| home == root)
            }))
        {
            return crate::util::message("Refusing to erase app data from the system root or user home directory");
        }
        let lock = self.lock()?;
        let paths = StatePaths::new(&self.home);
        let instances = paths.instances();
        let instance = paths.instance(NAME);
        for path in [&self.home, &instances, instance.root()] {
            reject_symlink(path)?;
        }
        if instances.exists() {
            for entry in std::fs::read_dir(&instances)? {
                if entry?.file_name() != NAME {
                    return crate::util::message(
                        "Other virtual machine instances share this data directory; refusing to delete them",
                    );
                }
            }
        }
        reject_symlink(&self.home.join("state"))?;
        if let Ok(cfg) = self.config() {
            let runtime = Runtime::new(&cfg);
            if runtime.alive() {
                Runner::new(cfg.clone())?.stop_vm_locked(&lock)?;
            }
            crate::chrome_viewer::stop(&cfg)?;
            crate::preview_forward::stop(&cfg)?;
        } else {
            self.stop_without_config(&instance, Duration::from_secs(12))?;
        }
        if recorded_qemu_pid(&instance, NAME)?.is_some() {
            return crate::util::message("A recorded machine process still exists; the disk and files were kept");
        }
        let proxy = crate::proxy::stop(&self.home, false)?;
        if proxy.running {
            return crate::util::message("The proxy could not be stopped; the machine disk was kept");
        }
        // Remove only paths owned by the app. The data root may be shared
        // with user files, and other processes need the same lock inode.
        for path in [
            paths.instances(), paths.images(), paths.engines(), paths.logs(), paths.tmp(),
            paths.legacy_journal(), paths.global_state(), paths.proxy(),
            self.home.join("network"), self.home.join("sync"), self.home.join("destroy.log"),
        ] {
            crate::util::remove_path(&path).map_err(|error| crate::Error::Message(format!(
                "Could not remove {}: {error}. Retry the reset to finish cleanup.", path.display()
            )))?;
        }
        drop(lock);
        Ok(self.info())
    }

    /// Stream-friendly status of each provisioning step.
    pub fn steps(&self) -> Result<Value> {
        Runner::new(self.config()?)?.status()
    }
}

/// Why `name` cannot be the VM's Linux user (same rules as the UI).
/// Remove what versions with disk snapshots left behind: the qcow2 internal
/// snapshots (only while the VM is stopped) and the Chrome recovery record.
fn drop_legacy_snapshots(cfg: &Config) -> Result<()> {
    let _ = std::fs::remove_file(crate::chrome_feature::legacy_transaction_path(cfg));
    if !cfg.disk_path().exists() || Runtime::new(cfg).alive() {
        return Ok(());
    }
    crate::steps::base::purge_legacy_snapshots(cfg)
}

pub fn username_problem(name: &str) -> Option<&'static str> {
    let mut chars = name.chars();
    let first = chars.next()?;
    if name.len() > 32 {
        return Some("use at most 32 characters");
    }
    if !(first.is_ascii_lowercase() || first == '_') {
        return Some("start with a lowercase letter or _");
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Some("use only lowercase letters, digits, - and _");
    }
    if name == "root" {
        return Some("root is reserved; choose a normal user name");
    }
    None
}

/// Why `name` cannot be the VM's hostname (one RFC 1123 label).
pub fn hostname_problem(name: &str) -> Option<&'static str> {
    if name.len() > 63 {
        return Some("use at most 63 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Some("use only lowercase letters, digits and -");
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Some("cannot start or end with -");
    }
    None
}

fn reject_symlink(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => crate::util::message(format!(
            "Refusing to destroy a machine through a symlink: {}", path.display()
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

struct ClearInstalling<'a>(&'a Machine);

impl Drop for ClearInstalling<'_> {
    fn drop(&mut self) {
        let mut installing = self.0.installing.locked();
        *installing = None;
        self.0.lifecycle.set_cancelled(false);
    }
}

struct StopPoller(Arc<AtomicBool>);

impl Drop for StopPoller {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process that stays alive for the duration of these checks.
    fn long_sleeping_child() -> std::process::Child {
        let mut command = long_sleeping_command();
        crate::util::no_window(&mut command);
        command.spawn().unwrap()
    }

    fn long_sleeping_command() -> std::process::Command {
        #[cfg(unix)]
        {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            command
        }
        #[cfg(windows)]
        {
            let mut command = std::process::Command::new("ping");
            command.args(["-n", "31", "127.0.0.1"]);
            command
        }
    }

    #[test]
    fn quit_cancels_install_and_prevents_a_later_boot_even_with_operation_locked() {
        // This path cancels all in-process downloads. Use a child test so
        // concurrent download tests in the parent keep their own flag.
        const CHILD: &str = "CABIN_R2_TEST_QUIT_DURING_INSTALL";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "machine::tests::quit_cancels_install_and_prevents_a_later_boot_even_with_operation_locked"])
                .env(CHILD, "1")
                .stdout(std::process::Stdio::null())
                .status().unwrap();
            assert!(status.success());
            return;
        }
        let home = crate::sockets::test_dir();
        let machine = Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        let cfg = machine.install_config(&InstallOptions::default()).unwrap();
        let lock = machine.lock().unwrap();
        let install = machine.begin_install(&cfg).unwrap();
        machine.shutdown(Duration::from_secs(4)).unwrap();
        drop(install);
        let runtime = Runtime::new(&cfg);
        let log = Arc::new(std::sync::Mutex::new(
            std::fs::File::create(home.join("start.log")).unwrap(),
        ));
        let err = runtime
            .start(&log, crate::vm::Boot::new(Duration::from_secs(1)))
            .unwrap_err();
        assert_eq!(err.to_string(), "Cabin is quitting");
        assert!(!cfg.pidfile().exists());
        drop(log);
        drop(lock);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn destroy_removes_machine_cache_and_settings_but_not_shared_files() {
        let base = std::env::temp_dir().join(format!("vm-destroy-test-{}", std::process::id()));
        let home = base.join("vm");
        let shared = base.join("shared");
        std::fs::create_dir_all(home.join("instances/agent")).unwrap();
        std::fs::create_dir_all(home.join("images")).unwrap();
        std::fs::create_dir_all(home.join("network")).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(home.join("instances/agent/disk.qcow2"), "disk").unwrap();
        std::fs::write(home.join("images/image.img"), "image").unwrap();
        std::fs::write(home.join("network/config"), "secret").unwrap();
        std::fs::write(shared.join("important"), "keep").unwrap();
        std::fs::write(home.join("personal.txt"), "keep me").unwrap();
        let machine = Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        assert!(!machine.destroy().unwrap().installed);
        assert!(!home.join("instances").exists());
        assert!(!home.join("images").exists());
        assert!(!home.join("network").exists());
        assert_eq!(std::fs::read_to_string(home.join("personal.txt")).unwrap(), "keep me");
        assert_eq!(
            std::fs::read_to_string(shared.join("important")).unwrap(),
            "keep"
        );
        // A clean destroy is idempotent and leaves the same usable lock.
        assert!(!machine.destroy().unwrap().installed);
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn stale_legacy_pid_is_ignored_but_other_instances_still_block_reset() {
        let base = std::env::temp_dir().join(format!("vm-destroy-safety-{}", std::process::id()));
        let home = base.join("vm");
        std::fs::create_dir_all(home.join("instances/agent")).unwrap();
        let mut child = long_sleeping_child();
        std::fs::write(
            home.join("instances/agent/qemu.pid"),
            child.id().to_string(),
        )
        .unwrap();
        let machine = Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        assert!(machine.stop().is_ok());
        assert!(machine.destroy().is_ok());
        assert!(!home.join("instances/agent/qemu.pid").exists());
        assert!(child.try_wait().unwrap().is_none());
        let _ = child.kill();
        let _ = child.wait();
        std::fs::create_dir_all(home.join("instances/another-vm")).unwrap();
        assert!(machine.destroy().is_err());
        assert!(home.join("instances/another-vm").exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn destroy_never_signals_an_unrelated_process_in_a_stale_pidfile() {
        let home = std::env::temp_dir().join(format!("vm-destroy-pid-{}", std::process::id()));
        let machine = Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        let cfg = machine.install_config(&InstallOptions::default()).unwrap();
        std::fs::create_dir_all(cfg.instance_dir()).unwrap();
        cfg.save_instance_meta().unwrap();
        let mut child = long_sleeping_child();
        std::fs::write(cfg.pidfile(), child.id().to_string()).unwrap();
        assert!(machine.destroy().is_ok());
        assert!(
            child.try_wait().unwrap().is_none(),
            "unrelated process was terminated"
        );
        assert!(!cfg.instance_meta_path().exists());
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn a_damaged_install_preserves_its_disk_until_an_explicit_reset() {
        let home = std::env::temp_dir().join(format!("vm-damaged-install-{}-{}", std::process::id(), crate::util::now_millis()));
        let machine = Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        let instance = StatePaths::new(&home).instance(NAME);
        std::fs::create_dir_all(instance.root()).unwrap();
        std::fs::write(instance.disk(), "workspace data").unwrap();
        for damaged_meta in [false, true] {
            if damaged_meta {
                std::fs::write(instance.meta(), "{broken").unwrap();
            }
            assert!(machine.info().installed);
            assert!(machine.install(&InstallOptions::default()).unwrap_err().to_string().contains("files to recover"));
            assert_eq!(std::fs::read_to_string(instance.disk()).unwrap(), "workspace data");
        }
        use crate::steps::base::{Step, StepContext};
        let cfg = machine.install_config(&InstallOptions::default()).unwrap();
        let mut disk = StepContext::new(&cfg, "disk", "test").unwrap();
        assert!(crate::steps::disk::DiskStep.apply(&mut disk).is_err());
        let mut provision = StepContext::new(&cfg, "provision", "test").unwrap();
        assert!(crate::steps::provision::ProvisionStep.rollback(&mut provision, &Value::Null, false).is_err());
        assert_eq!(std::fs::read_to_string(instance.disk()).unwrap(), "workspace data");
        assert!(!machine.destroy().unwrap().installed);
        assert!(!instance.disk().exists());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn destroy_refuses_a_symlinked_data_directory() {
        let base = std::env::temp_dir().join(format!("vm-destroy-link-{}", std::process::id()));
        let target = base.join("target");
        let link = base.join("vm");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("important"), "keep").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let machine = Machine::new(&link, env!("CARGO_MANIFEST_DIR"));
        assert!(machine.destroy().is_err());
        assert_eq!(
            std::fs::read_to_string(target.join("important")).unwrap(),
            "keep"
        );
        assert!(!target.join("lock").exists());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn uninstalled_machine_reports_not_installed() {
        let dir = std::env::temp_dir().join(format!("vm-machine-test-{}", std::process::id()));
        let machine = Machine::new(&dir, env!("CARGO_MANIFEST_DIR"));
        let info = machine.info();
        assert!(!info.installed && !info.running);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn username_and_hostname_rules() {
        assert_eq!(username_problem("dev"), None);
        assert_eq!(username_problem("_svc-1"), None);
        assert!(username_problem("Dev").is_some());
        assert!(username_problem("1dev").is_some());
        assert!(username_problem("root").is_some());
        assert!(username_problem("dev.user").is_some());
        assert!(username_problem(&"a".repeat(33)).is_some());
        assert_eq!(hostname_problem("agent-vm"), None);
        assert!(hostname_problem("-vm").is_some());
        assert!(hostname_problem("my_vm").is_some());
        assert!(hostname_problem("VM").is_some());
        let options = InstallOptions {
            username: Some("root".into()),
            ..Default::default()
        };
        assert!(Machine::validate(&options).is_err());
    }

    #[test]
    fn install_options_map_to_config_overrides() {
        let home = std::env::temp_dir().join(format!("vm-install-options-{}", std::process::id()));
        let machine = Machine::new(&home, env!("CARGO_MANIFEST_DIR"));
        if cfg!(target_os = "macos") {
            let engine = crate::qemu_engine::root(&home);
            for file in crate::arch::GuestArch::current().engine_files() {
                let path = engine.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, []).unwrap();
            }
        }
        let overrides = machine.install_overrides(&InstallOptions {
            cpus: Some(6),
            device: true,
            ..InstallOptions::default()
        });
        assert_eq!(overrides["name"], NAME);
        assert_eq!(overrides["cpus"], 6);
        // Chrome is not an installer option; it is added from the Chrome page.
        assert!(overrides.get("desktop_env").is_none());
        assert!(overrides.get("desktop_chrome").is_none());
        assert!(overrides.get("browser").is_none());
        assert_eq!(overrides["device"]["enabled"], true);
        // The overrides must produce a valid config with the shipped defaults.
        let cfg = build_config(&overrides, Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(cfg.cpus, 6);
        assert_eq!(cfg.ssh_port, 12233);
        assert!(cfg.device_enabled() && !cfg.browser_enabled() && !cfg.desktop_chrome);
        let resolved = cfg.device_resolved().unwrap();
        let plan = crate::device::render::network_plan(&resolved, Some(&cfg.hostname)).unwrap();
        assert!(plan["ethernets"].get("mgmt0").is_none(), "one NIC");
        assert_eq!(
            plan["ethernets"]["data0"]["match"]["macaddress"],
            cfg.mac_data()
        );
        // The QEMU argv mapping needs a resolvable QEMU (the macOS fake
        // engine above, or the distro QEMU on Linux/Windows dev hosts).
        if crate::qemu_engine::installed(&home).is_some() {
            let args = crate::device::render::network_args(&cfg, &resolved).unwrap();
            assert!(!args.iter().any(|arg| arg.starts_with("user,")));
            assert!(args.iter().any(|arg| arg.contains("stream,id=data")));
        }
        assert_eq!(
            crate::device::render::ssh_endpoint(&cfg, Some(&resolved)).unwrap(),
            ("127.0.0.1".into(), cfg.ssh_port)
        );

        let resolute = machine.install_overrides(&InstallOptions {
            os: Some("ubuntu-26.04".into()),
            ..InstallOptions::default()
        });
        let cfg = build_config(&resolute, Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(cfg.os, "ubuntu-26.04");
        assert!(
            !cfg.image_pinned(),
            "the build is pinned when the install starts"
        );
        assert_eq!(
            machine.plan(&InstallOptions::default()).unwrap().ssh_port,
            12233
        );

        assert!(
            Machine::validate(&InstallOptions {
                os: Some("debian-12".into()),
                ..InstallOptions::default()
            })
            .is_err()
        );
        assert!(
            Machine::validate(&InstallOptions {
                os: Some("fedora-41".into()),
                ..InstallOptions::default()
            })
            .is_err()
        );
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn step_state_accepts_millisecond_numbers_and_legacy_iso_strings() {
        let ms = 1_900_000_000_000;
        assert_eq!(
            state_millis(Some(&json!({ "finished_at": ms })), "finished_at"),
            Some(ms)
        );
        let legacy = crate::util::utcstamp_from_millis(ms);
        assert_eq!(
            state_millis(
                Some(&json!({ "rolled_back_at": legacy })),
                "rolled_back_at"
            ),
            Some(ms)
        );
    }

    #[test]
    fn step_log_age_reads_millisecond_and_legacy_headers() {
        let dir = std::env::temp_dir().join(format!("vm-log-age-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("disk-apply-1.log");
        let started = crate::util::now_millis() - 5_000;
        std::fs::write(&path, format!("# step=disk action=apply at={started}\n")).unwrap();
        let age = log_age_seconds(&path).unwrap();
        assert!((3..=10).contains(&age), "unexpected age {age}");
        let legacy = crate::util::utcstamp_of(
            std::time::SystemTime::now() - std::time::Duration::from_secs(5),
        );
        std::fs::write(&path, format!("# step=disk action=apply at={legacy}\n")).unwrap();
        let age = log_age_seconds(&path).unwrap();
        assert!((3..=10).contains(&age), "unexpected legacy age {age}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
