//! Project sync, the Mac's half: a Mac folder kept in step with a folder in
//! the machine (`agent_core::sync` has the shared logic, `cabind sync`
//! the guest's half).
//!
//! Each project runs one session while the machine is connected: a full
//! cycle first, then a cycle per burst of changes, scoped to the folders
//! that changed. Mac changes come from FSEvents; in two-way mode the guest
//! runs `cabind sync watch` (inotify) over its own SSH connection. A
//! full cycle also runs every few minutes as a safety net. A cycle scans
//! both sides under the same rules, lets `plan::reconcile` decide, sends
//! everything for the machine as one tar stream into `sync apply`, pulls
//! with one `sync pack`, and records the new base. Projects and bases live
//! in `<home>/sync`.

use agent_core::util::{LockExt, RwLockExt};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use agent_core::model::{AppEvent, SyncProject, SyncProjectInput, SyncState, SyncStatus, SyncView};
use agent_core::sync::archive::{ApplyChecks, ApplyReport};
use agent_core::sync::plan::BaseRecord;
use agent_core::sync::scan::{self, Entry};
use agent_core::sync::{SyncMode, SyncRules};
use agent_core::util::now_millis;

use crate::host_permissions::check_folder;

mod cycle;
mod pending;
mod transport;
use pending::{Destination, Pending};
use transport::{pull_into, remote_apply, remote_scan, watch_guest};

#[cfg(any(windows, test))]
mod windows_paths;

type Base = BTreeMap<String, BaseRecord>;
type Listing = BTreeMap<String, Entry>;
type Sink = Arc<dyn Fn(AppEvent) + Send + Sync>;

/// A full cycle at least this often, whatever the watchers say.
const FULL_EVERY: Duration = Duration::from_secs(600);
const RETRY_EVERY: Duration = Duration::from_secs(30);
const KICK_QUEUE: usize = 16;

fn project_requires_full_scan(project: &SyncProject) -> bool {
    cfg!(windows) && project.mode == SyncMode::TwoWay
}

enum Kick {
    Scope(Vec<String>),
    Full,
}

#[derive(Clone)]
struct Kicks {
    sender: mpsc::SyncSender<Kick>,
    rescan: Arc<AtomicBool>,
}

impl Kicks {
    fn send(&self, kick: Kick) -> Result<(), String> {
        match self.sender.try_send(kick) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => {
                self.rescan.store(true, Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err("The project sync worker has stopped".into())
            }
        }
    }

    fn take_rescan(&self) -> bool {
        self.rescan.swap(false, Ordering::Relaxed)
    }
}

struct Session {
    stop: Arc<AtomicBool>,
    kick: Kicks,
    #[cfg(unix)]
    worker: std::thread::JoinHandle<()>,
}

type InitialSync = mpsc::Sender<Result<(), String>>;

#[derive(Default)]
struct CycleOutcome {
    conflicts: Vec<String>,
    transfer_error: Option<String>,
}

impl CycleOutcome {
    fn require_transferred(self) -> Result<(), String> {
        self.transfer_error.map_or(Ok(()), Err)
    }

    fn require_complete(self) -> Result<(), String> {
        let has_conflicts = !self.conflicts.is_empty();
        self.require_transferred()?;
        if has_conflicts {
            return Err("Sync left conflicts; both versions were kept".into());
        }
        Ok(())
    }
}

fn transfer_error(failed: &[(String, String)]) -> Option<String> {
    let (path, error) = failed.first()?;
    Some(format!(
        "{} {} could not be synced (first: {path}: {error})",
        failed.len(),
        if failed.len() == 1 { "path" } else { "paths" }
    ))
}

fn require_applied(report: ApplyReport) -> Result<(), String> {
    if let Some(error) = transfer_error(&report.failed) {
        return Err(error);
    }
    if let Some(path) = report.kept_nonempty.first() {
        return Err(format!("{path} still contains files outside the sync plan"));
    }
    if let Some(path) = report.kept_present.first() {
        return Err(format!("{path} is still present on the source side"));
    }
    Ok(())
}

fn report_initial(initial: &mut Option<InitialSync>, outcome: Result<(), String>) {
    if let Some(initial) = initial.take() {
        let _ = initial.send(outcome);
    }
}

#[derive(Clone)]
enum GuestTransport {
    Ssh(Vec<String>),
    #[cfg(unix)]
    Native {
        executable: PathBuf,
        dir: PathBuf,
        desktop: bool,
        cancelled: Arc<AtomicBool>,
    },
}

impl GuestTransport {
    fn cancellation(&self) -> Option<&Arc<AtomicBool>> {
        match self {
            Self::Ssh(_) => None,
            #[cfg(unix)]
            Self::Native { cancelled, .. } => Some(cancelled),
        }
    }
}

pub struct SyncManager {
    dir: PathBuf,
    projects: Mutex<Vec<SyncProject>>,
    registry_error: Option<String>,
    statuses: Mutex<HashMap<String, SyncStatus>>,
    sessions: Mutex<HashMap<String, Session>>,
    guest: RwLock<Option<GuestTransport>>,
    transitions: Mutex<()>,
    #[cfg(unix)]
    native: RwLock<Option<(String, GuestTransport)>>,
    sink: RwLock<Option<Sink>>,
    /// One cycle per project at a time.
    cycles: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Reuse one SSH transport for the short sync commands in a project. Without
/// this every scan/apply pays a full handshake; the watcher already keeps its
/// own long-lived connection. A local shell used by tests is left untouched.
fn reuse_ssh(mut ssh: Vec<String>, dir: &Path) -> Vec<String> {
    use std::hash::{Hash, Hasher};
    // Windows OpenSSH has no ControlMaster: plain connections are used.
    if cfg!(windows) {
        return ssh;
    }
    let is_ssh = ssh
        .first()
        .and_then(|program| Path::new(program).file_name())
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "ssh" || name.ends_with("-ssh"));
    if !is_ssh || ssh.iter().any(|arg| arg.contains("ControlMaster")) {
        return ssh;
    }
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut hasher);
    // A recreated machine may use another port or key; do not reuse a master
    // that still points at the previous one.
    ssh.hash(&mut hasher);
    // The per-user temp directory, not a world-writable `/tmp`: another local
    // account must not be able to plant the control socket.
    #[cfg(unix)]
    let control = std::env::temp_dir()
        .join(format!("cabin-sync-{:016x}.sock", hasher.finish()))
        .to_string_lossy()
        .into_owned();
    #[cfg(not(unix))]
    let control = format!("/tmp/cabin-sync-{:016x}.sock", hasher.finish());
    let options = [
        "-o".to_string(),
        "ControlMaster=auto".to_string(),
        "-o".to_string(),
        format!("ControlPath={control}"),
        "-o".to_string(),
        "ControlPersist=30".to_string(),
    ];
    ssh.splice(1..1, options);
    ssh
}

fn write_json_atomic(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(err)?;
    agent_core::fsx::atomic_write(path, &bytes, Some(0o600)).map_err(err)
}

fn read_projects(path: &Path) -> Result<Vec<SyncProject>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let projects: Vec<SyncProject> = serde_json::from_slice(&bytes).map_err(err)?;
    let mut ids = std::collections::HashSet::new();
    for project in &projects {
        let id = Path::new(&project.id);
        if !matches!(id.components().next(), Some(std::path::Component::Normal(name)) if name == id.as_os_str())
            || !ids.insert(&project.id)
        {
            return Err("invalid or duplicate project ID".into());
        }
    }
    Ok(projects)
}

fn offline(id: &str) -> SyncStatus {
    SyncStatus {
        id: id.to_string(),
        state: SyncState::Offline,
        error: None,
        last_sync_at: None,
        files: 0,
        pushed: 0,
        pulled: 0,
        conflicts: Vec::new(),
    }
}

/// The folder whose listing a path belongs to (`""` for the root).
fn parent_of(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(dir, _)| dir.to_string())
        .unwrap_or_default()
}

fn guest_folder(path: &str) -> Result<String, String> {
    let path = path.trim().trim_end_matches('/');
    let relative = path.strip_prefix("~/").or_else(|| path.strip_prefix('/'));
    if !relative.is_some_and(|relative| {
        !relative.is_empty()
            && relative.split('/').all(|part| {
                !part.is_empty() && part != "." && part != ".." && !part.contains(['\0', '\\'])
            })
    }) {
        return Err("Choose a folder in the machine, such as ~/projects/app".into());
    }
    Ok(path.to_owned())
}

fn listing(entries: Vec<Entry>) -> Listing {
    // The map key owns the path so a scan holds only one string per file.
    entries
        .into_iter()
        .map(|mut entry| (std::mem::take(&mut entry.path), entry))
        .collect()
}

impl SyncManager {
    pub fn open(dir: PathBuf) -> Arc<Self> {
        let (projects, registry_error) = match read_projects(&dir.join("projects.json")) {
            Ok(projects) => (projects, None),
            Err(error) => {
                let error = format!("Could not read the sync project registry: {error}");
                cabin_diagnostics::event(
                    cabin_diagnostics::Level::Error,
                    "sync",
                    "project_registry_unreadable",
                    format_args!("{error}"),
                );
                (Vec::new(), Some(error))
            }
        };
        let statuses = projects
            .iter()
            .map(|p| {
                let mut status = offline(&p.id);
                if p.paused {
                    status.state = SyncState::Paused;
                }
                (p.id.clone(), status)
            })
            .collect();
        Arc::new(Self {
            dir,
            projects: Mutex::new(projects),
            registry_error,
            statuses: Mutex::new(statuses),
            sessions: Mutex::new(HashMap::new()),
            guest: RwLock::new(None),
            transitions: Mutex::new(()),
            #[cfg(unix)]
            native: RwLock::new(None),
            sink: RwLock::new(None),
            cycles: Mutex::new(HashMap::new()),
        })
    }

    pub fn set_sink(&self, sink: impl Fn(AppEvent) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }

    // ------------------------------------------------------------ state

    pub(crate) fn project(&self, id: &str) -> Result<SyncProject, String> {
        self.require_registry()?;
        self.projects
            .locked()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| "This synced project no longer exists".to_string())
    }

    pub(crate) fn require_registry(&self) -> Result<(), String> {
        match &self.registry_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn save_projects(&self, projects: &[SyncProject]) -> Result<(), String> {
        self.require_registry()?;
        write_json_atomic(&self.dir.join("projects.json"), &projects)
    }

    fn edit_project(
        &self,
        id: &str,
        change: impl FnOnce(&mut SyncProject),
    ) -> Result<SyncProject, String> {
        let mut projects = self.projects.locked();
        let mut next = projects.clone();
        let project = next
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or("This synced project no longer exists")?;
        change(project);
        let updated = project.clone();
        self.save_projects(&next)?;
        *projects = next;
        Ok(updated)
    }

    fn base_path(&self, id: &str) -> PathBuf {
        self.dir.join(id).join("base.bin")
    }

    fn load_base(&self, id: &str) -> Result<Base, String> {
        agent_core::sync::base::read(&self.base_path(id))
            .map(|base| base.unwrap_or_default())
            .map_err(|error| format!("Could not read the sync base: {error}"))
    }

    fn save_base(&self, id: &str, base: &Base) -> Result<(), String> {
        agent_core::sync::base::write(&self.base_path(id), base).map_err(err)
    }

    fn status(&self, id: &str) -> SyncStatus {
        self.statuses
            .locked()
            .get(id)
            .cloned()
            .unwrap_or_else(|| offline(id))
    }

    fn set_status(&self, id: &str, change: impl FnOnce(&mut SyncStatus)) {
        let status = {
            let mut statuses = self.statuses.locked();
            let Some(status) = statuses.get_mut(id) else {
                return;
            };
            change(status);
            status.clone()
        };
        let sink = self.sink.read_lock().clone();
        if let Some(sink) = sink {
            sink(AppEvent::Sync { status });
        }
    }

    fn guest(&self) -> Option<GuestTransport> {
        self.guest.read_lock().clone()
    }

    fn guest_for(&self, id: &str) -> Option<GuestTransport> {
        #[cfg(unix)]
        if let Some((_, transport)) = self
            .native
            .read_lock()
            .as_ref()
            .filter(|(project, _)| project == id)
        {
            return Some(transport.clone());
        }
        #[cfg(not(unix))]
        let _ = id;
        self.guest()
    }

    fn native_active(&self, id: &str) -> bool {
        #[cfg(unix)]
        return self
            .native
            .read_lock()
            .as_ref()
            .is_some_and(|(project, _)| project == id);
        #[cfg(not(unix))]
        {
            let _ = id;
            false
        }
    }

    fn require_no_native(&self, id: &str) -> Result<(), String> {
        if self.native_active(id) {
            Err("Wait for the Cabin CLI session before changing this sync project".into())
        } else {
            Ok(())
        }
    }

    // ------------------------------------------------------------ commands

    pub fn list(&self) -> Vec<SyncView> {
        let projects = self.projects.locked().clone();
        projects
            .into_iter()
            .map(|project| SyncView {
                status: self.status(&project.id),
                project,
            })
            .collect()
    }

    pub fn create(self: &Arc<Self>, input: SyncProjectInput) -> Result<SyncView, String> {
        self.create_project(input, true)
    }

    #[cfg(unix)]
    pub(crate) fn create_cli_binding(
        self: &Arc<Self>,
        input: SyncProjectInput,
    ) -> Result<SyncView, String> {
        self.create_project(input, false)
    }

    pub(crate) fn active_native_project(&self) -> Option<String> {
        #[cfg(unix)]
        {
            self.native.read_lock().as_ref().map(|(id, _)| id.clone())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn create_project(
        self: &Arc<Self>,
        input: SyncProjectInput,
        watch: bool,
    ) -> Result<SyncView, String> {
        let _transition = self.transitions.locked();
        let host = agent_core::sync::strip_verbatim_prefix(
            &agent_core::sync::expand_home(input.host_path.trim())
                .canonicalize()
                .map_err(|e| format!("{}: {e}", input.host_path))?,
        );
        if !host.is_dir() || host.parent().is_none() {
            return Err("Choose a folder on this computer".into());
        }
        let guest = guest_folder(&input.guest_path)?;
        let host = host
            .to_str()
            .ok_or("The project folder name must be valid UTF-8")?
            .to_owned();
        let project = {
            let mut projects = self.projects.locked();
            // Compared by path component, so Windows backslashes and guest
            // slashes both work.
            let nested =
                |a: &str, b: &str| Path::new(a).starts_with(b) || Path::new(b).starts_with(a);
            if let Some(other) = projects.iter().find(|p| nested(&p.host_path, &host)) {
                return Err(format!(
                    "{host} overlaps the synced folder of “{}”",
                    other.name
                ));
            }
            if let Some(other) = projects.iter().find(|p| nested(&p.guest_path, &guest)) {
                return Err(format!(
                    "{guest} overlaps the machine folder of “{}”",
                    other.name
                ));
            }
            let project = SyncProject {
                id: uuid_like(),
                workspace_id: input.workspace_id,
                name: input.name.trim().to_string(),
                host_path: host,
                guest_path: guest,
                mode: input.mode,
                rules: input.rules,
                paused: false,
                git_copied: false,
                created_at: now_millis(),
            };
            let mut next = projects.clone();
            next.push(project.clone());
            self.save_projects(&next)?;
            *projects = next;
            project
        };
        self.statuses
            .locked()
            .insert(project.id.clone(), offline(&project.id));
        self.set_status(&project.id, |_| {});
        if watch && self.guest().is_some() {
            self.start(&project.id);
        }
        Ok(SyncView {
            status: self.status(&project.id),
            project,
        })
    }

    pub fn update(
        self: &Arc<Self>,
        id: &str,
        mode: Option<SyncMode>,
        rules: Option<SyncRules>,
        paused: Option<bool>,
    ) -> Result<SyncView, String> {
        let _transition = self.transitions.locked();
        self.require_no_native(id)?;
        self.project(id)?;
        let cycle = self.cycle_lock(id);
        let _idle = cycle
            .try_lock()
            .map_err(|_| "The project is syncing. Retry after the current transfer finishes.")?;
        let project = self.edit_project(id, |p| {
            if let Some(mode) = mode {
                p.mode = mode;
            }
            if let Some(rules) = rules {
                p.rules = rules;
            }
            if let Some(paused) = paused {
                p.paused = paused;
            }
        })?;
        self.stop(id);
        if project.paused {
            self.set_status(id, |s| s.state = SyncState::Paused);
        } else if self.guest().is_some() {
            self.start(id);
        } else {
            self.set_status(id, |s| s.state = SyncState::Offline);
        }
        Ok(SyncView {
            status: self.status(id),
            project,
        })
    }

    /// Stop syncing; files stay where they are on both sides.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        let _transition = self.transitions.locked();
        self.require_no_native(id)?;
        let project = self.project(id)?;
        let cycle = self.cycle_lock(&project.id);
        let _idle = cycle
            .try_lock()
            .map_err(|_| "The project is syncing. Retry after the current transfer finishes.")?;
        {
            let mut projects = self.projects.locked();
            let mut next = projects.clone();
            next.retain(|p| p.id != project.id);
            self.save_projects(&next)?;
            *projects = next;
        }
        self.stop(&project.id);
        self.statuses.locked().remove(&project.id);
        self.cycles.locked().remove(&project.id);
        if let Err(error) = std::fs::remove_dir_all(self.dir.join(&project.id))
            && error.kind() != std::io::ErrorKind::NotFound
        {
            cabin_diagnostics::event(
                cabin_diagnostics::Level::Warn,
                "sync",
                "project_state_cleanup_failed",
                format_args!("{error}"),
            );
        }
        Ok(())
    }

    pub fn sync_now(self: &Arc<Self>, id: &str) -> Result<(), String> {
        self.project(id)?;
        let sessions = self.sessions.locked();
        match sessions.get(id) {
            Some(session) => session.kick.send(Kick::Full),
            None => Err("The machine is not connected, or this project is paused".into()),
        }
    }

    /// Settle a conflict: keep the Mac's or the machine's version.
    pub fn resolve(self: &Arc<Self>, id: &str, path: &str, keep_mac: bool) -> Result<(), String> {
        let _transition = self.transitions.locked();
        self.require_no_native(id)?;
        let project = self.project(id)?;
        guest_folder(&project.guest_path)?;
        let guest = self
            .guest_for(&project.id)
            .ok_or("The machine is not connected")?;
        agent_core::sync::safe_relative(path).ok_or("Not a path in this project")?;
        let host = PathBuf::from(&project.host_path);
        let scope = vec![parent_of(path)];
        let pending_path = self.dir.join(&project.id).join("pending.json");
        {
            let lock = self.cycle_lock(id);
            let _guard = lock.locked();
            check_folder(&host, !keep_mac).require(&host).map_err(err)?;
            let scope_ref = if cfg!(windows) {
                None
            } else {
                Some(scope.as_slice())
            };
            let guest_entries =
                remote_scan(&guest, &project.guest_path, &project.rules, scope_ref)?;
            #[cfg(windows)]
            {
                let ours = scan::scan_metadata(&host, &project.rules, None).map_err(err)?;
                windows_paths::check_names(
                    ours.iter()
                        .chain(&guest_entries)
                        .map(|entry| entry.path.as_str())
                        .chain([path]),
                )?;
                let local = ours.iter().any(|entry| entry.path == path);
                if !local {
                    windows_paths::require_local_absence(&host, path)?;
                }
            }
            let mac = listing(scan::stat(&host, path).map_err(err)?.into_iter().collect());
            let vm = listing(
                guest_entries
                    .into_iter()
                    .filter(|entry| entry.path == path)
                    .collect(),
            );
            let (source, destination) = if keep_mac { (&mac, &vm) } else { (&vm, &mac) };
            let paths = [path.to_string()];
            let (write, delete) = match (source.contains_key(path), destination.contains_key(path))
            {
                (true, _) => (paths.as_slice(), [].as_slice()),
                (false, true) => ([].as_slice(), paths.as_slice()),
                (false, false) => ([].as_slice(), [].as_slice()),
            };
            let checks = ApplyChecks::for_paths(source, destination, write, delete).map_err(err)?;
            let mut pending = Pending::load(&pending_path)?;
            pending.prepare(
                if keep_mac {
                    Destination::Guest
                } else {
                    Destination::Host
                },
                source,
                destination,
                write.iter().chain(delete),
            );
            pending.save(&pending_path)?;
            if keep_mac {
                require_applied(remote_apply(
                    &guest,
                    &project.guest_path,
                    &host,
                    delete,
                    write,
                    Some(checks),
                )?)?;
            } else {
                require_applied(pull_into(
                    &guest,
                    &project.guest_path,
                    &host,
                    delete,
                    write,
                    Some(checks),
                )?)?;
            }
        }
        self.cycle(&project, Some(scope))?.require_transferred()
    }

    // ------------------------------------------------------------ sessions

    /// The machine is connected: sync every project that is not paused.
    pub fn connect(self: &Arc<Self>, ssh: Vec<String>) {
        let _transition = self.transitions.locked();
        let ssh = reuse_ssh(ssh, &self.dir);
        *self.guest.write_lock() = Some(GuestTransport::Ssh(ssh));
        let ids: Vec<String> = self
            .projects
            .locked()
            .iter()
            .filter(|p| !p.paused)
            .map(|p| p.id.clone())
            .collect();
        for id in ids {
            if self.native_active(&id) {
                continue;
            }
            self.start(&id);
        }
    }

    #[cfg(unix)]
    pub(crate) fn connect_native(
        self: &Arc<Self>,
        id: &str,
        executable: PathBuf,
        dir: PathBuf,
        desktop: bool,
    ) -> Result<mpsc::Receiver<Result<(), String>>, String> {
        let _transition = self.transitions.locked();
        if self.native.read_lock().is_some() {
            return Err("Another native Cabin CLI session is active".into());
        }
        let project = self.project(id)?;
        if project.paused || project.mode != SyncMode::TwoWay {
            return Err(
                "Enable Two-way sync for this project before starting the Cabin CLI".into(),
            );
        }
        if let Err(error) = self.join_session(id) {
            self.restore_desktop_sync(id);
            return Err(error);
        }
        *self.native.write_lock() = Some((
            id.into(),
            GuestTransport::Native {
                executable,
                dir,
                desktop,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
        ));
        self.set_status(id, |s| {
            s.state = SyncState::Scanning;
            s.error = None;
        });
        let (sender, receiver) = mpsc::channel();
        if let Err(error) = self.spawn_session(id, Some(sender)) {
            *self.native.write_lock() = None;
            self.restore_desktop_sync(id);
            return Err(error);
        }
        Ok(receiver)
    }

    #[cfg(unix)]
    pub(crate) fn finish_native(self: &Arc<Self>, id: &str, reconcile: bool) -> Result<(), String> {
        let _transition = self.transitions.locked();
        {
            let native = self.native.read_lock();
            let Some((_, transport)) = native.as_ref().filter(|(project, _)| project == id) else {
                return Err("This project has no active Cabin CLI session".into());
            };
            if !reconcile && let Some(cancelled) = transport.cancellation() {
                cancelled.store(true, Ordering::Relaxed);
            }
        }
        let mut result = self.join_session(id);
        if reconcile && result.is_ok() {
            result = self
                .project(id)
                .and_then(|project| self.first_cycle(&project))
                .and_then(CycleOutcome::require_complete);
        }
        *self.native.write_lock() = None;
        self.restore_desktop_sync(id);
        result
    }

    #[cfg(unix)]
    fn restore_desktop_sync(self: &Arc<Self>, id: &str) {
        if self.guest().is_some() {
            self.start(id);
        } else {
            self.set_status(id, |s| s.state = SyncState::Offline);
        }
    }

    #[cfg(unix)]
    fn join_session(&self, id: &str) -> Result<(), String> {
        let session = self.sessions.locked().remove(id);
        if let Some(session) = session {
            session.stop.store(true, Ordering::Relaxed);
            let _ = session.kick.send(Kick::Full);
            session
                .worker
                .join()
                .map_err(|_| "Sync worker stopped unexpectedly")?;
        }
        Ok(())
    }

    pub fn disconnect(&self) {
        let _transition = self.transitions.locked();
        *self.guest.write_lock() = None;
        let ids: Vec<String> = self.sessions.locked().keys().cloned().collect();
        for id in ids {
            if self.native_active(&id) {
                continue;
            }
            self.stop(&id);
            self.set_status(&id, |s| {
                if s.state != SyncState::Paused {
                    s.state = SyncState::Offline;
                }
            });
        }
    }

    pub fn overlaps_host_path(&self, path: &Path) -> Result<bool, String> {
        self.require_registry()?;
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        Ok(self.projects.locked().iter().any(|project| {
            let folder = Path::new(&project.host_path);
            folder.starts_with(&path) || path.starts_with(folder)
        }))
    }

    /// Hold the project registry and get exclusive access to every project
    /// before removing persisted sync state. An active transfer can finish
    /// and the user can retry the reset without racing that transfer.
    pub fn reset_storage<T>(
        &self,
        erase: impl FnOnce() -> vm_core::Result<T>,
    ) -> vm_core::Result<T> {
        let _transition = self.transitions.locked();
        self.require_registry().map_err(vm_core::Error::Message)?;
        #[cfg(unix)]
        if self.native.read_lock().is_some() {
            return vm_core::message("Exit the Cabin CLI session before erasing the machine.");
        }
        let mut projects = self.projects.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(data) = self.dir.parent().and_then(|path| path.canonicalize().ok())
            && let Some(project) = projects
                .iter()
                .find(|project| Path::new(&project.host_path).starts_with(&data))
        {
            return vm_core::message(format!(
                "The synced Mac folder {} is inside the app data directory. Move it out before erasing the machine.",
                project.host_path
            ));
        }
        let locks: Vec<_> = projects
            .iter()
            .map(|project| self.cycle_lock(&project.id))
            .collect();
        let _idle = locks
            .iter()
            .map(|lock| lock.try_lock())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| {
                vm_core::Error::Message(
                    "A folder sync is finishing. Retry the reset shortly.".into(),
                )
            })?;
        let result = erase()?;
        projects.clear();
        self.statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        Ok(result)
    }

    fn cycle_lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.cycles
            .locked()
            .entry(id.to_string())
            .or_default()
            .clone()
    }

    fn require_current_project(&self, project: &SyncProject) -> Result<(), String> {
        let current = self.project(&project.id)?;
        if current.paused || current.mode != project.mode || current.rules != project.rules {
            return Err("The project sync configuration changed".into());
        }
        guest_folder(&project.guest_path)?;
        Ok(())
    }

    fn stop(&self, id: &str) {
        if let Some(session) = self.sessions.locked().remove(id) {
            session.stop.store(true, Ordering::Relaxed);
            let _ = session.kick.send(Kick::Full);
        }
    }

    fn start(self: &Arc<Self>, id: &str) {
        if let Err(error) = self.spawn_session(id, None) {
            self.set_status(id, |status| {
                status.state = SyncState::Error;
                status.error = Some(error);
            });
        }
    }

    fn spawn_session(
        self: &Arc<Self>,
        id: &str,
        initial: Option<InitialSync>,
    ) -> Result<(), String> {
        self.stop(id);
        let guest = self.guest_for(id).ok_or("The machine is not connected")?;
        let stop = Arc::new(AtomicBool::new(false));
        let (sender, kicks) = mpsc::sync_channel(KICK_QUEUE);
        let kick = Kicks {
            sender,
            rescan: Arc::new(AtomicBool::new(false)),
        };
        let mut sessions = self.sessions.locked();
        let me = self.clone();
        let worker_id = id.to_string();
        let worker_stop = stop.clone();
        let worker_kick = kick.clone();
        let worker = std::thread::Builder::new()
            .name(format!("sync-{id}"))
            .spawn(move || {
                me.run(
                    &worker_id,
                    &worker_stop,
                    &guest,
                    worker_kick,
                    kicks,
                    initial,
                )
            })
            .map_err(|error| format!("Could not start project sync: {error}"))?;
        #[cfg(not(unix))]
        drop(worker);
        sessions.insert(
            id.to_string(),
            Session {
                stop,
                kick,
                #[cfg(unix)]
                worker,
            },
        );
        Ok(())
    }

    fn run(
        self: Arc<Self>,
        id: &str,
        stop: &AtomicBool,
        guest: &GuestTransport,
        kick: Kicks,
        kicks: mpsc::Receiver<Kick>,
        mut initial: Option<InitialSync>,
    ) {
        let project = match self.project(id).and_then(|project| {
            guest_folder(&project.guest_path)?;
            Ok(project)
        }) {
            Ok(project) => project,
            Err(error) => {
                report_initial(&mut initial, Err(error.clone()));
                if !stop.load(Ordering::Relaxed) {
                    self.set_status(id, |status| {
                        status.state = SyncState::Error;
                        status.error = Some(error);
                    });
                }
                return;
            }
        };
        let host = PathBuf::from(&project.host_path);
        while !stop.load(Ordering::Relaxed) {
            let Err(error) = check_folder(&host, project.mode == SyncMode::TwoWay).require(&host)
            else {
                break;
            };
            report_initial(&mut initial, Err(error.message.clone()));
            self.set_status(id, |s| {
                s.state = SyncState::Error;
                s.error = Some(error.message);
            });
            // Recheck after a manual sync or after consent has had time to change.
            if matches!(
                kicks.recv_timeout(Duration::from_secs(15)),
                Err(RecvTimeoutError::Disconnected)
            ) {
                return;
            }
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        self.set_status(id, |s| {
            s.state = SyncState::Scanning;
            s.error = None;
        });
        // Watch before the first scan, so nothing changed during it is missed.
        // Dropping `_mac_watcher` at the end of the session ends the forwarder.
        let _mac_watcher = match agent_core::sync::watch::watch(&host, &project.rules).and_then(
            |agent_core::sync::watch::Batches { watcher, batches }| {
                let kick = kick.clone();
                std::thread::Builder::new()
                    .name("sync-host-watch".into())
                    .spawn(move || {
                        for batch in batches {
                            if kick.send(Kick::Scope(batch)).is_err() {
                                break;
                            }
                        }
                    })
                    .map_err(std::io::Error::other)?;
                Ok(watcher)
            },
        ) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                self.set_status(id, |s| {
                    s.error = Some(format!(
                        "Mac changes are picked up every few minutes only: {error}"
                    ))
                });
                None
            }
        };
        let mut guest_watch = None;
        if project.mode == SyncMode::TwoWay && !stop.load(Ordering::Relaxed) {
            match watch_guest(guest, &project, kick.clone()) {
                Ok(watch) => guest_watch = Some(watch),
                Err(_) if matches!(guest, GuestTransport::Ssh(_)) => {}
                Err(error) => {
                    let message = format!("Guest watcher could not start: {error}");
                    report_initial(&mut initial, Err(message.clone()));
                    self.set_status(id, |s| {
                        s.state = SyncState::Error;
                        s.error = Some(message);
                    });
                    return;
                }
            }
        }

        let mut failed = match self.first_cycle_on(&project, guest, Some(stop)) {
            Ok(outcome) => {
                let failed = outcome.transfer_error.is_some();
                report_initial(&mut initial, outcome.require_complete());
                failed
            }
            Err(error) => {
                report_initial(&mut initial, Err(error.clone()));
                if !stop.load(Ordering::Relaxed) {
                    self.set_status(id, |s| {
                        s.state = SyncState::Error;
                        s.error = Some(error);
                    });
                }
                true
            }
        };
        let mut last_full = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let next = match kicks.recv_timeout(Duration::from_secs(1)) {
                Ok(kick) => Some(kick),
                Err(RecvTimeoutError::Timeout) => {
                    let interval = if failed { RETRY_EVERY } else { FULL_EVERY };
                    (kick.take_rescan() || last_full.elapsed() >= interval).then_some(Kick::Full)
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let Some(next) = next else { continue };
            if stop.load(Ordering::Relaxed) {
                break;
            }
            // Fold everything already queued into one cycle.
            let mut scope = match next {
                Kick::Full => None,
                Kick::Scope(dirs) => Some(dirs),
            };
            for _ in 0..KICK_QUEUE {
                let Ok(more) = kicks.try_recv() else { break };
                match (&mut scope, more) {
                    (Some(dirs), Kick::Scope(extra)) => dirs.extend(extra),
                    (_, _) => scope = None,
                }
            }
            let rescan = kick.take_rescan();
            let scope = scope
                .filter(|_| !rescan)
                .map(|dirs| agent_core::sync::watch::collapse(dirs.into_iter().collect()))
                .filter(|dirs| dirs != &[String::new()]);
            let Ok(project) = self.project(id) else { break };
            if failed || project_requires_full_scan(&project) || scope.is_none() {
                last_full = Instant::now();
            }
            let result = if failed {
                self.first_cycle_on(&project, guest, Some(stop))
            } else {
                self.cycle_on(&project, scope, guest, Some(stop))
            };
            failed = match result {
                Ok(outcome) => outcome.transfer_error.is_some(),
                Err(error) => {
                    if !stop.load(Ordering::Relaxed) {
                        self.set_status(id, |s| {
                            s.state = SyncState::Error;
                            s.error = Some(error);
                        });
                    }
                    true
                }
            };
        }
        drop(guest_watch);
    }
}

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("sync-{:x}", nanos ^ (std::process::id() as u128) << 64)
}

#[cfg(test)]
mod tests;
