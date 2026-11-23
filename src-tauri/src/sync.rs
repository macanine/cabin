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
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use agent_core::model::{AppEvent, SyncProject, SyncProjectInput, SyncState, SyncStatus, SyncView};
use agent_core::sync::archive::{self, ApplyHeader, ApplyReport};
use agent_core::sync::plan::{self, Action, BaseRecord};
use agent_core::sync::scan::{self, Entry};
use agent_core::sync::{GitHandling, PackRequest, ScanRequest, SyncMode, SyncRules};
use agent_core::util::now_millis;

use crate::host_permissions::check_folder;

#[cfg(any(windows, test))]
mod windows_paths;

type Base = BTreeMap<String, BaseRecord>;
type Sink = Arc<dyn Fn(AppEvent) + Send + Sync>;

/// A full cycle at least this often, whatever the watchers say.
const FULL_EVERY: Duration = Duration::from_secs(600);
const RETRY_EVERY: Duration = Duration::from_secs(30);

fn project_requires_full_scan(project: &SyncProject) -> bool {
    cfg!(windows) && project.mode == SyncMode::TwoWay
}

enum Kick {
    Scope(Vec<String>),
    Full,
}

struct Session {
    stop: Arc<AtomicBool>,
    kick: mpsc::Sender<Kick>,
}

pub struct SyncManager {
    dir: PathBuf,
    projects: Mutex<Vec<SyncProject>>,
    statuses: Mutex<HashMap<String, SyncStatus>>,
    sessions: Mutex<HashMap<String, Session>>,
    /// SSH argv into the machine while it is connected.
    guest: RwLock<Option<Vec<String>>>,
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
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(err)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(value).map_err(err)?).map_err(err)?;
    std::fs::rename(&tmp, path).map_err(err)
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
    path.rsplit_once('/').map(|(dir, _)| dir.to_string()).unwrap_or_default()
}

impl SyncManager {
    pub fn open(dir: PathBuf) -> Arc<Self> {
        let projects: Vec<SyncProject> = std::fs::read(dir.join("projects.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
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
            statuses: Mutex::new(statuses),
            sessions: Mutex::new(HashMap::new()),
            guest: RwLock::new(None),
            sink: RwLock::new(None),
            cycles: Mutex::new(HashMap::new()),
        })
    }

    pub fn set_sink(&self, sink: impl Fn(AppEvent) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }

    // ------------------------------------------------------------ state

    pub(crate) fn project(&self, id: &str) -> Result<SyncProject, String> {
        self.projects.locked()
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| "This synced project no longer exists".to_string())
    }

    fn save_projects(&self, projects: &[SyncProject]) -> Result<(), String> {
        write_json_atomic(&self.dir.join("projects.json"), &projects)
    }

    fn edit_project(&self, id: &str, change: impl FnOnce(&mut SyncProject)) -> Result<SyncProject, String> {
        let mut projects = self.projects.locked();
        let mut next = projects.clone();
        let project = next.iter_mut().find(|p| p.id == id).ok_or("This synced project no longer exists")?;
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
        self.statuses.locked()
            .get(id)
            .cloned()
            .unwrap_or_else(|| offline(id))
    }

    fn set_status(&self, id: &str, change: impl FnOnce(&mut SyncStatus)) {
        let status = {
            let mut statuses = self.statuses.locked();
            let status = statuses.entry(id.to_string()).or_insert_with(|| offline(id));
            change(status);
            status.clone()
        };
        let sink = self.sink.read_lock().clone();
        if let Some(sink) = sink {
            sink(AppEvent::Sync { status });
        }
    }

    fn guest(&self) -> Option<Vec<String>> {
        self.guest.read_lock().clone()
    }

    // ------------------------------------------------------------ commands

    pub fn list(&self) -> Vec<SyncView> {
        let projects = self.projects.locked().clone();
        projects
            .into_iter()
            .map(|project| SyncView { status: self.status(&project.id), project })
            .collect()
    }

    pub fn create(self: &Arc<Self>, input: SyncProjectInput) -> Result<SyncView, String> {
        let host = agent_core::sync::expand_home(input.host_path.trim())
            .canonicalize()
            .map_err(|e| format!("{}: {e}", input.host_path))?;
        if !host.is_dir() || host.parent().is_none() {
            return Err("Choose a folder on this Mac".into());
        }
        let guest = input.guest_path.trim().trim_end_matches('/').to_string();
        if guest.is_empty() || guest == "~" || !(guest.starts_with('/') || guest.starts_with("~/")) {
            return Err("Choose a folder in the machine, such as ~/projects/app".into());
        }
        let host = host.to_string_lossy().into_owned();
        let project = {
            let mut projects = self.projects.locked();
            // Compared by path component, so Windows backslashes and guest
            // slashes both work.
            let nested = |a: &str, b: &str| Path::new(a).starts_with(b) || Path::new(b).starts_with(a);
            if let Some(other) = projects.iter().find(|p| nested(&p.host_path, &host)) {
                return Err(format!("{host} overlaps the synced folder of “{}”", other.name));
            }
            if let Some(other) = projects.iter().find(|p| nested(&p.guest_path, &guest)) {
                return Err(format!("{guest} overlaps the machine folder of “{}”", other.name));
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
        self.set_status(&project.id, |s| *s = offline(&project.id));
        if self.guest().is_some() {
            self.start(&project.id);
        }
        Ok(SyncView { status: self.status(&project.id), project })
    }

    pub fn update(
        self: &Arc<Self>,
        id: &str,
        mode: Option<SyncMode>,
        rules: Option<SyncRules>,
        paused: Option<bool>,
    ) -> Result<SyncView, String> {
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
        Ok(SyncView { status: self.status(id), project })
    }

    /// Stop syncing; files stay where they are on both sides.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        {
            let mut projects = self.projects.locked();
            let mut next = projects.clone();
            next.retain(|p| p.id != id);
            self.save_projects(&next)?;
            *projects = next;
        }
        self.stop(id);
        self.statuses.locked().remove(id);
        let _ = std::fs::remove_dir_all(self.dir.join(id));
        Ok(())
    }

    pub fn sync_now(self: &Arc<Self>, id: &str) -> Result<(), String> {
        self.project(id)?;
        let sessions = self.sessions.locked();
        match sessions.get(id) {
            Some(session) => {
                let _ = session.kick.send(Kick::Full);
                Ok(())
            }
            None => Err("The machine is not connected, or this project is paused".into()),
        }
    }

    /// Settle a conflict: keep the Mac's or the machine's version.
    pub fn resolve(self: &Arc<Self>, id: &str, path: &str, keep_mac: bool) -> Result<(), String> {
        let project = self.project(id)?;
        let guest = self.guest().ok_or("The machine is not connected")?;
        agent_core::sync::safe_relative(path).ok_or("Not a path in this project")?;
        let host = PathBuf::from(&project.host_path);
        {
            let lock = self.cycle_lock(id);
            let _guard = lock.locked();
            check_folder(&host, !keep_mac).require(&host).map_err(err)?;
            let scope = vec![parent_of(path)];
            #[cfg(windows)]
            {
                let ours = scan::scan(&host, &project.rules, None).map_err(err)?;
                let theirs = remote_scan(&guest, &project.guest_path, &project.rules, None)?;
                windows_paths::check_names(ours.iter().chain(&theirs).map(|entry| entry.path.as_str()).chain([path]))?;
                let local = ours.iter().any(|entry| entry.path == path);
                if !local {
                    windows_paths::require_local_absence(&host, path)?;
                }
            }
            if keep_mac {
                let (push, delete) = match scan::stat(&host, path).map_err(err)? {
                    Some(_) => (vec![path.to_string()], vec![]),
                    None => (vec![], vec![path.to_string()]),
                };
                remote_apply(&guest, &project.guest_path, &host, &delete, &push)?;
            } else {
                let vm = remote_scan(&guest, &project.guest_path, &project.rules, Some(&scope))?;
                if vm.iter().any(|e| e.path == path) {
                    pull(&guest, &project.guest_path, &host, &[], &[path.to_string()])?;
                } else {
                    archive::apply(&host, &[path.to_string()], std::io::empty()).map_err(err)?;
                }
            }
        }
        self.cycle(&project, Some(vec![parent_of(path)]))
    }

    // ------------------------------------------------------------ sessions

    /// The machine is connected: sync every project that is not paused.
    pub fn connect(self: &Arc<Self>, ssh: Vec<String>) {
        let ssh = reuse_ssh(ssh, &self.dir);
        *self.guest.write_lock() = Some(ssh);
        let ids: Vec<String> = self
            .projects.locked()
            .iter()
            .filter(|p| !p.paused)
            .map(|p| p.id.clone())
            .collect();
        for id in ids {
            self.start(&id);
        }
    }

    pub fn disconnect(&self) {
        *self.guest.write_lock() = None;
        let ids: Vec<String> = self.sessions.locked().keys().cloned().collect();
        for id in ids {
            self.stop(&id);
            self.set_status(&id, |s| {
                if s.state != SyncState::Paused {
                    s.state = SyncState::Offline;
                }
            });
        }
    }

    pub fn overlaps_host_path(&self, path: &Path) -> bool {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        self.projects.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|project| {
            let folder = Path::new(&project.host_path);
            folder.starts_with(&path) || path.starts_with(folder)
        })
    }

    /// Hold the project registry and get exclusive access to every project
    /// before removing persisted sync state. An active transfer can finish
    /// and the user can retry the reset without racing that transfer.
    pub fn reset_storage<T>(&self, erase: impl FnOnce() -> vm_core::Result<T>) -> vm_core::Result<T> {
        let mut projects = self.projects.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(data) = self.dir.parent().and_then(|path| path.canonicalize().ok())
            && let Some(project) = projects.iter().find(|project| Path::new(&project.host_path).starts_with(&data))
        {
            return vm_core::message(format!(
                "The synced Mac folder {} is inside the app data directory. Move it out before erasing the machine.",
                project.host_path
            ));
        }
        let locks: Vec<_> = projects.iter().map(|project| self.cycle_lock(&project.id)).collect();
        let _idle = locks.iter().map(|lock| lock.try_lock()).collect::<Result<Vec<_>, _>>()
            .map_err(|_| vm_core::Error::Message("A folder sync is finishing. Retry the reset shortly.".into()))?;
        let result = erase()?;
        projects.clear();
        self.statuses.lock().unwrap_or_else(|p| p.into_inner()).clear();
        Ok(result)
    }

    fn cycle_lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.cycles.locked().entry(id.to_string()).or_default().clone()
    }

    fn stop(&self, id: &str) {
        if let Some(session) = self.sessions.locked().remove(id) {
            session.stop.store(true, Ordering::Relaxed);
        }
    }

    fn start(self: &Arc<Self>, id: &str) {
        self.stop(id);
        let stop = Arc::new(AtomicBool::new(false));
        let (kick, kicks) = mpsc::channel();
        self.sessions.locked()
            .insert(id.to_string(), Session { stop: stop.clone(), kick: kick.clone() });
        let me = self.clone();
        let id = id.to_string();
        std::thread::Builder::new()
            .name(format!("sync-{id}"))
            .spawn(move || me.run(&id, &stop, kick, kicks))
            .ok();
    }

    fn run(self: Arc<Self>, id: &str, stop: &AtomicBool, kick: mpsc::Sender<Kick>, kicks: mpsc::Receiver<Kick>) {
        let Ok(project) = self.project(id) else { return };
        let host = PathBuf::from(&project.host_path);
        while !stop.load(Ordering::Relaxed) {
            let Err(error) = check_folder(&host, project.mode == SyncMode::TwoWay).require(&host) else {
                break;
            };
            self.set_status(id, |s| {
                s.state = SyncState::Error;
                s.error = Some(error.message);
            });
            // Recheck after a manual sync or after consent has had time to change.
            if matches!(kicks.recv_timeout(Duration::from_secs(15)), Err(RecvTimeoutError::Disconnected)) {
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
        let _mac_watcher = match agent_core::sync::watch::watch(&host, &project.rules) {
            Ok(agent_core::sync::watch::Batches { watcher, batches }) => {
                let kick = kick.clone();
                std::thread::spawn(move || {
                    for batch in batches {
                        if kick.send(Kick::Scope(batch)).is_err() {
                            break;
                        }
                    }
                });
                Some(watcher)
            }
            Err(error) => {
                self.set_status(id, |s| s.error = Some(format!("Mac changes are picked up every few minutes only: {error}")));
                None
            }
        };
        let mut guest_watch = (project.mode == SyncMode::TwoWay)
            .then(|| self.guest().and_then(|ssh| watch_guest(&ssh, &project, kick.clone()).ok()))
            .flatten();

        let mut failed = false;
        if let Err(error) = self.first_cycle(&project) {
            failed = true;
            self.set_status(id, |s| {
                s.state = SyncState::Error;
                s.error = Some(error);
            });
        }
        let mut last_full = Instant::now();
        while !stop.load(Ordering::Relaxed) {
            let next = match kicks.recv_timeout(Duration::from_secs(1)) {
                Ok(kick) => Some(kick),
                Err(RecvTimeoutError::Timeout) => {
                    let interval = if failed { RETRY_EVERY } else { FULL_EVERY };
                    (last_full.elapsed() >= interval).then_some(Kick::Full)
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
            while let Ok(more) = kicks.try_recv() {
                match (&mut scope, more) {
                    (Some(dirs), Kick::Scope(extra)) => dirs.extend(extra),
                    (_, _) => scope = None,
                }
            }
            let scope = scope.map(|dirs| agent_core::sync::watch::collapse(dirs.into_iter().collect()))
                .filter(|dirs| dirs != &[String::new()]);
            let Ok(project) = self.project(id) else { break };
            if failed || project_requires_full_scan(&project) || scope.is_none() {
                last_full = Instant::now();
            }
            let result = if failed {
                self.first_cycle(&project)
            } else {
                self.cycle(&project, scope)
            };
            failed = result.is_err();
            if let Err(error) = result {
                self.set_status(id, |s| {
                    s.state = SyncState::Error;
                    s.error = Some(error);
                });
            }
        }
        if let Some(watch) = guest_watch.as_mut() {
            let _ = watch.child.kill();
            let _ = watch.child.wait();
        }
    }

    /// The first cycle of a session: `.git` once if asked, then everything.
    fn first_cycle(&self, project: &SyncProject) -> Result<(), String> {
        if project.rules.git == GitHandling::CopyOnce && !project.git_copied {
            self.copy_git_once(project)?;
        }
        self.cycle(project, None)
    }

    /// Copy `.git` into the machine unless it already has one. From then on
    /// each side keeps its own history (see `GitHandling::CopyOnce`).
    fn copy_git_once(&self, project: &SyncProject) -> Result<(), String> {
        let lock = self.cycle_lock(&project.id);
        let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());
        let host = PathBuf::from(&project.host_path);
        let guest = self.guest().ok_or("The machine is not connected")?;
        check_folder(&host, project.mode == SyncMode::TwoWay).require(&host).map_err(err)?;
        if host.join(".git").is_dir() {
            let scope = vec![".git".to_string()];
            let theirs = remote_scan(&guest, &project.guest_path, &SyncRules::git_only(), Some(&scope))?;
            if !theirs.iter().any(|e| e.path.starts_with(".git/")) {
                let ours = agent_core::sync::scan(&host, &SyncRules::git_only(), Some(&scope)).map_err(err)?;
                let paths: Vec<String> = ours.into_iter().map(|e| e.path).collect();
                let report = remote_apply(&guest, &project.guest_path, &host, &[], &paths)?;
                if let Some((path, error)) = report.failed.first() {
                    return Err(format!("Could not copy .git ({path}: {error})"));
                }
            }
        }
        self.edit_project(&project.id, |p| p.git_copied = true).map(|_| ())
    }

    /// One reconcile of `scope` (`None`: everything).
    fn cycle(&self, project: &SyncProject, scope: Option<Vec<String>>) -> Result<(), String> {
        let lock = self.cycle_lock(&project.id);
        let _guard = lock.locked();
        let guest = self.guest().ok_or("The machine is not connected")?;
        let host = PathBuf::from(&project.host_path);
        check_folder(&host, project.mode == SyncMode::TwoWay).require(&host).map_err(err)?;
        // A scoped scan can miss an alias that differs in a parent directory's
        // spelling. Windows Two-way needs both full namespaces before writing.
        let scope = if project_requires_full_scan(project) { None } else { scope };
        let scope_ref = scope.as_deref();
        macro_rules! mark {
            ($label:expr, $started:expr) => {
                cabin_diagnostics::event(
                    cabin_diagnostics::Level::Debug,
                    "sync",
                    $label,
                    format_args!("{} ms", $started.elapsed().as_millis()),
                );
            };
        }
        self.set_status(&project.id, |s| {
            if s.state == SyncState::Error {
                s.error = None;
            }
            s.state = if scope.is_none() { SyncState::Scanning } else { SyncState::Syncing };
        });

        let to_map = |entries: Vec<Entry>| -> BTreeMap<String, Entry> {
            // The map key owns the path; keeping a second copy inside Entry
            // would allocate and retain one String per file for every cycle.
            entries
                .into_iter()
                .map(|mut entry| (std::mem::take(&mut entry.path), entry))
                .collect()
        };
        let (mac, vm, base_in, base_out, files) = if project.mode == SyncMode::Safe {
            let started = Instant::now();
            let mac = to_map(agent_core::sync::scan(&host, &project.rules, scope_ref).map_err(err)?);
            mark!("mac_scan", started);
            let started = Instant::now();
            let base_all = self.load_base(&project.id)?;
            mark!("base_load", started);
            let files = base_all.len() as u64;
            let (base_in, base_out): (Base, Base) = base_all
                .into_iter()
                .partition(|(path, _)| scan::in_scope(path, scope_ref));
            // Safe mode only pushes Mac changes. If the Mac matches the base,
            // there is nothing to push and no conflict to discover, so the
            // guest scan and the base rewrite can be skipped entirely.
            if !plan::mac_changed(&base_in, &mac) {
                self.set_status(&project.id, |s| {
                    s.state = SyncState::Watching;
                    s.last_sync_at = Some(now_millis());
                    s.files = files;
                });
                return Ok(());
            }
            let started = Instant::now();
            let vm = to_map(remote_scan(&guest, &project.guest_path, &project.rules, scope_ref)?);
            mark!("guest_scan", started);
            (mac, vm, base_in, base_out, files)
        } else {
            // Mirror and Two-way need both sides on every cycle. The Mac walk
            // and the guest walk are independent; run them together.
            let started = Instant::now();
            let (mac_entries, vm_entries) = std::thread::scope(|scope| {
                let vm = std::thread::Builder::new().name("sync-guest-scan".into())
                    .spawn_scoped(scope, || remote_scan(&guest, &project.guest_path, &project.rules, scope_ref))
                    .map_err(|error| format!("Could not start the guest scan: {error}"))?;
                let mac = agent_core::sync::scan(&host, &project.rules, scope_ref).map_err(err);
                let vm = vm.join().map_err(|_| "Guest scan stopped".to_string())?;
                Ok::<_, String>((mac?, vm?))
            })?;
            mark!("parallel_scans", started);
            let started = Instant::now();
            let base_all = self.load_base(&project.id)?;
            mark!("base_load", started);
            let files = base_all.len() as u64;
            let (base_in, base_out): (Base, Base) = base_all
                .into_iter()
                .partition(|(path, _)| scan::in_scope(path, scope_ref));
            (to_map(mac_entries), to_map(vm_entries), base_in, base_out, files)
        };
        // Nothing can be applied if both snapshots still match the base.
        // Skip both planning and Windows alias sorting on this read-only path.
        if self.base_path(&project.id).exists() && plan::base_is_current(&base_in, &mac, &vm) {
            self.set_status(&project.id, |s| {
                s.state = SyncState::Watching;
                s.last_sync_at = Some(now_millis());
                s.files = files;
            });
            return Ok(());
        }
        #[cfg(windows)]
        if project.mode == SyncMode::TwoWay {
            // Baseline-only paths absent on both sides cannot produce actions.
            // Checking the live namespace lets users fix an old collision by
            // renaming the offending file without throwing away sync history.
            windows_paths::check_names(mac.keys().chain(vm.keys()).map(String::as_str))?;
        }
        let started = Instant::now();
        let actions = plan::reconcile(project.mode, &base_in, &mac, &vm);
        mark!("plan", started);

        let mut push = Vec::new();
        let mut delete_vm = Vec::new();
        let mut pull = Vec::new();
        let mut delete_mac = Vec::new();
        let mut conflicts = Vec::new();
        for action in &actions {
            match action {
                Action::Push(p) => push.push(p.clone()),
                Action::DeleteVm(p) => delete_vm.push(p.clone()),
                Action::Pull(p) => pull.push(p.clone()),
                Action::DeleteMac(p) => delete_mac.push(p.clone()),
                Action::Conflict(p) => conflicts.push(p.clone()),
            }
        }
        #[cfg(windows)]
        if project.mode == SyncMode::TwoWay {
            // A scan may omit an ignored spelling or a Windows alias. Do not
            // infer a deletion or allow a new pull to overwrite such a file.
            for path in delete_vm.iter().chain(pull.iter().filter(|path| !mac.contains_key(*path))) {
                windows_paths::require_local_absence(&host, path)?;
            }
        }
        if !push.is_empty() || !delete_vm.is_empty() {
            self.set_status(&project.id, |s| s.state = SyncState::Syncing);
        }

        let mut failed: Vec<(String, String)> = Vec::new();
        let started = Instant::now();
        if !push.is_empty() || !delete_vm.is_empty() {
            let report = remote_apply(&guest, &project.guest_path, &host, &delete_vm, &push)?;
            failed.extend(report.failed);
        }
        if !pull.is_empty() || !delete_mac.is_empty() {
            let report = pull_into(&guest, &project.guest_path, &host, &delete_mac, &pull)?;
            failed.extend(report.failed);
        }
        mark!("apply", started);
        let failed_paths: std::collections::HashSet<&str> = failed.iter().map(|(p, _)| p.as_str()).collect();
        let applied: Vec<Action> = actions
            .iter()
            .filter(|a| !matches!(a, Action::Conflict(_)) && !failed_paths.contains(a.path()))
            .cloned()
            .collect();
        let started = Instant::now();
        let mut next = plan::next_base(&base_in, &mac, &vm, &applied);
        next.extend(base_out);
        self.save_base(&project.id, &next)?;
        mark!("base_write", started);

        let pushed = (push.len() + delete_vm.len()) as u64;
        let pulled = (pull.len() + delete_mac.len()) as u64;
        self.set_status(&project.id, |s| {
            s.state = SyncState::Watching;
            s.last_sync_at = Some(now_millis());
            s.files = next.len() as u64;
            s.pushed += pushed;
            s.pulled += pulled;
            s.conflicts.retain(|path| !scan::in_scope(path, scope_ref) || conflicts.contains(path));
            for path in &conflicts {
                if !s.conflicts.contains(path) {
                    s.conflicts.push(path.clone());
                }
            }
            s.conflicts.sort();
            s.error = failed.first().map(|(path, error)| {
                format!("{} {} could not be synced (first: {path}: {error})", failed.len(), if failed.len() == 1 { "path" } else { "paths" })
            });
        });
        Ok(())
    }
}

fn uuid_like() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("sync-{:x}", nanos ^ (std::process::id() as u128) << 64)
}

fn remote(ssh: &[String], command: &str, root: &str) -> Command {
    let mut argv = ssh.to_vec();
    argv.push(format!("\"$HOME/{}\" sync {command} {}", vm_core::agentd::REMOTE_BIN, shell_quote(root)));
    let mut process = Command::new(&argv[0]);
    process.args(&argv[1..]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    vm_core::util::no_window(&mut process);
    process
}

fn with_remote<T>(
    mut command: Command,
    what: &str,
    transfer: impl FnOnce(&mut Child) -> Result<T, String>,
) -> Result<(T, Vec<u8>), String> {
    let mut child = command.spawn().map_err(err)?;
    let errors = child.stderr.take().ok_or_else(|| std::io::Error::other("SSH has no stderr pipe"))
        .and_then(|stderr| {
            std::thread::Builder::new().name("sync-stderr".into())
                .spawn(move || read_remote_errors(stderr))
        });
    let errors = match errors {
        Ok(errors) => errors,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Could not read {what} diagnostics: {error}"));
        }
    };
    let result = transfer(&mut child);
    let output = child.wait_with_output();
    let (stderr, truncated) = errors.join().map_err(|_| format!("Reading {what} diagnostics stopped"))?.map_err(err)?;
    let output = output.map_err(err)?;
    if !output.status.success() {
        return Err(remote_error(what, output.status, &stderr, truncated));
    }
    Ok((result?, output.stdout))
}

fn read_remote_errors(mut stderr: impl Read) -> std::io::Result<(Vec<u8>, bool)> {
    const LIMIT: usize = 64 * 1024;
    let mut captured = Vec::new();
    let mut truncated = false;
    let mut buf = [0; 8192];
    loop {
        match stderr.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let keep = n.min(LIMIT - captured.len());
                captured.extend_from_slice(&buf[..keep]);
                truncated |= keep < n;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok((captured, truncated))
}

fn remote_error(what: &str, status: std::process::ExitStatus, stderr: &[u8], truncated: bool) -> String {
    let message = String::from_utf8_lossy(stderr);
    let message = message.trim();
    let suffix = if truncated { " [truncated]" } else { "" };
    if message.is_empty() {
        format!("{what} failed in the machine: {status}{suffix}")
    } else {
        format!("{what} failed in the machine: {message}{suffix}")
    }
}

fn remote_scan(ssh: &[String], root: &str, rules: &SyncRules, scope: Option<&[String]>) -> Result<Vec<Entry>, String> {
    let request = ScanRequest { rules: rules.clone(), scope: scope.map(|s| s.to_vec()) };
    let request = serde_json::to_string(&request).map_err(err)?;
    let (listing, _) = with_remote(remote(ssh, "scan", root), "Scanning", |child| {
        // The peer can write output before consuming a large request.
        let mut stdin = child.stdin.take().ok_or("SSH has no stdin pipe")?;
        let stdout = child.stdout.take().ok_or("SSH has no stdout pipe")?;
        let sender = std::thread::Builder::new().name("sync-scan-request".into())
            .spawn(move || writeln!(stdin, "{request}"))
            .map_err(|e| format!("Could not start sending the scan request: {e}"))?;
        let mut stdout = BufReader::with_capacity(64 * 1024, stdout);
        let listing = scan::read_binary(&mut stdout);
        if listing.is_err() {
            // Drain malformed output so the remote command can report its exit status.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        }
        drop(stdout);
        sender.join().map_err(|_| "Sending scan request stopped".to_string())?
            .map_err(|e| format!("Could not send scan request: {e}"))?;
        listing.map_err(|e| format!("The machine's file list was unreadable: {e}"))
    })?;
    Ok(listing)
}

/// Delete `delete` in the machine, then write `push` from the Mac there.
fn remote_apply(ssh: &[String], root: &str, host: &Path, delete: &[String], push: &[String]) -> Result<ApplyReport, String> {
    let (_, out) = with_remote(remote(ssh, "apply", root), "Writing files", |child| {
        let mut send = || -> std::io::Result<()> {
            let stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("SSH has no stdin pipe"))?;
            let mut out = std::io::BufWriter::with_capacity(1 << 20, stdin);
            let header = ApplyHeader { delete: delete.to_vec() };
            writeln!(out, "{}", serde_json::to_string(&header)?)?;
            archive::pack(host, push, &mut out)?;
            out.flush()
        };
        send().map_err(|e| format!("Could not send files: {e}"))
    })?;
    serde_json::from_slice(&out).map_err(|e| format!("The machine's reply was unreadable: {e}"))
}

/// Read `paths` from the machine and apply them (and `delete`) on the Mac.
fn pull_into(ssh: &[String], root: &str, host: &Path, delete: &[String], paths: &[String]) -> Result<ApplyReport, String> {
    let request = serde_json::to_string(&PackRequest { paths: paths.to_vec() }).map_err(err)?;
    let (report, _) = with_remote(remote(ssh, "pack", root), "Reading files", |child| {
        let mut stdin = child.stdin.take().ok_or("SSH has no stdin pipe")?;
        writeln!(stdin, "{request}").map_err(err)?;
        drop(stdin);
        let stdout = child.stdout.take().ok_or("SSH has no stdout pipe")?;
        let mut input = BufReader::with_capacity(1 << 20, stdout);
        let report = archive::apply(host, delete, &mut input);
        std::io::copy(&mut input, &mut std::io::sink()).map_err(err)?;
        report.map_err(err)
    })?;
    Ok(report)
}

fn pull(ssh: &[String], root: &str, host: &Path, delete: &[String], paths: &[String]) -> Result<(), String> {
    let report = pull_into(ssh, root, host, delete, paths)?;
    match report.failed.first() {
        Some((path, error)) => Err(format!("{path}: {error}")),
        None => Ok(()),
    }
}

/// The guest's `sync watch`; its stdin stays open for as long as it runs
/// (closing it ends the watch in the machine).
struct GuestWatch {
    child: Child,
    _stdin: std::process::ChildStdin,
}

/// Run `sync watch` in the machine; each batch becomes a scoped cycle.
fn watch_guest(ssh: &[String], project: &SyncProject, kick: mpsc::Sender<Kick>) -> Result<GuestWatch, String> {
    let mut child = remote(ssh, "watch", &project.guest_path).stderr(Stdio::null()).spawn().map_err(err)?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    writeln!(stdin, "{}", serde_json::to_string(&project.rules).map_err(err)?).map_err(err)?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(batch) = serde_json::from_str::<Vec<String>>(&line) else { continue };
            if !batch.is_empty() && kick.send(Kick::Scope(batch)).is_err() {
                break;
            }
        }
    });
    Ok(GuestWatch { child, _stdin: stdin })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corrupt_base_stops_sync_instead_of_losing_history() {
        let home = tempfile::tempdir().unwrap();
        let manager = SyncManager::open(home.path().join("sync"));
        let path = manager.base_path("project");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken base").unwrap();
        assert!(manager.load_base("project").unwrap_err().contains("Could not read the sync base"));
        assert_eq!(std::fs::read(path).unwrap(), b"broken base");
    }

    #[cfg(unix)]
    #[test]
    fn short_sync_commands_share_one_ssh_transport() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = reuse_ssh(vec!["ssh".into(), "dev@example".into()], dir.path());
        assert!(ssh.iter().any(|arg| arg == "ControlMaster=auto"));
        assert!(ssh.iter().any(|arg| arg.contains("ControlPath=/tmp/cabin-sync-")));
        let local = reuse_ssh(vec!["sh".into(), "-c".into(), "true".into()], dir.path());
        assert_eq!(local, ["sh", "-c", "true"]);
    }

    fn scan_from_script(script: &str, rules: &SyncRules) -> Result<Vec<Entry>, String> {
        let ssh = vec!["sh".into(), "-c".into(), script.into()];
        remote_scan(&ssh, "/unused", rules, None)
    }

    #[test]
    fn remote_scan_drains_stderr_while_sending_and_decoding() {
        // Fill stderr before reading a request larger than a pipe, then return
        // an empty binary listing. Sequential pipe handling would deadlock.
        let rules = SyncRules { ignore: vec!["x".repeat(256 * 1024)], ..SyncRules::default() };
        let script = "dd if=/dev/zero bs=1024 count=256 >&2 2>/dev/null; IFS= read -r request; printf 'VASCAN1\\000\\000\\000\\000\\000'";
        assert_eq!(scan_from_script(script, &rules).unwrap(), Vec::<Entry>::new());
    }

    #[test]
    fn remote_scan_reports_exit_and_bounds_non_utf8_stderr() {
        let script = "IFS= read -r request; printf 'failure: \\377' >&2; dd if=/dev/zero bs=1024 count=128 >&2 2>/dev/null; exit 7";
        let error = scan_from_script(script, &SyncRules::default()).unwrap_err();
        assert!(error.contains("Scanning failed in the machine: failure: �"), "{error:?}");
        assert!(error.contains("[truncated]"));
        assert!(error.len() < 70 * 1024);
    }

    #[test]
    fn remote_upload_drains_diagnostics_before_sending_archive() {
        let host = tempfile::tempdir().unwrap();
        std::fs::write(host.path().join("large.txt"), vec![b'x'; 512 * 1024]).unwrap();
        let script = r#"dd if=/dev/zero bs=1024 count=256 >&2 2>/dev/null; cat >/dev/null; printf '%s' '{"written":1,"deleted":0,"failed":[]}'"#;
        let ssh = vec!["sh".into(), "-c".into(), script.into()];
        let report = remote_apply(&ssh, "/unused", host.path(), &[], &["large.txt".into()]).unwrap();
        assert_eq!(report.written, 1);
    }

    #[test]
    fn remote_download_keeps_the_exit_error_after_large_diagnostics() {
        let host = tempfile::tempdir().unwrap();
        let script = "IFS= read -r request; printf 'pack failed: \\377' >&2; dd if=/dev/zero bs=1024 count=256 >&2 2>/dev/null; exit 7";
        let ssh = vec!["sh".into(), "-c".into(), script.into()];
        let error = pull_into(&ssh, "/unused", host.path(), &[], &["file.txt".into()]).unwrap_err();
        assert!(error.contains("Reading files failed in the machine: pack failed: �"));
        assert!(error.ends_with("[truncated]"));
        assert!(error.len() < 70 * 1024);
    }

    #[test]
    fn remote_scan_rejects_bad_streams_and_checks_exit_status() {
        let rules = SyncRules::default();
        let bad = "IFS= read -r request; printf 'bad data'; dd if=/dev/zero bs=1024 count=128 2>/dev/null";
        assert!(scan_from_script(bad, &rules).unwrap_err().contains("file list was unreadable"));
        let trailing = "IFS= read -r request; printf 'VASCAN1\\000\\000\\000\\000\\000extra'";
        assert!(scan_from_script(trailing, &rules).unwrap_err().contains("trailing data"));
        let failed = "IFS= read -r request; printf 'VASCAN1\\000\\000\\000\\000\\000'; printf 'scan failed' >&2; exit 9";
        assert!(scan_from_script(failed, &rules).unwrap_err().contains("scan failed"));
        let early = "exit 9";
        assert!(scan_from_script(early, &rules).unwrap_err().contains("Scanning failed in the machine"));
    }

    #[test]
    fn parents_quotes_and_overlaps() {
        assert_eq!(parent_of("a/b/c.rs"), "a/b");
        assert_eq!(parent_of("top"), "");
        assert_eq!(shell_quote("~/it's here"), r"'~/it'\''s here'");
    }

    #[test]
    fn projects_persist_and_reject_overlapping_folders() {
        let home = tempfile::tempdir().unwrap();
        let mac = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(mac.path().join("app/inner")).unwrap();
        let manager = SyncManager::open(home.path().join("sync"));
        let input = |path: &Path, guest: &str| SyncProjectInput {
            workspace_id: None,
            name: "app".into(),
            host_path: path.display().to_string(),
            guest_path: guest.into(),
            mode: SyncMode::Safe,
            rules: SyncRules::default(),
        };
        let view = manager.create(input(&mac.path().join("app"), "~/projects/app")).unwrap();
        assert_eq!(view.status.state, SyncState::Offline);
        assert!(manager.create(input(&mac.path().join("app/inner"), "~/projects/other")).unwrap_err().contains("overlaps"));
        assert!(manager.create(input(mac.path(), "~/projects/app/sub")).unwrap_err().contains("overlaps"));
        assert!(manager.create(input(mac.path(), "relative")).is_err());

        let again = SyncManager::open(home.path().join("sync"));
        assert_eq!(again.list().len(), 1);
        let paused = again.update(&view.project.id, Some(SyncMode::TwoWay), None, Some(true)).unwrap();
        assert_eq!((paused.project.mode, paused.status.state), (SyncMode::TwoWay, SyncState::Paused));
        again.remove(&view.project.id).unwrap();
        assert!(SyncManager::open(home.path().join("sync")).list().is_empty());
    }

    #[test]
    fn failed_project_save_preserves_live_and_saved_state() {
        let home = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let dir = home.path().join("sync");
        let manager = SyncManager::open(dir.clone());
        let input = |host: &Path, name: &str| SyncProjectInput {
            workspace_id: None,
            name: name.into(),
            host_path: host.display().to_string(),
            guest_path: format!("~/projects/{name}"),
            mode: SyncMode::Safe,
            rules: SyncRules::default(),
        };
        let id = manager.create(input(first.path(), "first")).unwrap().project.id;
        let base = manager.base_path(&id);
        std::fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::fs::write(&base, b"existing sync history").unwrap();
        std::fs::create_dir(dir.join("projects.tmp")).unwrap();

        assert!(manager.create(input(second.path(), "second")).is_err());
        assert!(manager.update(&id, Some(SyncMode::TwoWay), None, Some(true)).is_err());
        assert!(manager.remove(&id).is_err());

        for list in [manager.list(), SyncManager::open(dir).list()] {
            let [view] = list.as_slice() else { panic!("expected the original project") };
            assert_eq!(view.project.id, id);
            assert_eq!(view.project.mode, SyncMode::Safe);
            assert!(!view.project.paused);
            assert_eq!(view.status.state, SyncState::Offline);
        }
        assert_eq!(std::fs::read(base).unwrap(), b"existing sync history");
    }

    #[test]
    fn reset_refuses_an_active_sync_or_a_mac_folder_inside_the_data_root() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let external = root.path().join("external");
        let nested = data.join("sync/project");
        for path in [&external, &nested] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(path.join("keep"), "user files").unwrap();
        }
        let sync = SyncManager::open(data.join("sync"));
        let project = |host: &Path, guest: &str| SyncProjectInput {
            workspace_id: None, name: guest.into(), host_path: host.display().to_string(),
            guest_path: format!("~/projects/{guest}"), mode: SyncMode::Safe, rules: SyncRules::default(),
        };
        let view = sync.create(project(&external, "external")).unwrap();
        let lock = sync.cycle_lock(&view.project.id);
        let active = lock.lock().unwrap();
        assert!(sync.reset_storage::<()>(|| panic!("reset ran during sync")).unwrap_err().to_string().contains("finishing"));
        drop(active);
        sync.create(project(&nested, "nested")).unwrap();
        assert!(sync.reset_storage::<()>(|| panic!("reset would erase a Mac folder")).unwrap_err().to_string().contains("inside the app data directory"));
        assert!(nested.join("keep").is_file());
        assert_eq!(sync.list().len(), 2);
    }

    /// The real cycle against the real `cabind sync`, with the SSH hop
    /// replaced by a local shell whose HOME holds the binary.
    #[cfg(unix)]
    fn local_guest() -> Option<(tempfile::TempDir, Vec<String>)> {
        let binary = std::env::var_os("CABIND_TEST_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/target/debug/cabind"));
        // It must exist and run on this host (a shared target folder may hold
        // another platform's build).
        let runs = Command::new(&binary).arg("--version").output().is_ok_and(|out| out.status.success());
        if !runs {
            eprintln!("skipping: no runnable cabind (cargo build --bin cabind, or set CABIND_TEST_BIN)");
            return None;
        }
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(vm_core::agentd::REMOTE_BIN);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(binary.canonicalize().unwrap(), &bin).unwrap();
        let ssh = vec!["env".into(), format!("HOME={}", home.path().display()), "sh".into(), "-c".into()];
        Some((home, ssh))
    }

    /// `cabind` runs in the Linux guest; its end-to-end tests need a Unix shell.
    #[cfg(not(unix))]
    fn local_guest() -> Option<(tempfile::TempDir, Vec<String>)> {
        eprintln!("skipping: the end-to-end sync tests need a Unix shell");
        None
    }

    fn write(root: &Path, rel: &str, body: &str, secs: u64) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        let time = std::time::UNIX_EPOCH + Duration::from_secs(secs);
        std::fs::File::options().write(true).open(&path).unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(time)).unwrap();
    }

    fn read(root: &Path, rel: &str) -> Option<String> {
        std::fs::read_to_string(root.join(rel)).ok()
    }

    fn setup(mode: SyncMode) -> Option<(tempfile::TempDir, tempfile::TempDir, Arc<SyncManager>, SyncProject, PathBuf, PathBuf)> {
        let (guest_home, ssh) = local_guest()?;
        let state = tempfile::tempdir().unwrap();
        let mac = state.path().join("mac");
        std::fs::create_dir_all(&mac).unwrap();
        write(&mac, "src/main.rs", "v1", 1_000);
        write(&mac, ".gitignore", "target/\n", 1_000);
        write(&mac, "target/big.o", "build output", 1_000);
        write(&mac, ".git/HEAD", "ref: refs/heads/main", 1_000);
        let manager = SyncManager::open(state.path().join("sync"));
        let view = manager.create(SyncProjectInput {
            workspace_id: None,
            name: "app".into(),
            host_path: mac.display().to_string(),
            guest_path: "~/projects/app".into(),
            mode,
            rules: SyncRules::default(),
        }).unwrap();
        *manager.guest.write().unwrap() = Some(ssh);
        let vm = guest_home.path().join("projects/app");
        let mac = PathBuf::from(&view.project.host_path);
        Some((guest_home, state, manager, view.project, mac, vm))
    }

    #[test]
    fn safe_mode_end_to_end() {
        let Some((_g, _s, manager, project, mac, vm)) = setup(SyncMode::Safe) else { return };
        manager.first_cycle(&project).unwrap();
        assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("v1"));
        assert_eq!(read(&vm, ".git/HEAD").as_deref(), Some("ref: refs/heads/main"), ".git is copied once");
        assert!(read(&vm, "target/big.o").is_none(), "gitignored output stays on the Mac");
        let project = manager.project(&project.id).unwrap();
        assert!(project.git_copied);

        // The Mac edits; an agent adds a file in the machine.
        write(&mac, "src/main.rs", "v2", 2_000);
        write(&vm, "src/agent.rs", "mine", 2_000);
        manager.cycle(&project, Some(vec!["src".into()])).unwrap();
        assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("v2"));
        assert_eq!(read(&vm, "src/agent.rs").as_deref(), Some("mine"), "machine-only files are kept");
        assert!(read(&mac, "src/agent.rs").is_none(), "nothing flows back in Safe mode");

        // Both edit the same file: a conflict, nothing overwritten.
        write(&mac, "src/main.rs", "mac v3", 3_000);
        write(&vm, "src/main.rs", "agent v3", 3_001);
        manager.cycle(&project, None).unwrap();
        assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("agent v3"));
        assert_eq!(manager.status(&project.id).conflicts, ["src/main.rs"]);

        // Keep the Mac's version.
        manager.resolve(&project.id, "src/main.rs", true).unwrap();
        assert_eq!(read(&vm, "src/main.rs").as_deref(), Some("mac v3"));
        assert!(manager.status(&project.id).conflicts.is_empty());
        manager.cycle(&project, None).unwrap();
        assert!(manager.status(&project.id).conflicts.is_empty(), "a resolved conflict stays resolved");
    }

    #[test]
    fn mirror_mode_end_to_end() {
        let Some((_g, _s, manager, project, mac, vm)) = setup(SyncMode::Mirror) else { return };
        manager.first_cycle(&project).unwrap();
        write(&vm, "src/main.rs", "agent edit", 5_000);
        write(&vm, "stray.txt", "x", 5_000);
        std::fs::remove_file(mac.join("src/main.rs")).unwrap();
        write(&mac, "src/lib.rs", "lib", 5_000);
        manager.cycle(&project, None).unwrap();
        assert!(read(&vm, "src/main.rs").is_none());
        assert!(read(&vm, "stray.txt").is_none());
        assert_eq!(read(&vm, "src/lib.rs").as_deref(), Some("lib"));
    }

    #[test]
    fn two_way_mode_end_to_end() {
        let Some((_g, _s, manager, project, mac, vm)) = setup(SyncMode::TwoWay) else { return };
        manager.first_cycle(&project).unwrap();
        write(&vm, "src/main.rs", "agent v2", 2_000);
        write(&vm, "src/new/agent.rs", "new", 2_000);
        write(&mac, "README.md", "readme", 2_000);
        manager.cycle(&project, None).unwrap();
        assert_eq!(read(&mac, "src/main.rs").as_deref(), Some("agent v2"));
        assert_eq!(read(&mac, "src/new/agent.rs").as_deref(), Some("new"));
        assert_eq!(read(&vm, "README.md").as_deref(), Some("readme"));
        // A deletion in the machine reaches the Mac; nothing ping-pongs.
        std::fs::remove_file(vm.join("src/new/agent.rs")).unwrap();
        manager.cycle(&project, None).unwrap();
        assert!(read(&mac, "src/new/agent.rs").is_none());
        let before = manager.status(&project.id);
        manager.cycle(&project, None).unwrap();
        let after = manager.status(&project.id);
        assert_eq!((before.pushed, before.pulled), (after.pushed, after.pulled), "a settled tree needs nothing");
        // The machine's own git history is never overwritten after the copy.
        write(&vm, ".git/HEAD", "ref: refs/heads/agent", 9_000);
        manager.cycle(&project, None).unwrap();
        assert_eq!(read(&mac, ".git/HEAD").as_deref(), Some("ref: refs/heads/main"));
    }

    fn bench_throughput(mode: SyncMode, label: &str) {
        let Some((_g, _s, manager, project, mac, _vm)) = setup(mode) else { return };
        for dir in 0..200 {
            for file in 0..100 {
                write(&mac, &format!("src/d{dir}/f{file}.rs"), "fn main() { println!(\"hello\"); }\n", 1_000);
            }
        }
        for big in 0..5 {
            std::fs::write(mac.join(format!("assets-{big}.bin")), vec![7u8; 10 << 20]).unwrap();
        }
        let started = Instant::now();
        manager.first_cycle(&project).unwrap();
        let first = started.elapsed();
        let started = Instant::now();
        manager.cycle(&project, None).unwrap();
        let noop = started.elapsed();
        write(&mac, "src/d7/f3.rs", "changed", 9_000);
        let started = Instant::now();
        manager.cycle(&project, Some(vec!["src/d7".into()])).unwrap();
        let one = started.elapsed();
        eprintln!("{label} 20,000 files + 50 MB: first sync {first:?}; full cycle, nothing changed {noop:?}; one edit (scoped) {one:?}");
    }

    /// Timings on a real tree (run with --ignored --nocapture).
    #[test]
    #[ignore = "benchmark"]
    fn throughput() {
        bench_throughput(SyncMode::TwoWay, "two-way");
    }

    #[test]
    #[ignore = "benchmark"]
    fn throughput_safe() {
        bench_throughput(SyncMode::Safe, "safe");
    }
}
