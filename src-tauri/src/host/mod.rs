//! The Mac side: owns the VM (through `vm_core::machine`) and a live link to
//! `cabind` inside it. UI commands for workspaces, sessions, agents and
//! the environment are forwarded to the guest unchanged; guest events are
//! pushed to the window together with machine events.

mod dispatch;
mod guest;
mod machine;
mod network_check;

use agent_core::util::{LockExt, RwLockExt};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use agent_core::model::{
    AppEvent, AppSnapshot, GuestConnection, HostSnapshot, MachineInstallOptions, MachineState,
    MachineStatus, NetworkRoute, StartReport, StartState,
};
use serde_json::{Value, json};
use vm_core::machine::{InstallOptions, Machine, MachineInfo};
use vm_core::startup::{Attempt, Phase};

use crate::app_error::AppError;
use crate::global_network::GlobalNetwork;

use guest::{Deployment, Link};

type Sink = Arc<dyn Fn(AppEvent) + Send + Sync>;
type Reply = mpsc::Sender<Result<Value, AppError>>;

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
        cabin_diagnostics::event(cabin_diagnostics::Level::Warn, "ssh", "relay_port_write_failed", format_args!("{error}"));
    }
    Ok(())
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
        host.start_network_recovery()?;
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
            // The handshake can compute the fingerprint on demand if prewarming fails.
            let _ = std::thread::Builder::new().name("guest-build-fingerprint".into()).spawn(move || {
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
        if let Err(error) = std::thread::Builder::new().name("startup-progress".into()).spawn(move || {
            while hot.running() {
                std::thread::sleep(Duration::from_millis(400));
                if let Some(me) = me.upgrade() {
                    me.publish_start(&hot);
                } else {
                    break;
                }
            }
        }) {
            attempt.note(&format!("Live startup progress is unavailable: {error}"));
        }
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

    pub fn save_resources(&self, request: vm_core::resources::ResourceRequest, cancel_disk: bool) -> Result<Option<vm_core::resources::ResourceRequest>, AppError> {
        // Serialize with desktop machine operations. Validation errors belong to
        // the settings form, not the machine's connection state.
        let status = {
            let startup = self.startup.locked();
            if startup.as_ref().is_some_and(|(attempt, _)| attempt.running()) {
                return Err(unavailable("Wait for the machine startup attempt to finish"));
            }
            self.claim_machine("Saving machine resources", false)?
        };
        self.emit(AppEvent::Machine { status });
        let result = self.machine.save_resources(request, cancel_disk).map_err(Self::vm_error);
        self.update(|status| status.busy = None);
        result
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
            cabin_diagnostics::event(cabin_diagnostics::Level::Error, "lifecycle", "shutdown_failed", format_args!("{}", error.code));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
