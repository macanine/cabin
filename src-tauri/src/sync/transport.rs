//! Run guest sync commands over SSH or native channels and own their pipes.

use std::io::{self, BufRead, BufReader, PipeReader, PipeWriter, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_core::model::SyncProject;
use agent_core::sync::archive::{self, ApplyChecks, ApplyHeader, ApplyReport};
use agent_core::sync::scan::{self, Entry};
use agent_core::sync::{PackRequest, ScanRequest, SyncRules};
use agent_core::util::LockExt;
use vm_core::platform::{CommandTree, command_pipe};

use super::{GuestTransport, Kick, Kicks, err, shell_quote};

fn remote(guest: &GuestTransport, command: &str, root: &str) -> Result<Command, String> {
    let process = match guest {
        GuestTransport::Ssh(ssh) => {
            let (executable, args) = ssh.split_first().ok_or("SSH command is missing")?;
            let mut process = Command::new(executable);
            process.args(args);
            process.arg(format!(
                "\"$HOME/{}\" sync {command} {}",
                vm_core::agentd::REMOTE_BIN,
                shell_quote(root)
            ));
            process
        }
        #[cfg(unix)]
        GuestTransport::Native {
            executable,
            dir,
            desktop,
            ..
        } => {
            let mut process = Command::new(executable);
            process.arg(if *desktop {
                "--native-sync-worker"
            } else {
                "__sync"
            });
            process
                .args([command, root])
                .env("CABIN_CLI_CHANNEL_DIR", dir);
            process
        }
    };
    Ok(process)
}

/// How long one guest sync command may run before it is killed. Large projects
/// take minutes; a hung ssh or a wedged guest must not hold the cycle forever.
const REMOTE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// The child's pipes, handed to the transfer step so it can stream its request
/// and reply while the process stays killable.
pub(super) struct Pipes {
    stdin: Option<SyncPipe<PipeWriter>>,
    stdout: Option<SyncPipe<PipeReader>>,
    stderr: Option<SyncPipe<PipeReader>>,
}

struct SyncPipe<T> {
    pipe: T,
    stopped: Arc<AtomicBool>,
}

impl<T> SyncPipe<T> {
    fn new(pipe: T, stopped: &Arc<AtomicBool>) -> Self {
        Self {
            pipe,
            stopped: stopped.clone(),
        }
    }

    fn call(
        &mut self,
        mut operation: impl FnMut(&mut T) -> io::Result<usize>,
    ) -> io::Result<usize> {
        while !self.stopped.load(Ordering::Acquire) {
            match operation(&mut self.pipe) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
        Err(io::Error::from(io::ErrorKind::TimedOut))
    }
}

impl Read for SyncPipe<PipeReader> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.call(|pipe| command_pipe::read(pipe, buf))
    }
}

impl Write for SyncPipe<PipeWriter> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.call(|pipe| match pipe.write(buf) {
            Ok(0) if !buf.is_empty() => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            result => result,
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn with_remote<T>(
    guest: &GuestTransport,
    command: Command,
    what: &str,
    transfer: impl FnOnce(&mut Pipes) -> Result<T, String>,
) -> Result<T, String> {
    with_remote_timeout(
        command,
        what,
        REMOTE_TIMEOUT,
        guest.cancellation(),
        transfer,
    )
}

pub(super) fn with_remote_timeout<T>(
    mut command: Command,
    what: &str,
    timeout: Duration,
    cancelled: Option<&Arc<AtomicBool>>,
    transfer: impl FnOnce(&mut Pipes) -> Result<T, String>,
) -> Result<T, String> {
    let stopped = Arc::new(AtomicBool::new(false));
    let (input, stdin) = command_pipe::input().map_err(err)?;
    let (stdout, output) = command_pipe::output().map_err(err)?;
    let (stderr, errors) = command_pipe::output().map_err(err)?;
    command.stdin(input).stdout(output).stderr(errors);
    let child = CommandTree::spawn(&mut command).map_err(err)?;
    drop(command);
    let mut pipes = Pipes {
        stdin: Some(SyncPipe::new(stdin, &stopped)),
        stdout: Some(SyncPipe::new(stdout, &stopped)),
        stderr: Some(SyncPipe::new(stderr, &stopped)),
    };
    let errors = pipes
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("Guest sync has no stderr pipe"))
        .and_then(|stderr| {
            std::thread::Builder::new()
                .name("sync-stderr".into())
                .spawn(move || read_remote_errors(stderr))
        });
    let errors = match errors {
        Ok(errors) => errors,
        Err(error) => {
            return Err(format!("Could not read {what} diagnostics: {error}"));
        }
    };

    // The transfer streams over the pipes and can block on a wedged guest, so a
    // watchdog owns the deadline and the kill; the transfer only reports.
    let deadline = Instant::now() + timeout;
    let child = Arc::new(Mutex::new(child));
    let (finished, wait_for_finish) = mpsc::channel();
    let watchdog = {
        let child = child.clone();
        let cancelled = cancelled.cloned();
        let stopped = stopped.clone();
        std::thread::Builder::new()
            .name("sync-watchdog".into())
            .spawn(move || {
                loop {
                    if cancelled
                        .as_ref()
                        .is_some_and(|stop| stop.load(Ordering::Relaxed))
                    {
                        stopped.store(true, Ordering::Release);
                        child.locked().terminate();
                        return Some("was cancelled".to_string());
                    }
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        stopped.store(true, Ordering::Release);
                        child.locked().terminate();
                        return Some(format!("did not finish within {timeout:?} and was stopped"));
                    }
                    let wait = if cancelled.is_some() {
                        remaining.min(Duration::from_millis(100))
                    } else {
                        remaining
                    };
                    if wait_for_finish.recv_timeout(wait) != Err(RecvTimeoutError::Timeout) {
                        return None;
                    }
                }
            })
    };
    let watchdog = match watchdog {
        Ok(watchdog) => watchdog,
        Err(error) => {
            stopped.store(true, Ordering::Release);
            child.locked().terminate();
            let _ = errors.join();
            return Err(format!("Could not start the {what} watchdog: {error}"));
        }
    };

    let result = transfer(&mut pipes);
    // Close stdin so the guest sees end of input, then drain whatever the
    // transfer did not read so the child cannot block on a full pipe.
    drop(pipes.stdin.take());
    if let Some(mut stdout) = pipes.stdout.take() {
        let _ = std::io::copy(&mut stdout, &mut std::io::sink());
    }
    // Keep the watchdog armed until all pipes, including inherited ones, close.
    let diagnostics = errors.join();
    let status = loop {
        let status = child.locked().try_wait();
        match status {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() < deadline + Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                child.locked().terminate();
                break Err(format!(
                    "{what} did not finish within {timeout:?} and was stopped"
                ));
            }
            Err(error) => {
                child.locked().terminate();
                break Err(error.to_string());
            }
        }
    };
    let _ = finished.send(());
    let interrupted = watchdog
        .join()
        .map_err(|_| format!("The {what} watchdog stopped"))?;
    if let Some(reason) = interrupted {
        return Err(format!("{what} {reason}"));
    }
    let status = status?;
    let (stderr, truncated) = diagnostics
        .map_err(|_| format!("Reading {what} diagnostics stopped"))?
        .map_err(err)?;
    if !status.success() {
        return Err(remote_error(what, status, &stderr, truncated));
    }
    result
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

fn remote_error(
    what: &str,
    status: std::process::ExitStatus,
    stderr: &[u8],
    truncated: bool,
) -> String {
    let message = String::from_utf8_lossy(stderr);
    let message = message.trim();
    let suffix = if truncated { " [truncated]" } else { "" };
    if message.is_empty() {
        format!("{what} failed in the machine: {status}{suffix}")
    } else {
        format!("{what} failed in the machine: {message}{suffix}")
    }
}

pub(super) fn remote_scan(
    ssh: &GuestTransport,
    root: &str,
    rules: &SyncRules,
    scope: Option<&[String]>,
) -> Result<Vec<Entry>, String> {
    let request = ScanRequest {
        rules: rules.clone(),
        scope: scope.map(|s| s.to_vec()),
    };
    let request = serde_json::to_string(&request).map_err(err)?;
    let listing = with_remote(ssh, remote(ssh, "scan", root)?, "Scanning", |pipes| {
        // The peer can write output before consuming a large request.
        let mut stdin = pipes.stdin.take().ok_or("SSH has no stdin pipe")?;
        let stdout = pipes.stdout.take().ok_or("SSH has no stdout pipe")?;
        let sender = std::thread::Builder::new()
            .name("sync-scan-request".into())
            .spawn(move || writeln!(stdin, "{request}"))
            .map_err(|e| format!("Could not start sending the scan request: {e}"))?;
        let mut stdout = BufReader::with_capacity(64 * 1024, stdout);
        let listing = scan::read_content_binary(&mut stdout);
        if listing.is_err() {
            // Drain malformed output so the remote command can report its exit status.
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        }
        drop(stdout);
        sender
            .join()
            .map_err(|_| "Sending scan request stopped".to_string())?
            .map_err(|e| format!("Could not send scan request: {e}"))?;
        listing.map_err(|e| format!("The machine's file list was unreadable: {e}"))
    })?;
    Ok(listing)
}

/// Delete `delete` in the machine, then write `push` from the Mac there.
pub(super) fn remote_apply(
    ssh: &GuestTransport,
    root: &str,
    host: &Path,
    delete: &[String],
    push: &[String],
    mut checks: Option<ApplyChecks>,
) -> Result<ApplyReport, String> {
    let present = match &mut checks {
        Some(checks) => {
            let present = archive::present_deletions(host, delete)
                .map_err(|error| format!("Could not verify deletions: {error}"))?;
            for path in &present {
                checks.deletes.remove(path);
            }
            present
        }
        None => Vec::new(),
    };
    let delete = match &checks {
        Some(checks) => delete
            .iter()
            .filter(|path| checks.deletes.contains_key(*path))
            .cloned()
            .collect(),
        None => delete.to_vec(),
    };
    let mut report = send_archive(ssh, root, host, push, ApplyHeader::new(delete, checks))?;
    report.kept_present = present;
    Ok(report)
}

pub(super) fn copy_git(
    ssh: &GuestTransport,
    root: &str,
    host: &Path,
    paths: &[String],
    checks: ApplyChecks,
) -> Result<ApplyReport, String> {
    send_archive(ssh, root, host, paths, ApplyHeader::git_copy(checks))
}

fn send_archive(
    ssh: &GuestTransport,
    root: &str,
    host: &Path,
    paths: &[String],
    header: ApplyHeader,
) -> Result<ApplyReport, String> {
    with_remote(ssh, remote(ssh, "apply", root)?, "Writing files", |pipes| {
        let mut send = || -> std::io::Result<()> {
            let stdin = pipes
                .stdin
                .take()
                .ok_or_else(|| std::io::Error::other("SSH has no stdin pipe"))?;
            let mut out = std::io::BufWriter::with_capacity(1 << 20, stdin);
            writeln!(out, "{}", serde_json::to_string(&header)?)?;
            archive::pack(host, paths, &mut out)?;
            out.flush()
        };
        send().map_err(|e| format!("Could not send files: {e}"))?;
        // The reply only comes after the archive, so read it once the request
        // is complete.
        drop(pipes.stdin.take());
        let mut stdout = pipes.stdout.take().ok_or("SSH has no stdout pipe")?;
        let mut out = Vec::new();
        stdout
            .read_to_end(&mut out)
            .map_err(|e| format!("Could not read the machine's reply: {e}"))?;
        serde_json::from_slice(&out).map_err(|e| format!("The machine's reply was unreadable: {e}"))
    })
}

/// Read `paths` from the machine and apply them (and `delete`) on the Mac.
pub(super) fn pull_into(
    ssh: &GuestTransport,
    root: &str,
    host: &Path,
    delete: &[String],
    paths: &[String],
    checks: Option<ApplyChecks>,
) -> Result<ApplyReport, String> {
    let request = match &checks {
        Some(_) => PackRequest::checked(paths.to_vec(), delete.to_vec()),
        None => PackRequest::new(paths.to_vec(), false),
    };
    let request = serde_json::to_string(&request).map_err(err)?;
    let report = with_remote(ssh, remote(ssh, "pack", root)?, "Reading files", |pipes| {
        let mut stdin = pipes.stdin.take().ok_or("SSH has no stdin pipe")?;
        writeln!(stdin, "{request}").map_err(err)?;
        drop(stdin);
        let stdout = pipes.stdout.take().ok_or("SSH has no stdout pipe")?;
        let mut input = BufReader::with_capacity(1 << 20, stdout);
        let report = match checks {
            Some(checks) => archive::apply_checked_pack(host, delete, &mut input, checks),
            None => archive::apply(host, delete, &mut input),
        };
        std::io::copy(&mut input, &mut std::io::sink()).map_err(err)?;
        report.map_err(err)
    })?;
    Ok(report)
}

/// The guest's `sync watch`; its stdin stays open for as long as it runs
/// (closing it ends the watch in the machine).
pub(super) struct GuestWatch {
    child: CommandTree,
    stdin: Option<SyncPipe<PipeWriter>>,
    reader: Option<std::thread::JoinHandle<()>>,
    native: bool,
    stopped: Arc<AtomicBool>,
}

impl Drop for GuestWatch {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let grace = if self.native {
            Duration::from_secs(2)
        } else {
            Duration::ZERO
        };
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline
            && self
                .reader
                .as_ref()
                .is_some_and(|reader| !reader.is_finished())
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        self.stopped.store(true, Ordering::Release);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if !self.native || !matches!(self.child.try_wait(), Ok(Some(_))) {
            self.child.terminate();
        }
    }
}

/// Run `sync watch` in the machine; each batch becomes a scoped cycle.
pub(super) fn watch_guest(
    ssh: &GuestTransport,
    project: &SyncProject,
    kick: Kicks,
) -> Result<GuestWatch, String> {
    let rules = serde_json::to_string(&project.rules).map_err(err)?;
    let mut command = remote(ssh, "watch", &project.guest_path)?;
    let (input, stdin) = command_pipe::input().map_err(err)?;
    let (stdout, output) = command_pipe::output().map_err(err)?;
    command.stdin(input).stdout(output).stderr(Stdio::null());
    let child = CommandTree::spawn(&mut command).map_err(err)?;
    drop(command);
    let native = !matches!(ssh, GuestTransport::Ssh(_));
    let mut watch = GuestWatch {
        child,
        stdin: None,
        reader: None,
        native,
        stopped: Arc::new(AtomicBool::new(false)),
    };
    let mut stdin = SyncPipe::new(stdin, &watch.stopped);
    let stdout = BufReader::new(SyncPipe::new(stdout, &watch.stopped));
    let (ready, readiness) = mpsc::channel();
    watch.reader = Some(
        std::thread::Builder::new()
            .name("sync-guest-watch".into())
            .spawn(move || {
                let mut lines = stdout.lines();
                if native {
                    let started = matches!(lines.next(), Some(Ok(line)) if line.trim() == "[]");
                    if ready.send(started).is_err() || !started {
                        return;
                    }
                }
                for line in lines {
                    let Ok(line) = line else { break };
                    let Ok(batch) = serde_json::from_str::<Vec<String>>(&line) else {
                        continue;
                    };
                    if !batch.is_empty() && kick.send(Kick::Scope(batch)).is_err() {
                        break;
                    }
                }
            })
            .map_err(err)?,
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let (sent, opened) = mpsc::channel();
    let writer = std::thread::Builder::new()
        .name("sync-watch-start".into())
        .spawn(move || {
            let result = writeln!(stdin, "{rules}").map(|()| stdin);
            let _ = sent.send(result);
        })
        .map_err(err)?;
    let initialized = opened.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    match initialized {
        Ok(result) => {
            let _ = writer.join();
            watch.stdin = Some(result.map_err(err)?);
        }
        Err(_) => {
            watch.stopped.store(true, Ordering::Release);
            let _ = writer.join();
            return Err("Guest watcher did not accept its rules in time".into());
        }
    }
    if native
        && readiness.recv_timeout(deadline.saturating_duration_since(Instant::now())) != Ok(true)
    {
        return Err("Guest watcher did not report readiness".into());
    }
    Ok(watch)
}
