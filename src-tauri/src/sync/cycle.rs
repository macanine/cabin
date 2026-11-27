//! Reconcile one project against its saved base and apply checked changes.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use agent_core::model::{SyncProject, SyncState};
use agent_core::sync::archive::{ApplyChecks, ApplyReport};
use agent_core::sync::plan::{self, Action};
use agent_core::sync::{GitHandling, SyncMode, SyncRules, scan};
use agent_core::util::{LockExt, now_millis};

use crate::host_permissions::check_folder;

use super::pending::{Destination, Pending};
use super::transport::{copy_git, pull_into, remote_apply, remote_scan};
#[cfg(windows)]
use super::windows_paths;
use super::{
    Base, CycleOutcome, GuestTransport, Listing, SyncManager, err, listing,
    project_requires_full_scan, require_applied, transfer_error,
};

impl SyncManager {
    /// The first cycle of a session: `.git` once if asked, then everything.
    #[cfg(any(unix, test))]
    pub(super) fn first_cycle(&self, project: &SyncProject) -> Result<CycleOutcome, String> {
        let guest = self
            .guest_for(&project.id)
            .ok_or("The machine is not connected")?;
        self.first_cycle_on(project, &guest, None)
    }

    pub(super) fn first_cycle_on(
        &self,
        project: &SyncProject,
        guest: &GuestTransport,
        stop: Option<&AtomicBool>,
    ) -> Result<CycleOutcome, String> {
        if project.rules.git == GitHandling::CopyOnce && !project.git_copied {
            self.copy_git_once(project, guest, stop)?;
        }
        self.cycle_on(project, None, guest, stop)
    }

    /// Copy `.git` into the machine unless it already has one. From then on
    /// each side keeps its own history (see `GitHandling::CopyOnce`).
    fn copy_git_once(
        &self,
        project: &SyncProject,
        guest: &GuestTransport,
        stop: Option<&AtomicBool>,
    ) -> Result<(), String> {
        let lock = self.cycle_lock(&project.id);
        let _guard = lock.locked();
        require_running(stop)?;
        self.require_current_project(project)?;
        let host = PathBuf::from(&project.host_path);
        check_folder(&host, project.mode == SyncMode::TwoWay)
            .require(&host)
            .map_err(err)?;
        if host.join(".git").is_dir() {
            let scope = vec![".git".to_string()];
            let theirs = remote_scan(
                guest,
                &project.guest_path,
                &SyncRules::git_only(),
                Some(&scope),
            )?;
            if !theirs.iter().any(|entry| entry.path == ".git") {
                let ours = agent_core::sync::scan(&host, &SyncRules::git_only(), Some(&scope))
                    .map_err(err)?;
                let source = listing(ours);
                let paths: Vec<String> = source.keys().cloned().collect();
                let checks =
                    ApplyChecks::for_paths(&source, &Listing::new(), &paths, &[]).map_err(err)?;
                require_applied(copy_git(guest, &project.guest_path, &host, &paths, checks)?)
                    .map_err(|error| format!("Could not copy .git: {error}"))?;
            }
        }
        self.edit_project(&project.id, |p| p.git_copied = true)
            .map(|_| ())
    }

    /// One reconcile of `scope` (`None`: everything).
    pub(super) fn cycle(
        &self,
        project: &SyncProject,
        scope: Option<Vec<String>>,
    ) -> Result<CycleOutcome, String> {
        let guest = self
            .guest_for(&project.id)
            .ok_or("The machine is not connected")?;
        self.cycle_on(project, scope, &guest, None)
    }

    pub(super) fn cycle_on(
        &self,
        project: &SyncProject,
        scope: Option<Vec<String>>,
        guest: &GuestTransport,
        stop: Option<&AtomicBool>,
    ) -> Result<CycleOutcome, String> {
        let lock = self.cycle_lock(&project.id);
        let _guard = lock.locked();
        require_running(stop)?;
        self.require_current_project(project)?;
        let pending_path = self.dir.join(&project.id).join("pending.json");
        let mut pending = Pending::load(&pending_path)?;
        let host = PathBuf::from(&project.host_path);
        check_folder(&host, project.mode == SyncMode::TwoWay)
            .require(&host)
            .map_err(err)?;
        // A scoped scan can miss an alias that differs in a parent directory's
        // spelling. Windows Two-way needs both full namespaces before writing.
        let scope = if project_requires_full_scan(project) || !pending.is_empty() {
            None
        } else {
            scope
        };
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
            s.state = if scope.is_none() {
                SyncState::Scanning
            } else {
                SyncState::Syncing
            };
        });

        let (mac, vm, mut base_in, base_out, files) = if project.mode == SyncMode::Safe {
            let started = Instant::now();
            let mac =
                listing(agent_core::sync::scan(&host, &project.rules, scope_ref).map_err(err)?);
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
            if pending.is_empty() && !plan::mac_changed(&base_in, &mac) {
                self.set_status(&project.id, |s| {
                    s.state = SyncState::Watching;
                    s.last_sync_at = Some(now_millis());
                    s.files = files;
                    s.conflicts.retain(|path| !scan::in_scope(path, scope_ref));
                    s.error = None;
                });
                return Ok(CycleOutcome::default());
            }
            let started = Instant::now();
            let vm = listing(remote_scan(
                guest,
                &project.guest_path,
                &project.rules,
                scope_ref,
            )?);
            mark!("guest_scan", started);
            (mac, vm, base_in, base_out, files)
        } else {
            // Mirror and Two-way need both sides on every cycle. The Mac walk
            // and the guest walk are independent; run them together.
            let started = Instant::now();
            let (mac_entries, vm_entries) = std::thread::scope(|scope| {
                let vm = std::thread::Builder::new()
                    .name("sync-guest-scan".into())
                    .spawn_scoped(scope, || {
                        remote_scan(guest, &project.guest_path, &project.rules, scope_ref)
                    })
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
            (
                listing(mac_entries),
                listing(vm_entries),
                base_in,
                base_out,
                files,
            )
        };
        if !pending.is_empty() {
            pending.observe(&mut base_in, &mac, &vm);
            self.save_base(&project.id, &base_in)?;
            pending.save(&pending_path)?;
        }
        // Nothing can be applied if both snapshots still match the base.
        // Skip both planning and Windows alias sorting on this read-only path.
        if pending.is_empty()
            && self.base_path(&project.id).exists()
            && plan::base_is_current(&base_in, &mac, &vm)
        {
            self.set_status(&project.id, |s| {
                s.state = SyncState::Watching;
                s.last_sync_at = Some(now_millis());
                s.files = files;
                s.conflicts.retain(|path| !scan::in_scope(path, scope_ref));
                s.error = None;
            });
            return Ok(CycleOutcome::default());
        }
        #[cfg(windows)]
        if project.mode == SyncMode::TwoWay {
            // Baseline-only paths absent on both sides cannot produce actions.
            // Checking the live namespace lets users fix an old collision by
            // renaming the offending file without throwing away sync history.
            windows_paths::check_names(mac.keys().chain(vm.keys()).map(String::as_str))?;
        }
        let started = Instant::now();
        let mut actions = plan::reconcile(project.mode, &base_in, &mac, &vm);
        pending.protect(&mut actions);
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
        // The checked transport probes source deletions and verifies new pull
        // destinations on disk, including entries omitted by ignore rules.
        if !push.is_empty() || !delete_vm.is_empty() {
            self.set_status(&project.id, |s| s.state = SyncState::Syncing);
        }

        let mut failed: Vec<(String, String)> = Vec::new();
        let mut applied = Vec::new();
        let started = Instant::now();
        require_running(stop)?;
        if !push.is_empty() || !delete_vm.is_empty() {
            let checks = ApplyChecks::for_paths(&mac, &vm, &push, &delete_vm).map_err(err)?;
            pending.prepare(Destination::Guest, &mac, &vm, push.iter().chain(&delete_vm));
            pending.save(&pending_path)?;
            let report = remote_apply(
                guest,
                &project.guest_path,
                &host,
                &delete_vm,
                &push,
                Some(checks),
            )?;
            applied.extend(successful_actions(&actions, &report, Destination::Guest));
            failed.extend(report.failed);
            if !pull.is_empty() || !delete_mac.is_empty() {
                let mut uploaded = plan::next_base(&base_in, &mac, &vm, &applied);
                uploaded.extend(
                    base_out
                        .iter()
                        .map(|(path, record)| (path.clone(), record.clone())),
                );
                self.save_base(&project.id, &uploaded)?;
                pending.settle(push.iter().chain(&delete_vm));
            }
        }
        if !pull.is_empty() || !delete_mac.is_empty() {
            require_running(stop)?;
            let checks = ApplyChecks::for_paths(&vm, &mac, &pull, &delete_mac).map_err(err)?;
            pending.prepare(Destination::Host, &vm, &mac, pull.iter().chain(&delete_mac));
            pending.save(&pending_path)?;
            let report = pull_into(
                guest,
                &project.guest_path,
                &host,
                &delete_mac,
                &pull,
                Some(checks),
            )?;
            applied.extend(successful_actions(&actions, &report, Destination::Host));
            failed.extend(report.failed);
        }
        mark!("apply", started);
        let transfer_error = transfer_error(&failed);
        let started = Instant::now();
        let mut next = plan::next_base(&base_in, &mac, &vm, &applied);
        next.extend(base_out);
        self.save_base(&project.id, &next)?;
        pending.settle(
            push.iter()
                .chain(&delete_vm)
                .chain(&pull)
                .chain(&delete_mac),
        );
        pending.save(&pending_path)?;
        mark!("base_write", started);

        let pushed = applied
            .iter()
            .filter(|action| matches!(action, Action::Push(_) | Action::DeleteVm(_)))
            .count() as u64;
        let pulled = applied
            .iter()
            .filter(|action| matches!(action, Action::Pull(_) | Action::DeleteMac(_)))
            .count() as u64;
        self.set_status(&project.id, |s| {
            s.state = SyncState::Watching;
            s.last_sync_at = Some(now_millis());
            s.files = next.len() as u64;
            s.pushed += pushed;
            s.pulled += pulled;
            s.conflicts.retain(|path| !scan::in_scope(path, scope_ref));
            s.conflicts.extend(conflicts.iter().cloned());
            s.conflicts.sort();
            s.error.clone_from(&transfer_error);
        });
        Ok(CycleOutcome {
            conflicts,
            transfer_error,
        })
    }
}

fn require_running(stop: Option<&AtomicBool>) -> Result<(), String> {
    if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
        return Err("Project sync stopped".into());
    }
    Ok(())
}

fn successful_actions(
    actions: &[Action],
    report: &ApplyReport,
    destination: Destination,
) -> Vec<Action> {
    let incomplete: HashSet<&str> = report
        .failed
        .iter()
        .map(|(path, _)| path)
        .chain(&report.kept_nonempty)
        .chain(&report.kept_present)
        .map(String::as_str)
        .collect();
    actions
        .iter()
        .filter(move |action| {
            let targets_side = matches!(
                (destination, action),
                (Destination::Guest, Action::Push(_) | Action::DeleteVm(_))
                    | (Destination::Host, Action::Pull(_) | Action::DeleteMac(_))
            );
            targets_side && !incomplete.contains(action.path())
        })
        .cloned()
        .collect()
}
