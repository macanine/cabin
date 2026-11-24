//! Operation manager: the only gateway for config and system changes.
//!
//! An operation is a list of steps. All steps are prepared first (validation,
//! backups), then applied and verified in order. Any failure or cancellation
//! rolls back reversible steps in reverse order. Every state change is
//! persisted to the SQLite journal and emitted to the event sink.

use crate::util::{LockExt, RwLockExt};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use crate::error::{Error, Result};
use crate::model::{OperationRecord, OperationState, StepRecord, StepState};
use crate::store::Store;
use crate::util::{new_id, now_millis};

#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Context handed to each step phase.
pub struct StepCtx<'a> {
    cancel: &'a CancelToken,
    log: &'a dyn Fn(String),
}

impl StepCtx<'_> {
    pub fn log(&self, message: impl Into<String>) {
        (self.log)(message.into());
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

pub trait Step: Send {
    fn name(&self) -> String;

    /// Validate inputs and take backups. Must not change anything visible.
    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()>;

    /// Confirm the change took effect.
    fn verify(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        Ok(())
    }

    /// A failed operation must leave the progress made by this step in place.
    fn forward_only(&self) -> bool {
        false
    }

    /// Undo `apply`. Called for reversible steps and for a reversible step whose
    /// `apply`/`verify` failed, so it must tolerate partial application.
    fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        Ok(())
    }
}

/// A step built from closures, for small one-off steps.
pub struct FnStep<A, R>
where
    A: FnMut(&StepCtx<'_>) -> Result<()> + Send,
    R: FnMut(&StepCtx<'_>) -> Result<()> + Send,
{
    name: String,
    apply: A,
    rollback: R,
}

impl<A, R> FnStep<A, R>
where
    A: FnMut(&StepCtx<'_>) -> Result<()> + Send,
    R: FnMut(&StepCtx<'_>) -> Result<()> + Send,
{
    pub fn new(name: impl Into<String>, apply: A, rollback: R) -> Self {
        Self {
            name: name.into(),
            apply,
            rollback,
        }
    }
}

impl<A, R> Step for FnStep<A, R>
where
    A: FnMut(&StepCtx<'_>) -> Result<()> + Send,
    R: FnMut(&StepCtx<'_>) -> Result<()> + Send,
{
    fn name(&self) -> String {
        self.name.clone()
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        (self.apply)(ctx)
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        (self.rollback)(ctx)
    }
}

/// The queue an operation waits in. Operations in one lane run one at a
/// time; operations in different lanes may run at the same time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Lane {
    /// Installs and config edits (apt, npm, agent and MCP config, Node.js).
    /// Everything runs here unless it opts out, so these stay globally
    /// serialized.
    #[default]
    System,
    /// The VM's timezone and language (`environment`). These change local
    /// system settings through systemd-timedated, systemd-localed and
    /// `locale-gen`, need no network, and must not queue behind a package
    /// download that waits for the network to open: the network gate opens
    /// only after the timezone and language match the exit.
    ///
    /// Running beside the system lane is safe: `timedatectl` and `localectl`
    /// talk to their daemons over D-Bus, and `locale-gen` compiles locales
    /// with `localedef`, which locks the locale archive itself. None of them
    /// takes the dpkg or apt lock or reads the package lists, and no system
    /// lane operation changes the timezone or locale.
    Environment,
}

impl Lane {
    const ALL: [Lane; 2] = [Lane::System, Lane::Environment];

    fn index(self) -> usize {
        match self {
            Lane::System => 0,
            Lane::Environment => 1,
        }
    }
}

pub struct Operation {
    pub kind: String,
    pub title: String,
    /// Rejects a duplicate while an operation with the same key is running.
    pub key: Option<String>,
    /// Which queue the operation waits in (`Lane::System` by default).
    pub lane: Lane,
    /// Stored so the operation can be rebuilt for a retry.
    pub request: Option<serde_json::Value>,
    pub steps: Vec<Box<dyn Step>>,
}

impl Operation {
    pub fn new(kind: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            kind: kind.into(),
            title: title.into(),
            key: None,
            lane: Lane::default(),
            request: None,
            steps: Vec::new(),
        }
    }

    pub fn lane(mut self, lane: Lane) -> Self {
        self.lane = lane;
        self
    }

    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }

    pub fn request(mut self, request: serde_json::Value) -> Self {
        self.request = Some(request);
        self
    }

    pub fn step(mut self, step: impl Step + 'static) -> Self {
        self.steps.push(Box::new(step));
        self
    }
}

type Sink = Arc<dyn Fn(&OperationRecord) + Send + Sync>;

pub struct OperationManager {
    store: Arc<Store>,
    /// One gate per `Lane`, indexed by `Lane::index`.
    gates: [Mutex<()>; Lane::ALL.len()],
    cancels: Mutex<HashMap<String, CancelToken>>,
    keys: Mutex<HashSet<String>>,
    sink: RwLock<Option<Sink>>,
}

/// Shared mutable record for one running operation.
struct Journal<'a> {
    manager: &'a OperationManager,
    record: Mutex<OperationRecord>,
    key: Option<String>,
    request: Option<serde_json::Value>,
}

impl Journal<'_> {
    fn update(&self, change: impl FnOnce(&mut OperationRecord)) {
        let snapshot = {
            let mut record = self.record.locked();
            change(&mut record);
            record.updated_at = now_millis();
            record.clone()
        };
        // A failed journal write must not abort a half-applied change; the
        // in-memory record and the event stream still carry the truth.
        let _ = self.manager.store.save_operation(
            &snapshot,
            self.key.as_deref(),
            self.request.as_ref(),
        );
        self.manager.emit(&snapshot);
    }

    fn step_state(&self, index: usize, state: StepState) {
        self.update(|record| record.steps[index].state = state);
    }

    fn log(&self, index: usize, line: String) {
        self.update(|record| record.steps[index].log.push(line));
    }

    fn finish(&self) -> OperationRecord {
        self.record.locked()
            .clone()
    }
}

impl OperationManager {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            gates: Lane::ALL.map(|_| Mutex::new(())),
            cancels: Mutex::new(HashMap::new()),
            keys: Mutex::new(HashSet::new()),
            sink: RwLock::new(None),
        }
    }

    pub fn set_sink(&self, sink: impl Fn(&OperationRecord) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }

    fn emit(&self, record: &OperationRecord) {
        let sink = self.sink.read_lock().clone();
        if let Some(sink) = sink {
            sink(record);
        }
    }

    pub fn recent(&self, limit: usize) -> Result<Vec<OperationRecord>> {
        self.store.operations(limit)
    }

    pub fn cancel(&self, id: &str) -> Result<()> {
        let cancels = self.cancels.locked();
        let token = cancels
            .get(id)
            .ok_or_else(|| Error::Conflict("This operation is not running".into()))?;
        token.cancel();
        Ok(())
    }

    /// Run an operation to completion. Returns the final record; an operation
    /// that failed and rolled back is still `Ok` (inspect `record.state`).
    /// `Err` means the operation could not be started at all.
    pub fn run(&self, operation: Operation) -> Result<OperationRecord> {
        let Operation {
            kind,
            title,
            key,
            lane,
            request,
            mut steps,
        } = operation;
        if let Some(key) = &key {
            let mut keys = self.keys.locked();
            if !keys.insert(key.clone()) {
                return Err(Error::Conflict(format!("“{title}” is already running")));
            }
        }
        let _key_guard = KeyGuard {
            manager: self,
            key: key.clone(),
        };

        let now = now_millis();
        let id = new_id("op");
        let journal = Journal {
            manager: self,
            record: Mutex::new(OperationRecord {
                id: id.clone(),
                kind,
                title,
                state: OperationState::Pending,
                steps: steps
                    .iter()
                    .map(|step| StepRecord {
                        name: step.name(),
                        state: StepState::Pending,
                        log: Vec::new(),
                    })
                    .collect(),
                error: None,
                retryable: request.is_some(),
                created_at: now,
                updated_at: now,
            }),
            key,
            request,
        };
        journal.update(|_| {});

        let cancel = CancelToken::default();
        self.cancels.locked()
            .insert(id.clone(), cancel.clone());
        let _cancel_guard = CancelGuard { manager: self, id };

        let _gate = self.gates[lane.index()].locked();
        journal.update(|record| record.state = OperationState::Running);

        // Phase 1: prepare everything; nothing has changed yet.
        for (index, step) in steps.iter_mut().enumerate() {
            let log = |line: String| journal.log(index, line);
            let ctx = StepCtx {
                cancel: &cancel,
                log: &log,
            };
            if cancel.is_cancelled() {
                journal.update(|record| record.state = OperationState::Cancelled);
                return Ok(journal.finish());
            }
            if let Err(error) = step.prepare(&ctx) {
                journal.update(|record| {
                    record.steps[index].state = StepState::Failed;
                    record.steps[index].log.push(format!("Error: {error}"));
                    if let Some(details) = error.details() {
                        record.steps[index].log.push(details);
                    }
                    record.state = OperationState::Failed;
                    record.error = Some(format!("{}: {error}", record.steps[index].name));
                });
                return Ok(journal.finish());
            }
        }

        // Phase 2: apply + verify, rolling back on failure or cancel.
        let mut touched: Vec<usize> = Vec::new();
        let mut failure: Option<(usize, Error)> = None;
        for (index, step) in steps.iter_mut().enumerate() {
            if cancel.is_cancelled() {
                failure = Some((index, Error::Cancelled));
                break;
            }
            journal.step_state(index, StepState::Running);
            let log = |line: String| journal.log(index, line);
            let ctx = StepCtx {
                cancel: &cancel,
                log: &log,
            };
            touched.push(index);
            match step.apply(&ctx).and_then(|()| step.verify(&ctx)) {
                Ok(()) => journal.step_state(index, StepState::Succeeded),
                Err(error) => {
                    journal.update(|record| {
                        record.steps[index].state = StepState::Failed;
                        record.steps[index].log.push(format!("Error: {error}"));
                        if let Some(details) = error.details() {
                            record.steps[index].log.push(details);
                        }
                    });
                    failure = Some((index, error));
                    break;
                }
            }
        }

        let Some((failed_index, error)) = failure else {
            journal.update(|record| record.state = OperationState::Succeeded);
            return Ok(journal.finish());
        };

        let mut rollback_ok = true;
        let mut forward_only = false;
        for &index in touched.iter().rev() {
            if steps[index].forward_only() {
                forward_only = true;
                journal.log(index, "Leaving any completed changes in place for retry".into());
                continue;
            }
            let log = |line: String| journal.log(index, line);
            let ctx = StepCtx {
                cancel: &CancelToken::default(),
                log: &log,
            };
            match steps[index].rollback(&ctx) {
                Ok(()) => {
                    if index != failed_index {
                        journal.step_state(index, StepState::RolledBack);
                    } else {
                        journal.log(index, "Rolled back partial changes".into());
                    }
                }
                Err(rollback_error) => {
                    rollback_ok = false;
                    journal.update(|record| {
                        record.steps[index].state = StepState::RollbackFailed;
                        record.steps[index]
                            .log
                            .push(format!("Rollback failed: {rollback_error}"));
                    });
                }
            }
        }

        let cancelled = matches!(error, Error::Cancelled);
        journal.update(|record| {
            let step_name = record
                .steps
                .get(failed_index)
                .map(|step| step.name.clone())
                .unwrap_or_default();
            if forward_only && rollback_ok {
                record.state = if cancelled {
                    OperationState::Cancelled
                } else {
                    OperationState::Failed
                };
                record.error = Some(format!(
                    "{step_name}: {error}. Any completed forward-only changes are kept for retry."
                ));
                return;
            }
            record.state = match (cancelled, rollback_ok) {
                (_, false) => OperationState::Failed,
                (true, true) => OperationState::Cancelled,
                (false, true) => OperationState::RolledBack,
            };
            record.error = Some(match (cancelled, rollback_ok) {
                (true, true) => "Cancelled; earlier steps were rolled back".into(),
                (true, false) => {
                    "Cancelled, but rollback failed; see the steps below".into()
                }
                (false, true) => format!("{step_name}: {error}. Earlier steps were rolled back."),
                (false, false) => format!(
                    "{step_name}: {error}. Rollback also failed; the system may be partially changed."
                ),
            });
        });
        Ok(journal.finish())
    }
}

struct KeyGuard<'a> {
    manager: &'a OperationManager,
    key: Option<String>,
}

impl Drop for KeyGuard<'_> {
    fn drop(&mut self) {
        if let Some(key) = &self.key {
            self.manager
                .keys.locked()
                .remove(key);
        }
    }
}

struct CancelGuard<'a> {
    manager: &'a OperationManager,
    id: String,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        self.manager
            .cancels.locked()
            .remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    type Trace = Arc<Mutex<Vec<String>>>;

    struct Probe {
        name: &'static str,
        trace: Trace,
        fail_apply: bool,
        fail_prepare: bool,
    }

    impl Probe {
        fn new(name: &'static str, trace: &Trace) -> Self {
            Self {
                name,
                trace: trace.clone(),
                fail_apply: false,
                fail_prepare: false,
            }
        }

        fn push(&self, phase: &str) {
            self.trace
                .lock()
                .unwrap()
                .push(format!("{phase}:{}", self.name));
        }
    }

    impl Step for Probe {
        fn name(&self) -> String {
            self.name.into()
        }
        fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
            self.push("prepare");
            if self.fail_prepare {
                return Err(Error::invalid("bad input"));
            }
            Ok(())
        }
        fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
            self.push("apply");
            ctx.log("applied");
            if self.fail_apply {
                return Err(Error::invalid("boom"));
            }
            Ok(())
        }
        fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
            self.push("rollback");
            Ok(())
        }
    }

    fn manager() -> OperationManager {
        OperationManager::new(Arc::new(Store::in_memory().unwrap()))
    }

    #[test]
    fn success_runs_prepare_then_apply_and_journals() -> Result<()> {
        let trace = Trace::default();
        let ops = manager();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        ops.set_sink(move |record| seen.lock().unwrap().push(record.state));
        let record = ops.run(
            Operation::new("test", "Test")
                .step(Probe::new("a", &trace))
                .step(Probe::new("b", &trace)),
        )?;
        assert_eq!(record.state, OperationState::Succeeded);
        assert_eq!(
            *trace.lock().unwrap(),
            ["prepare:a", "prepare:b", "apply:a", "apply:b"]
        );
        assert_eq!(ops.recent(10)?[0].state, OperationState::Succeeded);
        assert_eq!(record.steps[0].log, ["applied"]);
        let events = events.lock().unwrap();
        assert_eq!(events.first(), Some(&OperationState::Pending));
        assert_eq!(events.last(), Some(&OperationState::Succeeded));
        Ok(())
    }

    #[test]
    fn failure_rolls_back_in_reverse() -> Result<()> {
        let trace = Trace::default();
        let mut failing = Probe::new("c", &trace);
        failing.fail_apply = true;
        let record = manager().run(
            Operation::new("test", "Test")
                .step(Probe::new("a", &trace))
                .step(Probe::new("b", &trace))
                .step(failing),
        )?;
        assert_eq!(record.state, OperationState::RolledBack);
        assert_eq!(
            trace.lock().unwrap()[6..],
            ["rollback:c", "rollback:b", "rollback:a"]
        );
        assert_eq!(record.steps[0].state, StepState::RolledBack);
        assert_eq!(record.steps[2].state, StepState::Failed);
        assert!(record.error.unwrap().starts_with("c: boom"));
        Ok(())
    }

    #[test]
    fn prepare_failure_changes_nothing() -> Result<()> {
        let trace = Trace::default();
        let mut bad = Probe::new("b", &trace);
        bad.fail_prepare = true;
        let record = manager().run(
            Operation::new("test", "Test")
                .step(Probe::new("a", &trace))
                .step(bad),
        )?;
        assert_eq!(record.state, OperationState::Failed);
        assert_eq!(*trace.lock().unwrap(), ["prepare:a", "prepare:b"]);
        Ok(())
    }

    #[test]
    fn cancel_between_steps_rolls_back() -> Result<()> {
        let ops = Arc::new(manager());
        let trace = Trace::default();
        let handle = ops.clone();
        let cancelling = FnStep::new(
            "cancel",
            move |_ctx: &StepCtx<'_>| {
                let id = handle.recent(1)?[0].id.clone();
                handle.cancel(&id)
            },
            |_ctx: &StepCtx<'_>| Ok(()),
        );
        let record = ops.run(
            Operation::new("test", "Test")
                .step(Probe::new("a", &trace))
                .step(cancelling)
                .step(Probe::new("never", &trace)),
        )?;
        assert_eq!(record.state, OperationState::Cancelled);
        assert!(!trace.lock().unwrap().contains(&"apply:never".to_string()));
        assert!(trace.lock().unwrap().contains(&"rollback:a".to_string()));
        Ok(())
    }

    #[test]
    fn duplicate_key_is_rejected_while_running() -> Result<()> {
        let ops = Arc::new(manager());
        let inner = ops.clone();
        let nested = FnStep::new(
            "nested",
            move |_ctx: &StepCtx<'_>| match inner.run(Operation::new("t", "Same").key("k")) {
                Err(Error::Conflict(_)) => Ok(()),
                other => Err(Error::Internal(format!(
                    "expected conflict, got {other:?}",
                    other = other.map(|r| r.state)
                ))),
            },
            |_ctx: &StepCtx<'_>| Ok(()),
        );
        let record = ops.run(Operation::new("t", "Same").key("k").step(nested))?;
        assert_eq!(record.state, OperationState::Succeeded);
        Ok(())
    }

    /// A system-lane step that holds its gate until released, like an apt
    /// update waiting for the network.
    fn blocking_step(
        started: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    ) -> impl Step {
        let release = Mutex::new(release);
        FnStep::new(
            "wait",
            move |_ctx: &StepCtx<'_>| {
                started.send(()).ok();
                release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .map_err(|_| Error::Internal("never released".into()))
            },
            |_ctx: &StepCtx<'_>| Ok(()),
        )
    }

    #[test]
    fn lanes_run_beside_each_other_and_serialize_within() -> Result<()> {
        use std::sync::mpsc::channel;
        let ops = Arc::new(manager());
        let (started_tx, started) = channel();
        let (release, release_rx) = channel();
        let blocked = ops.clone();
        let long = std::thread::spawn(move || {
            blocked.run(Operation::new("t", "Long").step(blocking_step(started_tx, release_rx)))
        });
        started.recv().unwrap();

        // The environment lane proceeds while the system lane is held.
        let trace = Trace::default();
        let record = ops.run(
            Operation::new("environment.apply", "Env")
                .lane(Lane::Environment)
                .step(Probe::new("env", &trace)),
        )?;
        assert_eq!(record.state, OperationState::Succeeded);
        assert!(!long.is_finished(), "the system-lane operation is still running");

        // Another system-lane operation still waits for the first one.
        let (done_tx, done) = channel();
        let queued = ops.clone();
        let second = std::thread::spawn(move || {
            let record = queued.run(Operation::new("t", "Second"));
            done_tx.send(()).ok();
            record
        });
        assert!(
            done.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
            "system lane stays serialized"
        );
        release.send(()).unwrap();
        assert_eq!(long.join().unwrap()?.state, OperationState::Succeeded);
        assert_eq!(second.join().unwrap()?.state, OperationState::Succeeded);
        Ok(())
    }

    #[test]
    fn interrupted_operations_are_marked_failed() -> Result<()> {
        let store = Store::in_memory()?;
        let record = OperationRecord {
            id: "op1".into(),
            kind: "t".into(),
            title: "T".into(),
            state: OperationState::Running,
            steps: vec![],
            error: None,
            retryable: false,
            created_at: 1,
            updated_at: 1,
        };
        store.save_operation(&record, None, None)?;
        assert_eq!(store.mark_interrupted()?, 1);
        assert_eq!(store.operations(1)?[0].state, OperationState::Failed);
        Ok(())
    }
}
