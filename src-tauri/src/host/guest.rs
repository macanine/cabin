//! The link to `cabind` over SSH: hello and deploy, guest calls and their
//! pending replies, and the event stream that overlays host state.

use agent_core::util::{LockExt, RwLockExt};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use agent_core::model::{
    AppEvent, AppSnapshot, GuestConnection, MachineState, NetworkGateMode, StartState,
};
use serde_json::{Value, json};
use vm_core::startup::{Attempt, Phase};

use super::{Host, start_gateway, unavailable};
use crate::app_error::AppError;

pub(super) struct Link {
    outbox: mpsc::SyncSender<Vec<u8>>,
    child: Child,
}

/// Requests waiting for the pipe. A guest that stops reading fills this and
/// then refuses new calls, rather than blocking them.
const OUTBOX_DEPTH: usize = 256;

/// Calls that only read guest state. They answer in milliseconds, so a missing
/// reply means the request is stuck.
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Everything else can install packages or wait on an agent. This only ends
/// calls that will never be answered.
const WORK_TIMEOUT: Duration = Duration::from_secs(30 * 60);

fn call_timeout(method: &str) -> Duration {
    let base = match method {
        "app_snapshot" | "clock" | "machine_observation" | "monitor_processes"
        | "terminal_snapshot" | "chat_snapshot" | "chat_attachment" | "agent_chat_capabilities"
        | "acp_runtimes" | "agent_config" | "agent_backups" | "agent_catalog" | "workspace_files"
        | "worktree_refs" | "fs_list" | "fs_read" | "git_status" | "git_diff" | "listening_ports"
        | "mcp_library" | "mcp_library_edit" | "mcp_library_export" | "mcp_library_parse"
        | "skill_library" | "skill_library_read" | "environment_state" | "environment_review"
        | "node_state" | "operations_list" | "diagnostic_events" => READ_TIMEOUT,
        _ => WORK_TIMEOUT,
    };
    base * vm_core::platform::slowdown()
}

/// Write requests in order on their own thread, so a full pipe never holds the
/// link. Dropping the sender, or a failed write, closes the pipe; the guest
/// then ends the link and the reader fails what is still pending.
fn spawn_writer(mut pipe: impl Write + Send + 'static, depth: usize) -> std::io::Result<mpsc::SyncSender<Vec<u8>>> {
    let (outbox, requests) = mpsc::sync_channel::<Vec<u8>>(depth);
    std::thread::Builder::new().name("guest-writer".into()).spawn(move || {
        for line in requests {
            if pipe.write_all(&line).and_then(|()| pipe.flush()).is_err() {
                return;
            }
        }
    })?;
    Ok(outbox)
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
pub(super) struct Deployment(Option<GuestBuild>);

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

impl Host {
    // ------------------------------------------------------- guest link

    pub(super) fn disconnect(&self) {
        if let Some(mut link) = self.link.locked().take() {
            let _ = link.child.kill();
            let _ = link.child.wait();
        }
    }

    pub(super) fn fail_pending(&self, message: &str) {
        for (_, reply) in self
            .pending.locked()
            .drain()
        {
            let _ = reply.send(Err(unavailable(message)));
        }
    }

    pub(super) fn call_guest(&self, method: &str, params: Value) -> Result<Value, AppError> {
        self.call_guest_with_timeout(method, params, call_timeout(method))
    }

    pub(super) fn call_guest_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, AppError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, wait) = mpsc::channel();
        {
            let link = self.link.locked();
            let Some(link) = link.as_ref() else {
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
            if let Err(error) = link.outbox.try_send(line.into_bytes()) {
                self.pending.locked()
                    .remove(&id);
                return Err(unavailable(match error {
                    mpsc::TrySendError::Full(_) => "The machine is not accepting requests right now. Try again in a moment.",
                    mpsc::TrySendError::Disconnected(_) => "The connection to the machine dropped",
                }));
            }
        }
        match wait.recv_timeout(timeout) {
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
        }
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
        if let Ok(runtime) = self.machine.runtime()
            && let Err(error) = vm_core::shell::migrate_chrome_autostart(&runtime)
        {
            cabin_diagnostics::event(cabin_diagnostics::Level::Warn, "chrome", "autostart_migration_failed", format_args!("{error}"));
        }
        if self.uses_gateway() {
            if let Some(attempt) = attempt {
                attempt.note("Applying IPv4-only guest network configuration");
            }
            self.call_guest_with_timeout("network_ipv4_ensure", json!({}), Duration::from_secs(60))
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
                Duration::from_secs(30),
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
            Duration::from_secs(90),
        );
        Ok(())
    }

    /// The bundled cabind's SHA-256, recomputed only when the file changes.
    pub(super) fn bundled_build(&self) -> Result<String, String> {
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
            .or_else(|| {
                let path = runtime.cfg.instance_logs_dir().join("guest-link.log");
                cabin_diagnostics::open_capture(&path, cabin_diagnostics::Capture::Replace).ok()
            })
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
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unreachable("SSH did not provide the cabind handshake pipes".into()));
        };
        let (hello_tx, hello_rx) = mpsc::channel();
        if let Err(error) = std::thread::Builder::new().name("guest-hello".into()).spawn(move || {
            let mut lines = BufReader::new(stdout).lines();
            let _ = hello_tx.send((lines.next(), lines));
        }) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(unreachable(format!("Could not start reading the cabind handshake: {error}")));
        }
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
        #[cfg(unix)]
        if vm_core::vm::cli_dir(&runtime.cfg).join("exec.sock").exists()
            && let Err(error) = vm_core::agentd::prepare_native(&runtime, deployed)
        {
            cabin_diagnostics::event(cabin_diagnostics::Level::Warn, "cli", "guest_setup_failed", format_args!("{error}"));
        }
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
            let outbox = match spawn_writer(stdin, OUTBOX_DEPTH) {
                Ok(outbox) => outbox,
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("Could not start writing to the machine: {error}"));
                }
            };
            *link = Some(Link { outbox, child });
        }

        let alive = Arc::new(AtomicBool::new(true));
        let worker_alive = alive.clone();
        let me = self.clone();
        let load = match std::thread::Builder::new().name("guest-snapshot".into()).spawn(move || {
            let result =
                me.load_guest(generation, &worker_alive, attempt.as_ref(), version, daemon);
            if result.is_err() {
                me.disconnect();
            }
            result
        }) {
            Ok(load) => load,
            Err(error) => {
                self.disconnect();
                self.fail_pending("The connection to the machine dropped");
                return Err(format!("Could not start loading the guest snapshot: {error}"));
            }
        };

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
    pub fn run_connection_loop(self: &Arc<Self>) -> Result<(), AppError> {
        let me = self.clone();
        std::thread::Builder::new()
            .name("guest-link".into())
            .spawn(move || {
                let Some(kick_rx) = me.kick_rx.locked().take() else { return };
                if me.machine.installed() && let Ok(attempt) = me.begin_start() {
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
            .map_err(|error| unavailable(format!("Could not start the guest connection: {error}")))?;
        Ok(())
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
    fn reads_time_out_early_and_work_is_given_time() {
        assert!(call_timeout("chat_snapshot") < call_timeout("setup_run"));
        assert_eq!(call_timeout("chat_send"), call_timeout("an_unknown_method"));
    }

    /// A pipe nobody reads: every write waits until it is released.
    struct Stuck(mpsc::Receiver<()>);
    impl Write for Stuck {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            let _ = self.0.recv();
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_stuck_pipe_refuses_requests_instead_of_blocking_them() {
        let (release, gate) = mpsc::channel();
        let outbox = spawn_writer(Stuck(gate), 2).unwrap();
        let begun = Instant::now();
        let mut refused = false;
        for _ in 0..8 {
            refused |= matches!(outbox.try_send(b"x".to_vec()), Err(mpsc::TrySendError::Full(_)));
        }
        assert!(refused && begun.elapsed() < Duration::from_secs(2));
        // Once the pipe breaks, the writer ends and later sends say so.
        drop(release);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(outbox.try_send(vec![]), Err(mpsc::TrySendError::Disconnected(_))) {
            assert!(Instant::now() < deadline, "the writer kept running");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn requests_are_written_whole_and_in_order() {
        let (read, write) = std::io::pipe().unwrap();
        let outbox = spawn_writer(write, 8).unwrap();
        for line in ["one\n", "two\n", "three\n"] {
            outbox.try_send(line.as_bytes().to_vec()).unwrap();
        }
        drop(outbox);
        let lines: Vec<_> = BufReader::new(read).lines().map(Result::unwrap).collect();
        assert_eq!(lines, ["one", "two", "three"]);
    }
}
