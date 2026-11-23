//! The Mac side: owns the VM (through `vm_core::machine`) and a live link to
//! `cabind` inside it. UI commands for workspaces, sessions, agents and
//! the environment are forwarded to the guest unchanged; guest events are
//! pushed to the window together with machine events.

use agent_core::util::{LockExt, RwLockExt};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use agent_core::model::{
    AppEvent, AppSnapshot, BrowserEnvironmentProbe, BrowserEnvironmentView, BrowserSettings, GuestConnection, GuestNetworkProbe,
    HostSnapshot, InstallPlan, InstallStep, MachineInstallOptions, MachineState, MachineStatus,
    MirrorResult, NetworkCheckReport, NetworkConnectivity, NetworkGateMode,
    NetworkRoute, NetworkStatus, StartReport, StartState,
};
use agent_core::util::now_millis;
use serde_json::{Value, json};
use vm_core::machine::{InstallOptions, Machine, MachineInfo};
use vm_core::startup::{Attempt, Phase};

use crate::app_error::AppError;
use crate::global_network::{GlobalNetwork, RollbackPoint};
use crate::network_gate::{CheckInputs, evaluate_report, route_revision};
use crate::network_health::{self, HoldCheck};

type Sink = Arc<dyn Fn(AppEvent) + Send + Sync>;
type Reply = mpsc::Sender<Result<Value, AppError>>;

struct Link {
    stdin: ChildStdin,
    child: Child,
}

/// An open `cabind connect` link: the SSH child, its stdin, its stdout
/// lines and the daemon's hello.
type GuestLink = (Child, ChildStdin, std::io::Lines<BufReader<std::process::ChildStdout>>, Value);

/// Why a link could not be opened, and whether the guest answered at all.
struct LinkFailure {
    reason: String,
    reachable: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct GuestBuild {
    generation: u64,
    pid: Option<u32>,
    build: String,
}

#[derive(Default)]
struct Deployment(Option<GuestBuild>);

impl Deployment {
    fn claim(&mut self, guest: &GuestBuild) -> bool {
        if self.0.as_ref() == Some(guest) {
            return false;
        }
        self.0 = Some(guest.clone());
        true
    }

    fn ready(&mut self, guest: &GuestBuild) -> bool {
        if self.0.as_ref() == Some(guest) {
            self.0 = None;
            return true;
        }
        false
    }
}

pub struct Host {
    machine: Machine,
    /// Mac folders kept in step with folders in the machine.
    pub sync: Arc<crate::sync::SyncManager>,
    network: GlobalNetwork,
    agentd: PathBuf,
    /// SHA-256 of the bundled cabind, keyed by its size and mtime so a
    /// rebuild during development is noticed. Hashed once, off the start path.
    agentd_build: Mutex<Option<(u64, std::time::SystemTime, String)>>,
    deployment: Mutex<Deployment>,
    status: RwLock<MachineStatus>,
    sink: RwLock<Option<Sink>>,
    link: Mutex<Option<Link>>,
    pending: Mutex<HashMap<u64, Reply>>,
    next_id: AtomicU64,
    generation: AtomicU64,
    guest_snapshot: RwLock<Option<AppSnapshot>>,
    /// The current attempt and last published report, ordered together.
    startup: Mutex<Option<(Arc<Attempt>, StartReport)>>,
    stopping: AtomicBool,
    cancel_requested: AtomicBool,
    /// Bumped to wake the connection loop early (after start/stop).
    kick: Mutex<mpsc::Sender<()>>,
    kick_rx: Mutex<Option<mpsc::Receiver<()>>>,
}

fn unavailable(message: impl Into<String>) -> AppError {
    AppError {
        code: "unavailable".into(),
        message: message.into(),
        details: None,
    }
}

/// Start the machine's gateway and record the SSH port it actually got
/// (another program may hold the configured one), so SSH follows it.
fn start_gateway(
    network: &GlobalNetwork,
    cfg: &vm_core::config::Config,
    route: Option<&NetworkRoute>,
) -> Result<(), AppError> {
    vm_core::vm::validate_socket_paths(cfg).map_err(Host::vm_error)?;
    match route {
        Some(route) => network.start_with(&cfg.net_socket(), cfg.ssh_port, route)?,
        None => network.start(&cfg.net_socket(), cfg.ssh_port)?,
    }
    if let Err(error) = cfg.set_ssh_relay_port(network.ssh_port()) {
        eprintln!("cabin: could not record the SSH relay port: {error}");
    }
    Ok(())
}

/// First boot needs package downloads before the normal internet gate opens.
/// Keep that exception tied to the install instead of changing the route.
fn start_install_gateway(
    network: &GlobalNetwork,
    cfg: &vm_core::config::Config,
) -> Result<bool, AppError> {
    if !cfg.isolated() {
        return Ok(false);
    }
    let _transaction = network.transaction.locked();
    if network.is_up() {
        network.switch(&network.route()?)?;
    }
    network.begin_install();
    if let Err(error) = start_gateway(network, cfg, None) {
        network.end_install();
        return Err(error);
    }
    Ok(true)
}

fn failure_summary(report: &NetworkCheckReport) -> String {
    let failed = report
        .checks
        .iter()
        .filter(|check| {
            check.required && check.status != agent_core::model::NetworkCheckStatus::Pass
        })
        .map(|check| check.title.as_str())
        .collect::<Vec<_>>();
    if failed.is_empty() {
        "The network did not pass its complete check".into()
    } else {
        format!("The network check did not pass: {}", failed.join(", "))
    }
}

impl Host {
    pub fn new(machine: Machine, agentd: PathBuf) -> Result<Arc<Self>, AppError> {
        let (kick, kick_rx) = mpsc::channel();
        let network = GlobalNetwork::open(&machine.home().join("network"))?;
        let sync = crate::sync::SyncManager::open(machine.home().join("sync"));
        let host = Arc::new(Self {
            sync,
            machine,
            network,
            agentd,
            agentd_build: Mutex::new(None),
            deployment: Mutex::default(),
            status: RwLock::new(MachineStatus {
                state: MachineState::NotInstalled,
                connection: GuestConnection::Offline,
                busy: None,
                ssh_port: None,
                cpus: None,
                memory_mb: None,
                disk_size: None,
                display: false,
                os: None,
                browser: false,
                device: false,
                guest_version: None,
                username: None,
                hostname: None,
                error: None,
            }),
            sink: RwLock::new(None),
            link: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            guest_snapshot: RwLock::new(None),
            startup: Mutex::new(None),
            stopping: AtomicBool::new(false),
            cancel_requested: AtomicBool::new(false),
            kick: Mutex::new(kick),
            kick_rx: Mutex::new(Some(kick_rx)),
        });
        host.set_machine_info(host.machine.configured_info(false));
        host.start_network_recovery();
        {
            let me = Arc::downgrade(&host);
            host.sync.set_sink(move |event| {
                if let Some(me) = me.upgrade() {
                    me.emit(event);
                }
            });
        }
        {
            // Fingerprint the bundled daemon while QEMU boots.
            let me = Arc::downgrade(&host);
            std::thread::spawn(move || {
                if let Some(me) = me.upgrade() {
                    let _ = me.bundled_build();
                }
            });
        }
        Ok(host)
    }

    // ---------------------------------------------------------- events

    pub fn set_sink(&self, sink: impl Fn(AppEvent) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }

    fn emit(&self, event: AppEvent) {
        let sink = self.sink.read_lock().clone();
        if let Some(sink) = sink {
            sink(event);
        }
    }

    fn uses_gateway(&self) -> bool {
        self.machine.config().is_ok_and(|cfg| cfg.isolated())
    }

    fn update(&self, change: impl FnOnce(&mut MachineStatus)) {
        let status = {
            let mut status = self.status.write_lock();
            change(&mut status);
            status.clone()
        };
        self.emit(AppEvent::Machine { status });
    }

    fn claim_machine(&self, label: &str, connecting: bool) -> Result<MachineStatus, AppError> {
        let mut status = self.status.write().unwrap_or_else(|p| p.into_inner());
        if self.stopping.load(Ordering::Acquire) {
            return Err(unavailable("Cabin is quitting"));
        }
        if status.busy.is_some() {
            return Err(unavailable("A machine operation is already in progress"));
        }
        status.busy = Some(label.into());
        status.error = None;
        if connecting {
            status.connection = GuestConnection::Connecting;
        }
        Ok(status.clone())
    }

    fn begin_start(self: &Arc<Self>) -> Result<Arc<Attempt>, AppError> {
        let attempt = Arc::new(Attempt::default());
        attempt.begin(Phase::Gateway);
        let report: StartReport = Self::convert(attempt.snapshot())?;
        let status = {
            let mut startup = self.startup.locked();
            if startup.as_ref().is_some_and(|(current, _)| current.running()) {
                return Err(unavailable("A machine operation is already in progress"));
            }
            let status = self.claim_machine("Starting the machine", true)?;
            self.generation.fetch_add(1, Ordering::SeqCst);
            *startup = Some((attempt.clone(), report.clone()));
            status
        };
        self.emit(AppEvent::Start { report: Box::new(report) });
        self.emit(AppEvent::Machine { status });
        let me = Arc::downgrade(self);
        let hot = attempt.clone();
        std::thread::spawn(move || {
            while hot.running() {
                std::thread::sleep(Duration::from_millis(400));
                if let Some(me) = me.upgrade() {
                    me.publish_start(&hot);
                } else {
                    break;
                }
            }
        });
        Ok(attempt)
    }

    fn publish_start(&self, attempt: &Arc<Attempt>) {
        let mut startup = self.startup.locked();
        let Some((current, last)) = startup.as_mut() else {
            return;
        };
        if Arc::ptr_eq(current, attempt)
            && last.state == StartState::Running
            && let Ok(report) = Self::convert(attempt.snapshot())
            && *last != report
        {
            *last = report.clone();
            self.emit(AppEvent::Start {
                report: Box::new(report),
            });
        }
    }

    fn active_start(&self) -> Option<Arc<Attempt>> {
        self.startup.locked()
            .as_ref()
            .filter(|(attempt, _)| attempt.running())
            .map(|(attempt, _)| attempt.clone())
    }

    fn start_state(&self) -> Option<StartState> {
        self.startup.locked()
            .as_ref()
            .map(|(_, report)| report.state)
    }

    fn finish_start(&self, attempt: &Arc<Attempt>, error: Option<String>) {
        attempt.finish(error);
        self.publish_start(attempt);
    }

    fn wake(&self) {
        let _ = self.kick.locked().send(());
    }

    /// Re-read the VM's configuration and run state.
    pub fn machine_details(&self) -> Result<Option<vm_core::machine_details::MachineDetails>, AppError> {
        self.machine.details().map_err(Self::vm_error)
    }

    pub fn storage(&self, identifier: &str) -> crate::storage::StorageReport {
        let blocked = if self.machine.storage_in_use() {
            Some("Stop the machine to clean up storage".into())
        } else if self.sync.overlaps_host_path(self.machine.home()) {
            Some("A synced Mac folder overlaps the app data directory".into())
        } else {
            None
        };
        crate::storage::StorageReport::read(&self.machine, identifier, blocked)
    }

    pub fn clean_storage(&self, target: vm_core::storage::CleanupTarget) -> Result<vm_core::storage::CleanupResult, AppError> {
        self.busy("Cleaning storage", || {
            if self.sync.overlaps_host_path(self.machine.home()) {
                return vm_core::message("A synced Mac folder overlaps the app data directory; its files were kept");
            }
            self.machine.clean_storage(target)
        })
    }

    pub fn refresh_machine(&self) {
        self.set_machine_info(self.machine.info());
    }

    fn set_machine_info(&self, info: MachineInfo) {
        self.update(|status| {
            status.state = match (info.installed, info.running) {
                (false, _) => MachineState::NotInstalled,
                (true, false) => MachineState::Stopped,
                (true, true) => MachineState::Running,
            };
            status.ssh_port = info.ssh_port;
            status.cpus = info.cpus;
            status.memory_mb = info.memory_mb;
            status.disk_size = info.disk_size.clone();
            status.display = info.desktop;
            status.os = info.os.clone();
            status.browser = info.browser;
            status.device = info.device;
            status.username = info.username.clone();
            status.hostname = info.hostname.clone();
        });
    }

    pub fn snapshot(&self) -> HostSnapshot {
        let machine = self
            .status.read_lock()
            .clone();
        let mut guest = self
            .guest_snapshot.read_lock()
            .clone();
        if let Some(snapshot) = &mut guest {
            self.overlay_snapshot(snapshot);
        }
        let start = self
            .startup.locked()
            .as_ref()
            .map(|(_, report)| report.clone());
        HostSnapshot {
            machine,
            guest,
            start,
        }
    }

    // ------------------------------------------------------ machine ops

    fn busy<T>(
        &self,
        label: &str,
        work: impl FnOnce() -> vm_core::Result<T>,
    ) -> Result<T, AppError> {
        let status = {
            let startup = self.startup.lock().unwrap_or_else(|p| p.into_inner());
            if startup.as_ref().is_some_and(|(current, _)| current.running()) {
                return Err(unavailable("Wait for the machine startup attempt to finish"));
            }
            self.claim_machine(label, false)?
        };
        self.emit(AppEvent::Machine { status });
        let result = work();
        self.refresh_machine();
        self.update(|status| {
            status.busy = None;
            if let Err(error) = &result {
                status.error = Some(error.to_string());
            }
        });
        self.wake();
        result.map_err(|error| AppError {
            code: "machine".into(),
            message: error.to_string(),
            details: None,
        })
    }

    fn vm_options(options: MachineInstallOptions) -> InstallOptions {
        InstallOptions {
            cpus: options.cpus,
            memory_mb: options.memory_mb,
            disk_size: options.disk_size,
            username: options.username,
            hostname: options.hostname,
            device: options.device,
            proxy: None,
            apt_mirror: options.apt_mirror,
            os: options.os,
        }
    }

    fn convert<A: serde::Serialize, B: serde::de::DeserializeOwned>(
        value: A,
    ) -> Result<B, AppError> {
        serde_json::to_value(value)
            .and_then(serde_json::from_value)
            .map_err(|error| AppError::internal(error.to_string()))
    }

    fn vm_error(error: vm_core::Error) -> AppError {
        AppError {
            code: "machine".into(),
            message: error.to_string(),
            details: None,
        }
    }

    /// Test the chosen OS's ARM64 mirrors (its defaults plus any extra URIs).
    pub fn test_mirrors(
        &self,
        os: Option<String>,
        extra: Vec<String>,
    ) -> Result<Vec<MirrorResult>, AppError> {
        let os = vm_core::os::get(os.as_deref().unwrap_or(vm_core::os::DEFAULT))
            .map_err(Self::vm_error)?;
        let mut uris: Vec<String> = os.mirrors().iter().map(|s| s.to_string()).collect();
        for uri in extra {
            let uri = uri.trim().trim_end_matches('/').to_string();
            if !uri.is_empty() && !uris.contains(&uri) {
                uris.push(uri);
            }
        }
        Self::convert(vm_core::mirror::test_mirrors(os, &uris))
    }

    /// Check every step without changing anything.
    pub fn verify(&self) -> Result<Vec<InstallStep>, AppError> {
        Self::convert(self.machine.verify().map_err(Self::vm_error)?)
    }

    /// Re-apply missing steps and start the VM, optionally with a new mirror.
    pub fn resume(
        self: &Arc<Self>,
        change_mirror: bool,
        apt_mirror: Option<String>,
    ) -> Result<MachineStatus, AppError> {
        let me = self.clone();
        self.busy("Installing the machine", || {
            let cfg = self.machine.config()?;
            if vm_core::vm::Runtime::new(&cfg).alive() {
                return vm_core::message("Stop the machine before repairing its installation");
            }
            let install_network = start_install_gateway(&self.network, &cfg)
                .map_err(|error| vm_core::Error::Message(error.message))?;
            let result = self.machine
                .resume_with_progress(change_mirror.then_some(apt_mirror), move |steps| {
                    if let Ok(steps) = Self::convert(steps) {
                        me.emit(AppEvent::Install { steps });
                    }
                });
            if install_network {
                self.network.end_install();
            }
            if result.is_ok() && self.start_state() == Some(StartState::Failed) {
                *self.startup.locked() = None;
            }
            result
        })?;
        Ok(self.snapshot().machine)
    }

    pub fn plan(&self, options: MachineInstallOptions) -> Result<InstallPlan, AppError> {
        let plan = self
            .machine
            .plan(&Self::vm_options(options))
            .map_err(|error| AppError {
                code: "invalid".into(),
                message: error.to_string(),
                details: None,
            })?;
        serde_json::from_value(serde_json::to_value(plan).unwrap_or_default())
            .map_err(|error| AppError::internal(error.to_string()))
    }

    /// Stop the running install; it rolls back and reports "cancelled".
    pub fn cancel_install(&self) -> Result<(), AppError> {
        self.cancel_requested.store(true, Ordering::SeqCst);
        if !self.machine.abort_install() {
            self.cancel_requested.store(false, Ordering::SeqCst);
            return Err(unavailable("No install is running"));
        }
        self.update(|status| status.busy = Some("Cancelling the install".into()));
        Ok(())
    }

    pub fn install(
        self: &Arc<Self>,
        options: MachineInstallOptions,
    ) -> Result<MachineStatus, AppError> {
        let options = Self::vm_options(options);
        let me = self.clone();
        let mut cancelled = false;
        let result = self.busy("Installing the machine", || {
            if self.machine.installed() {
                return vm_core::util::message("The machine is already installed or has files to recover");
            }
            self.cancel_requested.store(false, Ordering::SeqCst);
            self.network.reopen_store().map_err(|error| vm_core::Error::Message(error.message))?;
            let cfg = self.machine.install_config(&options)?;
            let install_network = start_install_gateway(&self.network, &cfg)
                .map_err(|error| vm_core::Error::Message(error.message))?;
            let result = self.machine.install_with_progress(&options, move |steps| {
                if let Ok(steps) =
                    serde_json::from_value(serde_json::to_value(steps).unwrap_or_default())
                {
                    me.emit(AppEvent::Install { steps });
                }
            });
            if install_network {
                self.network.end_install();
            }
            if result.is_err() && !self.machine.info().running {
                self.network.stop();
            }
            cancelled = result.is_err() && self.cancel_requested.swap(false, Ordering::SeqCst)
                && !self.machine.installed();
            result
        });
        if cancelled {
            return Err(AppError {
                code: "cancelled".into(),
                message: "Install cancelled. The partial machine was removed; the downloaded image is kept.".into(),
                details: None,
            });
        }
        result?;
        Ok(self.snapshot().machine)
    }

    pub fn start(self: &Arc<Self>) -> Result<MachineStatus, AppError> {
        let status = self
            .status.read_lock()
            .clone();
        if status.state == MachineState::Running && status.connection == GuestConnection::Connected
        {
            return Ok(status);
        }
        if !self.machine.installed() {
            return Err(unavailable("Install the machine first"));
        }
        let attempt = self.begin_start()?;
        self.start_machine(&attempt)?;
        Ok(self
            .status.read_lock()
            .clone())
    }

    fn start_machine(&self, attempt: &Arc<Attempt>) -> Result<(), AppError> {
        let result: Result<(), AppError> = (|| {
            if self.stopping.load(Ordering::Acquire) {
                return Err(unavailable("Cabin is quitting"));
            }
            self.machine
                .open_start_log(attempt)
                .map_err(Self::vm_error)?;
            let cfg = self.machine.config().map_err(Self::vm_error)?;
            if cfg.isolated() {
                if !self.network.is_up() {
                    start_gateway(&self.network, &cfg, None)?;
                }
                attempt.complete(Phase::Gateway, "The gateway and SSH relay are listening");
            } else {
                attempt.skip(Phase::Gateway, "This machine does not use the gateway");
            }
            self.publish_start(attempt);
            self.machine.start_for_app(attempt).map_err(Self::vm_error)
        })();
        match &result {
            Ok(()) => {
                self.set_machine_info(self.machine.configured_info(true));
                attempt.begin(Phase::Daemon);
                self.publish_start(attempt);
            }
            Err(error) => {
                self.finish_start(attempt, Some(error.message.clone()));
                self.refresh_machine();
                if self.status.read_lock().state
                    != MachineState::Running
                {
                    self.network.stop();
                }
            }
        }
        self.update(|status| {
            status.busy = None;
            if let Err(error) = &result {
                status.connection = GuestConnection::Offline;
                status.error = Some(error.message.clone());
            }
        });
        self.wake();
        result
    }

    pub fn stop(&self) -> Result<MachineStatus, AppError> {
        if self.active_start().is_some() {
            return Err(unavailable(
                "Wait for the machine startup attempt to finish",
            ));
        }
        self.busy("Stopping the machine", || {
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.update(|status| status.connection = GuestConnection::Offline);
            self.sync.disconnect();
            self.disconnect();
            self.fail_pending("The machine is stopping");
            let info = self.machine.stop()?;
            self.network.stop();
            Ok(info)
        })?;
        Ok(self.snapshot().machine)
    }

    /// Chrome is host-managed because setup may need to stop and snapshot the VM.
    pub fn chrome_status(&self) -> Result<Value, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let running = self.machine.info().running;
        let current = self
            .status.read_lock()
            .clone();
        let busy = current.busy.is_some();
        let install = vm_core::chrome_feature::install_log_path(&cfg).map(|path| {
            let lines = std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            json!({
                "running": current.busy.as_deref() == Some("Installing Chrome"),
                "logPath": path.display().to_string(),
                "lines": lines,
            })
        });
        let configured = cfg.desktop_chrome && cfg.browser_enabled();
        let probe =
            if configured && running && !busy && current.connection == GuestConnection::Connected {
                Some(
                    vm_core::shell::browser_status(self.machine.home(), &cfg.name)
                        .map_err(Self::vm_error)?,
                )
            } else {
                None
            };
        Ok(json!({
            "configured": configured,
            "installed": probe.as_ref().and_then(|p| p.get("chrome_installed")),
            "running": probe.as_ref().and_then(|p| p.get("chrome_running")),
            "version": probe.as_ref().and_then(|p| p.get("chrome_version")),
            "viewerReady": probe.as_ref().and_then(|p| p.get("viewer_ready")),
            "displayProtocols": if cfg.desktop_spice { vec!["vnc", "spice"] } else { vec!["vnc"] },
            "canEnableSpice": running && !busy && current.connection == GuestConnection::Connected
                && vm_core::chrome_feature::can_enable_spice(&cfg),
            "install": install,
        }))
    }

    /// The browser environment panel's data. `check` also audits the guest
    /// (installed profile and live Chrome; bounded, a few seconds at most).
    pub fn browser_environment_view(&self, check: bool) -> Result<BrowserEnvironmentView, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let running = self.machine.info().running
            && self.status.read_lock().connection == GuestConnection::Connected;
        if !cfg.browser_enabled() {
            return Ok(BrowserEnvironmentView { enabled: false, machine_running: running, settings: None, probe: None, live_matches: None, checked_at: None });
        }
        let settings = self.machine.browser_settings().map_err(Self::vm_error)?;
        let settings = serde_json::to_value(settings)
            .and_then(serde_json::from_value::<BrowserSettings>)
            .map_err(|error| AppError::internal(error.to_string()))?;
        let probe = (check && running).then(|| Self::audit_browser(&cfg));
        let live_matches = probe.as_ref().and_then(crate::network_gate::live_matches);
        Ok(BrowserEnvironmentView {
            enabled: true,
            machine_running: running,
            settings: Some(settings),
            live_matches,
            checked_at: probe.as_ref().map(|_| now_millis()),
            probe,
        })
    }

    /// Save the browser settings and apply them to the running machine (live
    /// for a region change; otherwise the profile is written again and Chrome
    /// restarts). The environment revision changes, so the network gate
    /// rechecks on its own.
    pub fn browser_settings_save(&self, settings: BrowserSettings) -> Result<BrowserEnvironmentView, AppError> {
        let settings: vm_core::browser::Settings = serde_json::to_value(settings)
            .and_then(serde_json::from_value)
            .map_err(|error| AppError::internal(error.to_string()))?;
        self.machine.set_browser_settings(&settings).map_err(Self::vm_error)?;
        let _ = self.emit_network();
        self.browser_environment_view(true)
    }

    pub fn install_chrome(&self) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let install_network = start_install_gateway(&self.network, &cfg)?;
        let result = self.busy("Installing Chrome", || {
            self.disconnect();
            self.fail_pending("The machine is restarting to install Chrome");
            if let Ok(cfg) = self.machine.config() {
                let _ = vm_core::chrome_viewer::stop(&cfg);
            }
            self.machine.install_chrome()
        });
        if install_network {
            self.network.end_install();
        }
        result
    }

    pub fn chrome_open(&self, url: &str) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let result = vm_core::shell::browser_open(self.machine.home(), &cfg.name, url)
            .map_err(Self::vm_error)?;
        if result.code != 0 {
            return Err(unavailable(result.output));
        }
        Ok(())
    }

    pub fn chrome_stop(&self) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let result =
            vm_core::shell::browser_stop(self.machine.home(), &cfg.name).map_err(Self::vm_error)?;
        if result.code != 0 {
            return Err(unavailable(result.output));
        }
        Ok(())
    }

    pub fn chrome_paste(&self, text: &str) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        vm_core::shell::browser_paste(self.machine.home(), &cfg.name, Some(text))
            .map_err(Self::vm_error)
    }

    pub fn chrome_native_display(&self, request: Value) -> Result<Value, AppError> {
        self.machine
            .browser_native_display(request)
            .map_err(Self::vm_error)
    }

    pub fn chrome_enable_spice(&self) -> Result<(), AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        if !cfg.desktop_spice && !vm_core::chrome_feature::can_enable_spice(&cfg) {
            return Err(unavailable(
                "SPICE requires an installed Sway desktop without the device profile.",
            ));
        }
        self.busy("Enabling SPICE", || {
            self.disconnect();
            self.fail_pending("The machine is restarting to enable SPICE");
            self.machine.enable_spice()
        })
    }

    pub fn chrome_display(
        &self,
        protocol: Option<&str>,
        parent_origin: Option<&str>,
    ) -> Result<Value, AppError> {
        let protocol = protocol.unwrap_or("vnc");
        let url = self
            .machine
            .desktop_viewer(protocol, parent_origin)
            .map_err(Self::vm_error)?;
        Ok(json!({"protocol": protocol, "url": url}))
    }

    /// Forward the guest's `localhost:<port>` to the Mac for a browser preview.
    pub fn preview_forward(&self, port: u16) -> Result<Value, AppError> {
        let url = self.machine.preview_url(port).map_err(Self::vm_error)?;
        Ok(json!({"port": port, "url": url}))
    }

    pub fn preview_close(&self, port: u16) -> Result<(), AppError> {
        self.machine.preview_close(port).map_err(Self::vm_error)
    }

    /// Graphical apps installed in the machine's desktop.
    pub fn desktop_apps(&self) -> Result<Value, AppError> {
        let cfg = self.running_desktop()?;
        let apps = vm_core::desktop::apps(&cfg).map_err(Self::vm_error)?;
        serde_json::to_value(apps).map_err(|error| AppError::internal(error.to_string()))
    }

    /// Start an app on its own workspace of the desktop.
    pub fn desktop_launch(&self, id: &str) -> Result<(), AppError> {
        let cfg = self.running_desktop()?;
        vm_core::desktop::launch(&cfg, id).map_err(Self::vm_error)
    }

    fn running_desktop(&self) -> Result<vm_core::Config, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        if cfg.desktop_env.is_none() {
            return Err(unavailable("Install Chrome first; it sets up the desktop."));
        }
        if !self.machine.info().running {
            return Err(unavailable("Start the machine to use its desktop."));
        }
        Ok(cfg)
    }

    pub fn destroy(&self) -> Result<MachineStatus, AppError> {
        self.busy("Destroying the environment", || {
            let _guard = self
                .network
                .transaction.locked();
            self.generation.fetch_add(1, Ordering::SeqCst);
            self.update(|status| status.connection = GuestConnection::Offline);
            self.sync.disconnect();
            self.disconnect();
            self.fail_pending("The environment is being destroyed");
            self.network.stop();
            self.network
                .suspend_store()
                .map_err(|e| vm_core::Error::Message(e.message))?;
            let result = self.sync.reset_storage(|| self.machine.destroy());
            if let Err(error) = result {
                if let Err(reopen) = self.network.reopen_store() {
                    return Err(vm_core::Error::Message(format!(
                        "{error}; restoring network settings failed: {}",
                        reopen.message
                    )));
                }
                if self.machine.info().running
                    && let Ok(cfg) = self.machine.config()
                    && cfg.isolated()
                    && let Err(restart) = start_gateway(&self.network, &cfg, None)
                {
                    return Err(vm_core::Error::Message(format!(
                        "{error}; restoring the network gateway failed: {}",
                        restart.message
                    )));
                }
                return Err(error);
            } else {
                *self
                    .guest_snapshot.write_lock() = None;
                *self.startup.locked() = None;
                self.update(|status| {
                    status.connection = GuestConnection::Offline;
                    status.guest_version = None;
                });
                self.emit(AppEvent::Install { steps: vec![] });
            }
            Ok(self.machine.info())
        })?;
        Ok(self.snapshot().machine)
    }

    /// Host readiness for an install of the given size.
    pub fn checks(&self, disk_gb: u32, memory_mb: u32) -> vm_core::readiness::ReadinessReport {
        let mut items = vm_core::readiness::host_readiness(self.machine.home(), disk_gb, memory_mb);
        items.insert(
            items.iter().position(|item| item.id == "disk").unwrap_or(items.len()),
            vm_core::readiness::service_check(self.agentd.is_file()),
        );
        vm_core::readiness::ReadinessReport { items, host: vm_core::readiness::host_facts() }
    }

    /// Carry out a readiness fix.
    pub fn fix(&self, fix: vm_core::readiness::Fix) -> Result<vm_core::readiness::FixOutcome, AppError> {
        vm_core::readiness::apply_fix(&fix).map_err(|error| AppError {
            code: "fixFailed".into(),
            message: error.to_string(),
            details: None,
        })
    }

    // ------------------------------------------------------- guest link

    fn disconnect(&self) {
        if let Some(mut link) = self.link.locked().take() {
            let _ = link.child.kill();
            let _ = link.child.wait();
        }
    }

    fn fail_pending(&self, message: &str) {
        for (_, reply) in self
            .pending.locked()
            .drain()
        {
            let _ = reply.send(Err(unavailable(message)));
        }
    }

    /// The node catalog and the route live on the Mac; the guest's own
    /// (legacy) proxy state is replaced.
    fn overlay_snapshot(&self, snapshot: &mut AppSnapshot) {
        self.sync_environment_revision();
        if let Ok(catalog) = self.network.catalog() {
            snapshot.proxy = catalog;
        }
        if let Ok(status) = self.network.status(self.uses_gateway()) {
            snapshot.network = status;
        }
    }

    fn sync_environment_revision(&self) {
        if !self.network.is_up() {
            return;
        }
        match self.machine.config().and_then(|cfg| {
            if !cfg.isolated() {
                return Err(vm_core::Error::Message("The machine no longer has a managed network".into()));
            }
            cfg.environment_revision()
        }) {
            Ok(revision) => {
                self.network.observe_environment_revision(Some(revision));
            }
            Err(error) => {
                self.network.invalidate_gate(format!(
                    "The machine environment cannot be verified: {error}"
                ));
            }
        }
    }

    fn emit_network(&self) -> Result<(), AppError> {
        self.sync_environment_revision();
        self.emit(AppEvent::Network {
            catalog: self.network.catalog()?,
            status: self.network.status(self.uses_gateway())?,
        });
        Ok(())
    }

    /// A closed gate cuts every provider connection. Fail active chats now so
    /// they show a real error instead of retrying until the user stops them.
    fn fail_guest_chats(&self, message: impl Into<String>) {
        let _ = self.call_guest_with_timeout(
            "chat_fail_active", json!({ "message": message.into() }), Some(Duration::from_secs(3)),
        );
    }

    fn require_gateway(&self) -> Result<(), AppError> {
        if self.uses_gateway() {
            return Ok(());
        }
        Err(AppError {
            code: "network".into(),
            message:
                "This machine has no network gateway. Install a new machine to route its traffic."
                    .into(),
            details: None,
        })
    }

    fn restore_network_transaction(
        &self,
        route: &NetworkRoute,
        gate: RollbackPoint,
    ) -> Result<(), AppError> {
        let saved = self.network.save(route);
        let switched = if self.network.is_up() {
            self.network.switch(route)
        } else {
            Ok(())
        };
        if saved.is_ok() && switched.is_ok() {
            self.network.restore_gate(gate);
            return Ok(());
        }

        // A half-restored transaction must never inherit an old grant.
        let reason = saved
            .err()
            .or_else(|| switched.err())
            .map(|error| error.message)
            .unwrap_or_else(|| "unknown rollback failure".into());
        let message = format!("The previous route could not be restored: {reason}");
        self.network.abandon_upstream(&message);
        Err(AppError { code: "network_rollback".into(), message, details: None })
    }

    fn candidate_error(&self, error: AppError, rollback: Result<(), AppError>) -> AppError {
        match rollback {
            Ok(()) => error,
            Err(rollback) => AppError {
                code: error.code,
                message: format!("{}; {}", error.message, rollback.message),
                details: error.details.or(rollback.details),
            },
        }
    }

    /// The browser's part of a network check. Reads the installed settings
    /// and looks at the live Chrome (bounded; a closed or sleeping Chrome is a
    /// normal answer). The guest display transaction that makes the browser
    /// follow the machine's time zone and language runs only when they
    /// actually differ, not on every check.
    fn browser_environment(&self, guest: &GuestNetworkProbe) -> Option<BrowserEnvironmentProbe> {
        let cfg = self.machine.config().ok()?;
        if !cfg.browser_enabled() {
            return None;
        }
        let mut probe = Self::audit_browser(&cfg);
        if let (Some(timezone), Some(language)) = (&guest.timezone, &guest.language)
            && crate::network_gate::browser_needs_follow(&probe, guest)
        {
            match vm_core::shell::browser_follow_environment(self.machine.home(), &cfg.name, timezone, language) {
                Ok(true) => {
                    if let Ok(cfg) = self.machine.config() {
                        probe = Self::audit_browser(&cfg);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    probe.error = Some(format!("The browser could not follow the machine environment: {error}"));
                }
            }
        }
        Some(probe)
    }

    fn audit_browser(cfg: &vm_core::config::Config) -> BrowserEnvironmentProbe {
        let failed = |error: String| BrowserEnvironmentProbe {
            enabled: true,
            error: Some(error),
            ..BrowserEnvironmentProbe::default()
        };
        match vm_core::browser::audit_environment(cfg) {
            Ok(value) => serde_json::from_value(value).unwrap_or_else(|error| {
                failed(format!("The browser environment returned an invalid result: {error}"))
            }),
            Err(error) => failed(error.to_string()),
        }
    }

    fn apply_network_route(&self, route: &NetworkRoute) -> Result<NetworkStatus, AppError> {
        self.require_gateway()?;
        let connected = {
            let machine = self.status.read_lock();
            machine.state == MachineState::Running && machine.connection == GuestConnection::Connected
        };
        if !connected || self.network.status(true)?.paused {
            self.network.save_interface(route)?;
            self.emit_network()?;
            return self.network.status(true);
        }
        self.verify_route(route, true, "candidateCheck")
    }

    /// Verify the saved route or a candidate. A valid existing grant can be
    /// renewed in place. A candidate stays closed until all checks pass.
    fn verify_route(
        &self,
        route: &NetworkRoute,
        commit_candidate: bool,
        kind: &str,
    ) -> Result<NetworkStatus, AppError> {
        self.require_gateway()?;
        let previous_route = self.network.route()?;
        let mut previous_gate = None;
        let result = self.verify_route_attempt(route, commit_candidate, &previous_route, &mut previous_gate, kind);
        match result {
            Err(error) if commit_candidate => {
                let error = if let Some(point) = previous_gate {
                    let rollback = self.restore_network_transaction(&previous_route, point);
                    let message = if rollback.is_ok() { "Previous route restored" } else { "Previous route could not be restored" };
                    self.network.record_check("routeRollback", message, None);
                    self.candidate_error(error, rollback)
                } else {
                    error
                };
                let _ = self.emit_network();
                Err(error)
            }
            other => other,
        }
    }

    fn verify_route_attempt(
        &self,
        route: &NetworkRoute,
        commit_candidate: bool,
        previous_route: &NetworkRoute,
        previous_gate: &mut Option<RollbackPoint>,
        kind: &str,
    ) -> Result<NetworkStatus, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let profile_revision = cfg.environment_revision().map_err(Self::vm_error)?;
        self.network.observe_environment_revision(Some(profile_revision.clone()));
        if self.status.read_lock().state != MachineState::Running {
            return Err(unavailable("Start the machine before checking its network"));
        }
        let state = self.network.status(true)?;
        if state.paused {
            return Err(unavailable("Resume internet access before checking the route"));
        }
        let held = self.network.hold_check();
        let candidate_changed = commit_candidate && route != previous_route;
        if !candidate_changed && matches!(held, Some(HoldCheck::Reachability | HoldCheck::Guest)) {
            self.network.wake_health();
            return Ok(state);
        }
        if let Err(error) = self.network.upstream(route) {
            if !candidate_changed && held.is_none() {
                self.network.fail_route_preflight(&error.message);
                let _ = self.emit_network();
            }
            return Err(error);
        }
        let started_at = now_millis();
        // Diagnose a broken candidate before disturbing the working path.
        let candidate_check = if candidate_changed && route.enabled {
            let check = self.network.check(route).inspect_err(|e| {
                self.network.record_check(kind, &e.message, None);
            })?;
            if let Some(index) = check.broken_at {
                let message = format!("Route hop {} did not pass its transport probe", index + 1);
                self.network.record_check(kind, &message, None);
                return Err(AppError {
                    code: "network_check".into(),
                    message,
                    details: serde_json::to_string(&check).ok(),
                });
            }
            Some(check)
        } else {
            None
        };
        if commit_candidate {
            *previous_gate = Some(self.network.rollback_point());
        }
        if candidate_changed {
            self.network.invalidate_gate("Internet access is paused while the candidate route is checked");
        }
        let prepared = if self.network.is_up() {
            self.network.switch(route)
        } else {
            start_gateway(&self.network, &cfg, Some(route))
        };
        if let Err(error) = prepared {
            if !candidate_changed && held.is_none() {
                self.network.fail_route_preflight(&error.message);
                let _ = self.emit_network();
            }
            return Err(error);
        }
        let Some(attempt) = self.network.begin_check(route_revision(route), !candidate_changed) else {
            return Err(unavailable("The network changed before verification could start. Check again when the connection is ready."));
        };
        self.emit_network()?;

        let observed = std::thread::scope(|scope| {
            let exits = scope.spawn(|| self.network.exit_samples());
            let route_check = scope.spawn(|| {
                if candidate_check.is_some() || !route.enabled {
                    Ok(candidate_check)
                } else {
                    self.network.check(route).map(Some)
                }
            });
            let probe = self.guest_network_probe(attempt.checks_closed_boundary(), Duration::from_secs(20))?;
            let browser = self.browser_environment(&probe);
            let exits = exits.join().map_err(|_| unavailable("Exit sampling stopped"))??;
            let route_check = route_check.join().map_err(|_| unavailable("Route checking stopped"))??;
            Ok::<_, AppError>((probe, browser, exits, route_check))
        });
        let (probe, browser, mut exits, route_check) = match observed {
            Ok(observed) => observed,
            Err(error) => {
                if attempt.refresh {
                    if self.network.recheck_verification(&attempt, HoldCheck::Verification, error.message.clone()) {
                        self.stop_chats_after_hold();
                    }
                } else {
                    self.network.fail_check(&attempt, &error.message);
                }
                self.network.record_check(kind, &error.message, None);
                self.emit_network()?;
                return Err(error);
            }
        };
        self.network.reuse_exit_samples(&attempt, &probe, &mut exits);
        let report = evaluate_report(
            CheckInputs {
                route,
                profile_revision: Some(&profile_revision),
                route_check: route_check.as_ref(),
                gateway_running: self.network.is_up(),
                gateway_connected: self.network.gateway_connected(),
                guest: &probe,
                require_closed_boundary: attempt.checks_closed_boundary(),
                browser: browser.as_ref(),
                exits: &exits,
            },
            started_at,
        );
        let verification = if report.can_open { "Network and environment checks passed".into() } else { failure_summary(&report) };
        self.network.record_check(kind, &verification, Some(&report));
        self.sync_environment_revision();
        if attempt.refresh && !report.can_open {
            let check = if report.checks.iter().filter(|check| {
                check.required && check.status != agent_core::model::NetworkCheckStatus::Pass
            }).all(|check| matches!(check.id.as_str(), "dns" | "transport" | "boundary" | "gateway")) {
                HoldCheck::Guest
            } else {
                HoldCheck::Verification
            };
            if self.network.recheck_verification(&attempt, check, verification.clone()) {
                self.stop_chats_after_hold();
            }
            self.emit_network()?;
            return self.network.status(true);
        }
        let opened = match self.network.finish_check(
            &attempt,
            report.clone(),
            probe.public_ip.as_deref().and_then(|ip| ip.parse().ok()),
            &exits,
            commit_candidate.then_some(route),
        ) {
            Ok(opened) => opened,
            Err(error) if attempt.refresh => {
                if self.network.recheck_verification(&attempt, HoldCheck::Verification, error.message) {
                    self.stop_chats_after_hold();
                }
                false
            }
            Err(error) => return Err(error),
        };
        if commit_candidate && !opened {
            return Err(AppError {
                code: "network_check".into(),
                message: if report.can_open { "The network changed before the candidate could be committed".into() } else { verification },
                details: serde_json::to_string(&report).ok(),
            });
        }
        if commit_candidate {
            let message = if route.enabled {
                format!("Applied a proxy route with {} hops", route.chain.len())
            } else {
                "Applied the direct route".into()
            };
            self.network.record_check("routeApplied", &message, None);
        }
        self.emit_network()?;
        let status = self.network.status(true)?;
        if state.gate == NetworkGateMode::Open && status.gate == NetworkGateMode::Closed
            && status.connectivity == NetworkConnectivity::Normal && !opened
        {
            self.fail_guest_chats(format!("The machine's internet access was blocked. {verification}"));
        }
        Ok(status)
    }

    fn guest_network_probe(&self, check_boundary: bool, timeout: Duration) -> Result<GuestNetworkProbe, AppError> {
        let probe = self.call_guest_with_timeout(
            "network_probe", json!({ "checkClosedBoundary": check_boundary }), Some(timeout),
        )?;
        serde_json::from_value(probe).map_err(|error| {
            unavailable(format!("The machine returned an invalid probe result: {error}"))
        })
    }

    fn retry_guest_network(&self) -> Result<Option<std::net::Ipv4Addr>, String> {
        let probe = self.guest_network_probe(true, Duration::from_secs(7)).map_err(|error| error.message)?;
        if !probe.dns_ok {
            return Err("The machine could not resolve the check hostname".into());
        }
        if !(probe.connect_ok && probe.https_ok) {
            return Err(probe.error.unwrap_or_else(|| "The machine could not complete the HTTPS check".into()));
        }
        if !(probe.private_blocked && probe.ipv6_blocked && probe.udp_blocked && probe.unlisted_blocked == Some(true)) {
            return Err("The machine's traffic boundary did not pass".into());
        }
        Ok(probe.public_ip.and_then(|ip| ip.parse().ok()))
    }

    fn verify_current_network(&self) -> Result<NetworkStatus, AppError> {
        self.verify_route(&self.network.route()?, false, "startupCheck")
    }

    fn recover_network_if_due(&self) {
        // Detect changes to non-default adapters even during a route transaction.
        if self.network.poll_interface_paths() {
            let _ = self.emit_network();
        }
        // Expiry also runs while a different verification owns the transaction.
        if self.network.expire_hold_now() {
            let _ = self.emit_network();
            self.fail_guest_chats("The network recheck timed out. Internet access is blocked.");
        }
        if self.status.read_lock().connection != GuestConnection::Connected {
            return;
        }
        let Ok(_transaction) = self.network.transaction.try_lock() else { return; };
        self.sync_environment_revision();
        let (due, expired) = self.network.gate_due();
        if expired {
            let _ = self.emit_network();
        }
        if due && let Err(error) = self.network.route().and_then(|route| self.verify_route(&route, false, "automaticCheck")) {
            eprintln!("cabin: automatic network check: {}", error.message);
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn install_network_power_watch(self: &Arc<Self>) {
        let host = Arc::downgrade(self);
        network_health::install_power_watch(move |sleeping| {
            if let Some(host) = host.upgrade() {
                host.network.system_sleep_changed(sleeping);
                if !sleeping {
                    host.wake();
                }
            }
        });
    }

    fn start_network_recovery(self: &Arc<Self>) {
        self.start_network_health();
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("network-authorization".into())
            .spawn(move || {
                while let Some(me) = weak.upgrade() {
                    if me.stopping.load(Ordering::Relaxed) { break; }
                    me.recover_network_if_due();
                    drop(me);
                    std::thread::sleep(Duration::from_secs(2));
                }
            })
            .expect("spawn network authorization thread");
    }

    fn start_network_health(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        let (sender, signal) = mpsc::sync_channel(1);
        self.network.set_health_wake(sender);
        std::thread::Builder::new()
            .name("network-connectivity".into())
            .spawn(move || {
                #[cfg(target_os = "macos")]
                let _watch = {
                    let weak = weak.clone();
                    network_health::PathWatch::new(move || {
                        if let Some(host) = weak.upgrade() {
                            host.network.path_changed();
                            host.network.wake_health();
                        }
                    })
                };
                let mut delay = network_health::CHECK_INTERVAL;
                loop {
                    if matches!(signal.recv_timeout(delay), Ok(()))
                        && let Some(me) = weak.upgrade()
                    {
                        let _ = me.emit_network();
                        me.recover_network_if_due();
                    }
                    let Some(me) = weak.upgrade() else { break; };
                    if me.stopping.load(Ordering::Relaxed) { break; }
                    delay = network_health::CHECK_INTERVAL;
                    if let Some(target) = me.network.health_target() {
                        if target.check == Some(HoldCheck::Verification) {
                            // Updates, manual checks and this retry share the same transaction.
                            if let Ok(_transaction) = me.network.transaction.try_lock()
                                && me.network.held_target_is_current(&target)
                            {
                                let result = match &target.proxy_url {
                                    Ok(_) => me.network.route().and_then(|route| {
                                        me.verify_route(&route, false, "holdCheck")
                                    }).map(|_| ()).map_err(|error| error.message),
                                    Err(reason) => Err(reason.clone()),
                                };
                                if let Err(reason) = result {
                                    if me.network.fail_held_preflight(&target, reason) {
                                        me.stop_chats_after_hold();
                                    }
                                    let _ = me.emit_network();
                                }
                            }
                        } else {
                            let proxy = target.proxy_url.as_deref().map_err(String::clone);
                            let change = if target.check == Some(HoldCheck::Guest) {
                                let result = proxy.and_then(|_| me.retry_guest_network());
                                me.network.observe_guest_health(&target, result)
                            } else {
                                let result = proxy.and_then(network_health::probe);
                                me.network.observe_health(&target, result)
                            };
                            if change.changed {
                                let _ = me.emit_network();
                            }
                            if change.hard_block {
                                me.stop_chats_after_hold();
                            }
                        }
                        if me.network.hold_check().is_some() {
                            delay = network_health::RETRY_INTERVAL;
                        }
                    }
                }
            })
            .expect("spawn network connectivity thread");
    }

    fn stop_chats_after_hold(&self) {
        self.fail_guest_chats("Three network rechecks failed. Internet access is blocked and active agent requests were stopped.");
    }

    /// Forward one UI command to cabind and wait for its result. Network
    /// and catalog methods are answered here: they belong to the Mac.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, AppError> {
        let text = |key: &str| params.get(key).and_then(Value::as_str);
        let required = |key: &str| text(key).ok_or_else(|| unavailable(format!("Missing {key}")));
        let ids = |key: &str| -> Result<Vec<String>, AppError> {
            serde_json::from_value(params.get(key).cloned().unwrap_or(Value::Null))
                .map_err(|_| unavailable(format!("Missing {key}")))
        };
        let to_value =
            |value: Result<Value, serde_json::Error>| value.map_err(|e| unavailable(e.to_string()));
        // Changes are serialized; checks and lookups run alongside them.
        let _guard = matches!(
            method,
            "proxy_import"
                | "proxy_add_url"
                | "proxy_refresh_from_host"
                | "proxy_remove"
                | "proxy_group_save"
                | "proxy_group_remove"
                | "network_apply"
                | "network_verify"
                | "network_pause"
                | "network_auto_recover"
        )
        .then(|| {
            self.network
                .transaction.locked()
        });
        let changed = match method {
            "network_state" => {
                self.sync_environment_revision();
                return to_value(serde_json::to_value(
                    self.network.status(self.uses_gateway())?,
                ));
            }
            "network_interfaces" => {
                let interfaces = vm_core::network_interfaces::list().map_err(|e| unavailable(e.to_string()))?;
                return to_value(serde_json::to_value(interfaces));
            }
            "network_traffic" => return to_value(serde_json::to_value(self.network.traffic())),
            "network_history" => {
                let before = params.get("beforeId").and_then(Value::as_i64);
                let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(40).clamp(1, 100) as u32;
                return to_value(serde_json::to_value(self.network.history(before, limit)?));
            }
            "network_apply" => {
                let route: NetworkRoute = serde_json::from_value(params.get("route").cloned().unwrap_or(Value::Null))
                    .map_err(|e| unavailable(format!("Invalid route: {e}")))?;
                return to_value(serde_json::to_value(self.apply_network_route(&route)?));
            }
            "network_verify" => {
                let status = self.verify_route(&self.network.route()?, false, "manualCheck")?;
                return to_value(serde_json::to_value(status));
            }
            "network_pause" => {
                let paused = params
                    .get("paused")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| unavailable("Missing paused"))?;
                self.require_gateway()?;
                self.network.set_paused(paused)?;
                if paused {
                    self.fail_guest_chats(
                        "Internet access was paused. Active agent requests were stopped; resume the network and try again.",
                    );
                }
                self.emit_network()?;
                return to_value(serde_json::to_value(self.network.status(true)?));
            }
            "network_auto_recover" => {
                let enabled = params
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| unavailable("Missing enabled"))?;
                self.require_gateway()?;
                self.network.set_auto_recover(enabled)?;
                self.emit_network()?;
                return to_value(serde_json::to_value(self.network.status(true)?));
            }
            "network_check" => {
                let route: NetworkRoute = if let Some(value) = params.get("route") {
                    serde_json::from_value(value.clone()).map_err(|e| unavailable(format!("Invalid route: {e}")))?
                } else {
                    // Older clients sent only the candidate chain.
                    NetworkRoute { enabled: true, chain: ids("chain")?, bind_interface: self.network.route()?.bind_interface }
                };
                return to_value(serde_json::to_value(self.network.check(&route)?));
            }
            "network_exit" => {
                self.require_gateway()?;
                return to_value(serde_json::to_value(self.network.exit()?));
            }
            "network_exit_type" => {
                self.require_gateway()?;
                let refresh = params.get("refresh").and_then(Value::as_bool).unwrap_or(false);
                return to_value(serde_json::to_value(self.network.exit_type(required("ip")?, refresh)?));
            }
            "network_origin" => return to_value(serde_json::to_value(self.network.origin()?)),
            "network_locate" => {
                return to_value(serde_json::to_value(self.network.locate(&ids("nodeIds")?)?));
            }
            "proxy_probe" => {
                return to_value(serde_json::to_value(
                    self.network.probe(required("nodeId")?)?,
                ));
            }
            "proxy_catalog" => return to_value(serde_json::to_value(self.network.catalog()?)),
            "proxy_node_test" => {
                let kind = serde_json::from_value(params.get("kind").cloned().unwrap_or(Value::Null))
                    .map_err(|_| unavailable("Choose a latency or download test"))?;
                let interface: Option<String> = serde_json::from_value(params.get("bindInterface").cloned().unwrap_or(Value::Null))
                    .map_err(|_| unavailable("Invalid network interface"))?;
                return to_value(serde_json::to_value(self.network.test_node(
                    required("nodeId")?,
                    required("configRevision")?,
                    kind,
                    interface.as_deref(),
                )?));
            }
            "proxy_group_save" => serde_json::to_value(self.network.save_node_group(
                text("id"),
                required("name")?,
                &ids("nodeIds")?,
            )?),
            "proxy_group_remove" => serde_json::to_value(self.network.remove_node_group(required("id")?)?),
            "proxy_import" => serde_json::to_value(self.network.import(
                required("name")?,
                text("url"),
                text("contents"),
            )?),
            "proxy_add_url" => serde_json::to_value(self.network.add_manual(required("url")?)?),
            "proxy_refresh" => serde_json::to_value(self.network.refresh(
                required("id")?,
                params.get("automatic").and_then(Value::as_bool).unwrap_or(false),
            )?),
            "proxy_refresh_from_host" => {
                serde_json::to_value(self.network.refresh_from_host(required("id")?)?)
            }
            "proxy_remove" => serde_json::to_value(self.network.remove(required("id")?)?),
            _ => {
                if matches!(method, "chat_send" | "chat_steer" | "chat_retry")
                    && self.uses_gateway()
                    && let Some(reason) = self.network.blocked_reason()
                {
                    return Err(AppError {
                        code: "network_check".into(),
                        message: reason,
                        details: None,
                    });
                }
                return self.call_guest(method, params);
            }
        };
        let message = match method {
            "proxy_import" => "Imported a subscription",
            "proxy_add_url" => "Added a proxy node",
            "proxy_refresh" | "proxy_refresh_from_host" => "Refreshed a subscription",
            "proxy_remove" => "Removed a subscription",
            "proxy_group_save" => "Saved a node group",
            "proxy_group_remove" => "Removed a node group",
            _ => "Updated the network catalog",
        };
        self.network.record_check("catalogChanged", message, None);
        self.emit_network()?;
        to_value(changed)
    }

    fn call_guest(&self, method: &str, params: Value) -> Result<Value, AppError> {
        self.call_guest_with_timeout(method, params, None)
    }

    fn call_guest_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, AppError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, wait) = mpsc::channel();
        {
            let mut link = self.link.locked();
            let Some(link) = link.as_mut() else {
                let state = self.status.read_lock().state;
                return Err(unavailable(match state {
                    MachineState::NotInstalled => {
                        "The machine is not installed yet. Install it from Setup."
                    }
                    MachineState::Stopped => {
                        "The machine is stopped. Start it to use workspaces and agents."
                    }
                    MachineState::Running => "Connecting to the machine…",
                }));
            };
            self.pending.locked()
                .insert(id, reply);
            let mut line = json!({ "id": id, "method": method, "params": params }).to_string();
            line.push('\n');
            if let Err(error) = link
                .stdin
                .write_all(line.as_bytes())
                .and_then(|()| link.stdin.flush())
            {
                self.pending.locked()
                    .remove(&id);
                return Err(unavailable(format!(
                    "The connection to the machine dropped: {error}"
                )));
            }
        }
        if let Some(timeout) = timeout {
            return match wait.recv_timeout(timeout) {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(unavailable("The connection to the machine dropped"))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.pending.locked()
                        .remove(&id);
                    Err(unavailable(format!(
                        "The guest request {method} timed out after {} seconds",
                        timeout.as_secs()
                    )))
                }
            };
        }
        wait.recv()
            .unwrap_or_else(|_| Err(unavailable("The connection to the machine dropped")))
    }

    /// Load the guest library and make the first environment result available
    /// before exposing a connected workspace. The SSH reader stays live while
    /// this worker waits for guest RPCs.
    fn load_guest(
        &self,
        generation: u64,
        alive: &AtomicBool,
        attempt: Option<&Arc<Attempt>>,
        version: String,
        daemon: String,
    ) -> Result<(), String> {
        let still_connected = || {
            alive.load(Ordering::Acquire)
                && generation == self.generation.load(Ordering::SeqCst)
                && !self.stopping.load(Ordering::Relaxed)
        };
        if let Some(attempt) = attempt {
            attempt.complete(Phase::Daemon, daemon);
            attempt.begin(Phase::Library);
            self.publish_start(attempt);
        }
        if self.uses_gateway() {
            if let Some(attempt) = attempt {
                attempt.note("Applying IPv4-only guest network configuration");
            }
            self.call_guest_with_timeout("network_ipv4_ensure", json!({}), Some(Duration::from_secs(60)))
                .map_err(|error| {
                    let message = format!("Could not configure IPv4-only guest networking: {}", error.message);
                    self.network.invalidate_gate(&message);
                    let _ = self.emit_network();
                    message
                })?;
        }
        let value = self
            .call_guest_with_timeout(
                "app_startup_snapshot",
                json!({}),
                Some(Duration::from_secs(30)),
            )
            .map_err(|error| {
                format!(
                    "Could not load the initial guest snapshot: {}",
                    error.message
                )
            })?;
        let mut snapshot: AppSnapshot = serde_json::from_value(value)
            .map_err(|error| format!("The initial guest snapshot was invalid: {error}"))?;
        if !still_connected() {
            return Err("The machine disconnected while loading guest state".into());
        }
        if let Some(attempt) = attempt {
            attempt.complete(
                Phase::Library,
                format!(
                    "Loaded {} workspaces and {} sessions",
                    snapshot.workspaces.len(),
                    snapshot.sessions.len()
                ),
            );
            if self.uses_gateway() {
                attempt.begin(Phase::Network);
                self.publish_start(attempt);
                let _transaction = self
                    .network
                    .transaction.locked();
                if !still_connected() {
                    return Err("The machine disconnected before the network check".into());
                }
                let result = self.verify_current_network();
                if !still_connected() {
                    return Err("The machine disconnected during the network check".into());
                }
                match result {
                    Ok(status) if status.gate == NetworkGateMode::Open => {
                        attempt.complete(Phase::Network, "Internet access is ready");
                    }
                    Ok(status) => attempt.check_failed(
                        Phase::Network,
                        &status
                            .gate_error
                            .unwrap_or_else(|| "Internet access is blocked".into()),
                    ),
                    Err(error) => attempt.check_failed(Phase::Network, &error.message),
                }
            } else {
                attempt.skip(Phase::Network, "This machine does not use the gateway");
            }
        }
        self.overlay_snapshot(&mut snapshot);
        if !still_connected() {
            return Err("The machine disconnected before startup finished".into());
        }
        *self
            .guest_snapshot.write_lock() = Some(snapshot.clone());
        self.update(|status| {
            status.connection = GuestConnection::Connected;
            status.guest_version = Some(version);
            status.error = None;
        });
        self.emit(AppEvent::Snapshot {
            snapshot: Box::new(snapshot),
        });
        if let Some(attempt) = attempt {
            self.finish_start(attempt, None);
        }
        // Synced projects follow the guest link.
        if let Ok(runtime) = self.machine.runtime()
            && let Ok(ssh) = runtime.ssh_argv(false)
        {
            self.sync.connect(ssh);
        }
        // The first snapshot already has the installed CLIs and models. Their
        // version and sign-in checks are published separately after readiness.
        let _ = self.call_guest_with_timeout(
            "agents_refresh",
            json!({}),
            Some(Duration::from_secs(90)),
        );
        Ok(())
    }

    /// The bundled cabind's SHA-256, recomputed only when the file changes.
    fn bundled_build(&self) -> Result<String, String> {
        let meta = std::fs::metadata(&self.agentd).map_err(|_| {
            format!(
                "cabind for the VM was not found at {}",
                self.agentd.display()
            )
        })?;
        let key = (meta.len(), meta.modified().unwrap_or(std::time::UNIX_EPOCH));
        let mut cached = self.agentd_build.locked();
        if let Some((len, modified, hash)) = cached.as_ref()
            && (*len, *modified) == key
        {
            return Ok(hash.clone());
        }
        let hash = agent_core::util::sha256_file(&self.agentd)
            .map_err(|error| format!("Could not read the bundled cabind: {error}"))?;
        *cached = Some((key.0, key.1, hash.clone()));
        Ok(hash)
    }

    /// Open `cabind connect` over SSH and read its hello line. The error
    /// says whether the guest answered at all: `reachable == false` (SSH
    /// itself failed or hung) must not be "fixed" by redeploying the daemon.
    fn open_link(
        &self,
        runtime: &vm_core::vm::Runtime,
        attempt: Option<&Arc<Attempt>>,
    ) -> Result<GuestLink, LinkFailure> {
        let unreachable = |reason: String| LinkFailure { reason, reachable: false };
        let argv = vm_core::agentd::connect_argv(runtime)
            .map_err(|error| unreachable(error.to_string()))?;
        let stderr = attempt
            .and_then(|attempt| attempt.log_file())
            .map_or_else(Stdio::null, Stdio::from);
        let mut command = Command::new(&argv[0]);
        command
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        vm_core::util::no_window(&mut command);
        let mut child = command
            .spawn()
            .map_err(|e| unreachable(format!("could not run {}: {e}", argv[0])))?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        let (hello_tx, hello_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            let _ = hello_tx.send((lines.next(), lines));
        });
        let timeout = Duration::from_secs(20) * vm_core::platform::slowdown();
        let deadline = Instant::now() + timeout;
        let hello = match hello_rx.recv_timeout(timeout) {
            Err(_) => Err(unreachable(
                "Timed out waiting for the cabind handshake".into(),
            )),
            Ok((Some(Ok(line)), lines)) => match serde_json::from_str::<Value>(&line) {
                Ok(hello) => Ok((hello, lines)),
                Err(error) => Err(LinkFailure {
                    reason: format!("Invalid cabind handshake: {error}"),
                    reachable: true,
                }),
            },
            // The stream closed without a hello: ssh exits 255 when the guest
            // could not be reached; any other code means the guest answered
            // but no (working) daemon did.
            Ok(_) => {
                let code = wait_for_ssh_exit(&mut child, deadline);
                Err(match code {
                    Some(255) => unreachable("Could not reach the guest over SSH".into()),
                    None => unreachable("SSH closed the handshake without exiting".into()),
                    _ => LinkFailure {
                        reason: "cabind did not answer".into(),
                        reachable: true,
                    },
                })
            }
        };
        match hello {
            Ok((hello, lines)) => Ok((child, stdin, lines, hello)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error)
            }
        }
    }

    /// One connection attempt; returns when the link drops.
    ///
    /// Version sync is handshake-first: connect, read the daemon's `build`
    /// fingerprint, and continue if it is the bundled build. Anything else
    /// (another build, an older daemon without fingerprints, or no daemon at
    /// all) deploys the bundled binary once and reconnects.
    fn session(self: &Arc<Self>, attempt: Option<Arc<Attempt>>) -> Result<(), String> {
        use agent_core::hello::{Handshake, check_hello, short_build};
        let generation = self.generation.load(Ordering::SeqCst);
        let runtime = self.machine.runtime().map_err(|error| error.to_string())?;
        let guest = GuestBuild {
            generation,
            pid: runtime.pid(),
            build: self.bundled_build()?,
        };
        let expected = &guest.build;
        let note = |text: &str| {
            if let Some(attempt) = &attempt {
                attempt.note(text);
            }
        };
        let changed = || {
            generation != self.generation.load(Ordering::SeqCst)
                || self.stopping.load(Ordering::Relaxed)
        };
        let (mut child, stdin, lines, version) = loop {
            if changed() {
                return Err("The environment changed while connecting".into());
            }
            note("Connecting to cabind");
            let outcome = self.open_link(&runtime, attempt.as_ref());
            let reason = match outcome {
                Ok((mut child, stdin, lines, hello)) => match check_hello(&hello, expected) {
                    Handshake::Ready(version) => break (child, stdin, lines, version),
                    Handshake::Outdated(reason) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        reason
                    }
                },
                Err(LinkFailure { reason, reachable: false }) => return Err(reason),
                Err(LinkFailure { reason, .. }) => reason,
            };
            if changed() {
                return Err("The environment changed while connecting".into());
            }
            if !self.deployment.locked().claim(&guest) {
                let error = format!(
                    "cabind still does not match this app after an update was attempted: {reason}"
                );
                if let Some(attempt) = &attempt {
                    self.finish_start(attempt, Some(error.clone()));
                }
                return Err(error);
            }
            self.update(|status| status.connection = GuestConnection::Updating);
            note(&format!(
                "{reason}; installing build {}",
                short_build(expected)
            ));
            vm_core::agentd::deploy(&runtime, &self.agentd).map_err(|error| error.to_string())?;
        };
        let deployed = self.deployment.locked().ready(&guest);
        let daemon = format!(
            "cabind {version} · build {}{}",
            short_build(expected),
            if deployed { " · updated" } else { "" }
        );
        note(&daemon);
        {
            let mut link = self.link.locked();
            if generation != self.generation.load(Ordering::SeqCst)
                || self.stopping.load(Ordering::Relaxed)
            {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The environment changed while connecting".into());
            }
            *link = Some(Link { stdin, child });
        }

        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = alive.clone();
        let me = self.clone();
        let load = std::thread::spawn(move || {
            let result =
                me.load_guest(generation, &worker_alive, attempt.as_ref(), version, daemon);
            if result.is_err() {
                me.disconnect();
            }
            result
        });

        for line in lines {
            if generation != self.generation.load(Ordering::SeqCst) {
                break;
            }
            let Ok(line) = line else { break };
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(event) = value.get("event") {
                if let Ok(mut event) = serde_json::from_value::<AppEvent>(event.clone()) {
                    match &mut event {
                        AppEvent::Snapshot { snapshot } => self.overlay_snapshot(snapshot),
                        AppEvent::Agents { agents } => {
                            if let Some(snapshot) = self
                                .guest_snapshot.write_lock()
                                .as_mut()
                            {
                                snapshot.agents.clone_from(agents);
                            }
                        }
                        AppEvent::Environment { .. } => {
                            self.network.invalidate_gate(
                                "Environment settings changed and need a full check",
                            );
                            let _ = self.emit_network();
                        }
                        _ => {}
                    }
                    self.emit(event);
                }
            } else if let Some(id) = value.get("id").and_then(Value::as_u64) {
                let reply = self
                    .pending.locked()
                    .remove(&id);
                if let Some(reply) = reply {
                    let result = match value.get("error") {
                        Some(error) => Err(serde_json::from_value(error.clone())
                            .unwrap_or_else(|_| unavailable("Malformed error from the machine"))),
                        None => Ok(value.get("result").cloned().unwrap_or(Value::Null)),
                    };
                    let _ = reply.send(result);
                }
            }
        }
        alive.store(false, Ordering::Release);
        self.sync.disconnect();
        self.disconnect();
        self.fail_pending("The connection to the machine dropped");
        load.join()
            .map_err(|_| "Guest snapshot worker stopped unexpectedly".to_string())?
    }

    /// Start an installed machine once per app launch, then maintain its link.
    pub fn run_connection_loop(self: &Arc<Self>) {
        let kick_rx = self
            .kick_rx.locked()
            .take()
            .expect("connection loop started once");
        let launch = self
            .machine
            .installed()
            .then(|| self.begin_start().ok())
            .flatten();
        let me = self.clone();
        std::thread::Builder::new()
            .name("guest-link".into())
            .spawn(move || {
                if let Some(attempt) = launch {
                    let _ = me.start_machine(&attempt);
                }
                let mut backoff = Duration::from_millis(250);
                while !me.stopping.load(Ordering::Relaxed) {
                    let status = me.status.read_lock().clone();
                    let start_state = me.start_state();
                    if status.state != MachineState::Running
                        || status.busy.is_some()
                        || start_state == Some(StartState::Failed)
                    {
                        backoff = Duration::from_millis(250);
                        let _ = kick_rx.recv_timeout(Duration::from_secs(2));
                        continue;
                    }
                    if start_state.is_none() {
                        // The installation already booted QEMU. Finish startup by attaching.
                        if let Ok(attempt) = me.begin_start() {
                            let _ = me.start_machine(&attempt);
                        }
                        continue;
                    }
                    let attempt = me.active_start();
                    me.update(|status| status.connection = GuestConnection::Connecting);
                    let generation = me.generation.load(Ordering::SeqCst);
                    let outcome = (|| {
                        if let Ok(cfg) = me.machine.config()
                            && cfg.isolated()
                            && !me.network.is_up()
                        {
                            start_gateway(&me.network, &cfg, None)
                                .map_err(|error| error.message)?;
                        }
                        me.session(attempt.clone())
                    })();
                    *me.guest_snapshot.write_lock() = None;
                    me.update(|status| {
                        status.connection = GuestConnection::Offline;
                        if generation == me.generation.load(Ordering::SeqCst) {
                            status.error = outcome
                                .as_ref()
                                .err()
                                .map(|error| format!("Cannot reach cabind: {error}"));
                        }
                    });
                    if generation == me.generation.load(Ordering::SeqCst)
                        && !me.stopping.load(Ordering::Relaxed)
                    {
                        me.refresh_machine();
                        if let Some(attempt) = &attempt
                            && attempt.running()
                        {
                            let detail = outcome
                                .as_ref()
                                .err()
                                .map(String::as_str)
                                .unwrap_or("The guest link closed");
                            attempt.note(&format!("Connection attempt ended: {detail}"));
                            let stopped = me.status.read_lock().state
                                != MachineState::Running;
                            let initializing = attempt.phase_running(Phase::Library)
                                || attempt.phase_running(Phase::Network);
                            if stopped
                                || initializing
                                || attempt.phase_elapsed(Phase::Daemon)
                                    >= Duration::from_secs(90) * vm_core::platform::slowdown()
                            {
                                me.finish_start(
                                    attempt,
                                    Some(if stopped {
                                        format!("The machine stopped while connecting: {detail}")
                                    } else {
                                        detail.to_string()
                                    }),
                                );
                            }
                        }
                    }
                    if outcome.is_ok() {
                        backoff = Duration::from_millis(250);
                    }
                    let _ = kick_rx.recv_timeout(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(5));
                }
            })
            .expect("spawn guest link thread");
    }

    pub fn prepare_quit(&self, stop_machine: bool) -> Result<(), AppError> {
        self.quit(stop_machine, Duration::from_secs(12))
    }

    fn quit(&self, stop_machine: bool, timeout: Duration) -> Result<(), AppError> {
        self.stopping.store(true, Ordering::Release);
        self.machine.begin_quit();
        let busy = self.status.read_lock().busy.is_some();
        let start = self.active_start();
        let busy = busy || start.is_some();
        if let Some(attempt) = start {
            self.finish_start(&attempt, Some("Cabin is quitting".into()));
        }
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.wake();
        self.disconnect();
        self.sync.disconnect();
        self.fail_pending("Cabin is quitting");
        let stopped = if stop_machine || busy {
            self.machine.shutdown(timeout).map_err(Self::vm_error)
        } else {
            Ok(())
        };
        self.network.stop();
        stopped
    }

    pub fn shutdown(&self) {
        // The normal quit command already completed its cleanup. A native
        // exit (including the Windows end-session notification) takes this path.
        if !self.stopping.load(Ordering::Acquire)
            && let Err(error) = self.quit(true, Duration::from_secs(4))
        {
            eprintln!("cabin: {}", error.message);
        }
    }
}

fn wait_for_ssh_exit(child: &mut Child, deadline: Instant) -> Option<i32> {
    loop {
        if let Some(status) = child.try_wait().ok()? {
            return status.code();
        }
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return None;
        }
        std::thread::sleep(remain.min(Duration::from_millis(20)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_does_not_repeat_a_deploy_until_a_match_or_a_new_start() {
        let mut deployment = Deployment::default();
        let mut guest = GuestBuild {
            generation: 1,
            pid: Some(2),
            build: "new".into(),
        };
        assert!(deployment.claim(&guest));
        for _ in 0..3 {
            assert!(!deployment.claim(&guest));
        }
        guest.generation += 1;
        assert!(deployment.claim(&guest));
        assert!(!deployment.claim(&guest));
        assert!(deployment.ready(&guest));
        assert!(deployment.claim(&guest));
    }

    #[test]
    fn quit_with_a_pending_start_rejects_new_machine_operations() {
        let dir = tempfile::tempdir().unwrap();
        let machine = Machine::new(dir.path(), vm_core::paths::default_project_root());
        let host = Host::new(machine, dir.path().join("cabind")).unwrap();
        let _attempt = host.begin_start().unwrap();
        host.prepare_quit(false).unwrap();
        let result = host.claim_machine("Starting", true);
        assert_eq!(result.unwrap_err().message, "Cabin is quitting");
    }

    #[test]
    fn network_apply_defers_only_interface_changes_without_live_verification() {
        for (running, paused) in [(false, false), (true, false), (true, true)] {
            let home = tempfile::tempdir().unwrap();
            let machine = Machine::new(home.path(), concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
            machine.install_config(&InstallOptions::default()).unwrap().save_instance_meta().unwrap();
            let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
            if running {
                host.network.start(&home.path().join("gateway.sock"), 0).unwrap();
                host.network.set_paused(paused).unwrap();
                host.update(|status| {
                    status.state = MachineState::Running;
                    status.connection = if paused { GuestConnection::Connected } else { GuestConnection::Offline };
                });
            }
            let bound = NetworkRoute { bind_interface: Some("en-missing".into()), ..NetworkRoute::default() };
            host.network.save(&bound).unwrap();
            let proxy = NetworkRoute { enabled: true, chain: vec!["test".into()], ..bound.clone() };
            assert!(host.call("network_apply", json!({ "route": proxy })).is_err());
            assert_eq!(host.network.route().unwrap(), bound);

            let ssh = host.network.ssh_port();
            let system = NetworkRoute::default();
            let reply = host.call("network_apply", json!({ "route": system })).unwrap();
            let status: NetworkStatus = serde_json::from_value(reply).unwrap();
            assert_eq!(status.route, system);
            assert_eq!(status.gate, NetworkGateMode::Closed);
            assert_eq!(status.paused, paused);
            assert_eq!(host.network.ssh_port(), ssh);
            assert_eq!(host.network.route().unwrap(), system);
            host.shutdown();
        }
    }

    #[test]
    fn an_unreadable_machine_config_closes_a_running_gateway() {
        let home = tempfile::tempdir().unwrap();
        let machine = Machine::new(home.path(), concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        let route = NetworkRoute::default();
        host.network.start(&home.path().join("gateway.sock"), 0).unwrap();
        host.network.begin_check(route_revision(&route), false).unwrap();
        assert_eq!(host.network.status(true).unwrap().gate, NetworkGateMode::Checking);
        host.sync_environment_revision();
        let status = host.network.status(true).unwrap();
        assert_eq!(status.gate, NetworkGateMode::Closed);
        assert!(status.gate_error.unwrap().contains("cannot be verified"));
    }

    #[test]
    fn failed_auto_start_keeps_a_report_and_retry_gets_a_new_log() {
        let home = std::env::temp_dir().join(format!("cabin-host-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let meta = vm_core::paths::StatePaths::new(&home).instance_meta(vm_core::machine::NAME);
        std::fs::create_dir_all(meta.parent().unwrap()).unwrap();
        std::fs::write(&meta, "{}").unwrap();
        let machine = Machine::new(&home, concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        let (events, wait) = mpsc::channel();
        host.set_sink(move |event| {
            let _ = events.send(event);
        });
        host.run_connection_loop();
        let first = loop {
            let event = wait.recv_timeout(Duration::from_secs(3)).unwrap();
            if let AppEvent::Machine { status } = event
                && status.error.is_some()
                && status.busy.is_none()
            {
                break host.snapshot().start.unwrap();
            }
        };
        assert_eq!(first.state, StartState::Failed);
        assert_eq!(
            first.phases[0].state,
            agent_core::model::StartPhaseState::Failed
        );
        assert!(host.start().is_err());
        let retry = host.snapshot().start.unwrap();
        assert_eq!(retry.state, StartState::Failed);
        assert_ne!(first.log_path, retry.log_path);
        assert!(
            retry
                .log_tail
                .iter()
                .any(|line| line.contains("Startup failed"))
        );
        host.shutdown();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_competing_install_leaves_the_active_operation_and_network_untouched() {
        let home = tempfile::tempdir().unwrap();
        let machine = Machine::new(home.path(), concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        host.busy("Stopping the machine", || {
            assert!(host.install(MachineInstallOptions::default()).is_err());
            assert_eq!(host.snapshot().machine.busy.as_deref(), Some("Stopping the machine"));
            assert!(!host.network.is_up());
            Ok(())
        }).unwrap();
        assert_eq!(host.snapshot().machine.busy, None);
    }

    #[test]
    fn destroy_resets_host_store_and_machine_files_without_stale_state() {
        let home = std::env::temp_dir().join(format!("cabin-host-destroy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let machine = Machine::new(&home, concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        let catalog = host
            .network
            .import(
                "old",
                None,
                Some("proxies:\n  - {name: old, type: socks5, server: 127.0.0.1, port: 1080}\n"),
            )
            .unwrap();
        host.network
            .save(&NetworkRoute {
                enabled: true,
                chain: vec![catalog.nodes[0].id.clone()],
                bind_interface: None,
            })
            .unwrap();
        std::fs::create_dir_all(home.join("images")).unwrap();
        std::fs::write(home.join("images/cached"), "image").unwrap();
        let status = host.destroy().unwrap();
        assert_eq!(status.state, MachineState::NotInstalled);
        assert!(!home.join("images").exists());
        assert!(!home.join("network").exists());
        assert!(host.network.catalog().unwrap().nodes.is_empty());
        assert!(!host.network.route().unwrap().enabled);
        host.network.reopen_store().unwrap();
        host.network
            .import(
                "new",
                None,
                Some("proxies:\n  - {name: new, type: socks5, server: 127.0.0.1, port: 1081}\n"),
            )
            .unwrap();
        drop(host);
        let reopened = GlobalNetwork::open(&home.join("network")).unwrap();
        assert_eq!(reopened.catalog().unwrap().sources[0].name, "new");
        drop(reopened);
        let _ = std::fs::remove_dir_all(home);
    }
}
