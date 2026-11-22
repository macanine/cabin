//! PTY supervision for plain shells and agent terminals.

use crate::util::{LockExt, RwLockExt};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, mpsc};

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::error::{Error, Result};
use crate::model::{SessionStatus, TerminalSnapshot};
use crate::util::{Utf8Stream, sha256_text};

const SCROLLBACK_LIMIT: usize = 512 * 1024;
const TRIM_HEADROOM: usize = 64 * 1024;
const TRIM_SEARCH: usize = 8 * 1024;
// Cancel any unfinished escape, leave the alternate screen, and reset modes
// and attributes while keeping the normal screen and its scrollback.
const SHELL_BOUNDARY: &str = "\x18\x1b[?1049l\x1b[?1047l\x1b[?47l\x1b[!p\x1b[0m\r\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Output {
        id: String,
        generation: u32,
        offset: u64,
        data: String,
    },
    Status {
        id: String,
        generation: u32,
        status: SessionStatus,
    },
}

/// The user's login shell for terminal sessions: `$SHELL` when it is an
/// absolute path to an existing file, else bash, else sh.
pub fn login_shell() -> PathBuf {
    pick_shell(std::env::var("SHELL").ok().as_deref(), |path| {
        path.is_file()
    })
}

fn pick_shell(env: Option<&str>, exists: impl Fn(&Path) -> bool) -> PathBuf {
    env.map(Path::new)
        .filter(|path| path.is_absolute() && exists(path))
        .map(Path::to_path_buf)
        .or_else(|| {
            ["/bin/bash", "/usr/bin/bash"]
                .iter()
                .map(Path::new)
                .find(|path| exists(path))
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

#[derive(Debug, Clone)]
pub struct LaunchSpec {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

struct History {
    data: String,
    end_offset: u64,
    path: Option<PathBuf>,
    rewrite: bool,
}

impl History {
    fn load(path: Option<PathBuf>) -> Self {
        let restored = path.as_ref().and_then(|path| {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
                .ok()?;
            if !file.metadata().ok()?.is_file() {
                return None;
            }
            let mut bytes = Vec::new();
            file.take(SCROLLBACK_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .ok()?;
            (bytes.len() <= SCROLLBACK_LIMIT)
                .then(|| String::from_utf8(bytes).ok())
                .flatten()
        });
        let rewrite = restored.is_none();
        let data = restored.unwrap_or_default();
        Self {
            end_offset: data.len() as u64,
            data,
            path,
            rewrite,
        }
    }

    fn push(&mut self, data: &str) {
        self.end_offset += data.len() as u64;
        self.rewrite |= push_scrollback(&mut self.data, data);
        if let Some(path) = &self.path {
            // Appends are cheap between compactions. Replace atomically at
            // the bound (or after any incomplete append).
            self.rewrite = if self.rewrite {
                crate::fsx::atomic_write(path, self.data.as_bytes(), Some(0o600)).is_err()
            } else {
                OpenOptions::new()
                    .append(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(path)
                    .and_then(|mut file| file.write_all(data.as_bytes()))
                    .is_err()
            };
        }
    }
}

/// Keep the tail at line and UTF-8 boundaries when a nearby line is available.
fn push_scrollback(buffer: &mut String, data: &str) -> bool {
    buffer.push_str(data);
    if buffer.len() <= SCROLLBACK_LIMIT {
        return false;
    }
    let mut cut = buffer.len() - SCROLLBACK_LIMIT + TRIM_HEADROOM;
    let window = &buffer.as_bytes()[cut..buffer.len().min(cut + TRIM_SEARCH)];
    if let Some(newline) = window.iter().position(|byte| *byte == b'\n') {
        cut += newline + 1;
    }
    while !buffer.is_char_boundary(cut) {
        cut += 1;
    }
    buffer.drain(..cut);
    true
}

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

struct Live {
    master: Box<dyn MasterPty + Send>,
    writer: Writer,
    killer: Option<Box<dyn ChildKiller + Send + Sync>>,
    pid: u32,
}

struct State {
    generation: u32,
    status: SessionStatus,
    history: History,
    live: Option<Live>,
}

impl State {
    fn snapshot(&self) -> TerminalSnapshot {
        TerminalSnapshot {
            generation: self.generation,
            status: self.status.clone(),
            start_offset: self.history.end_offset - self.history.data.len() as u64,
            end_offset: self.history.end_offset,
            data: self.history.data.clone(),
        }
    }

    fn set_status(&mut self, id: &str, sink: &EventSink, status: SessionStatus) {
        self.status = status.clone();
        emit(
            sink,
            SessionEvent::Status {
                id: id.into(),
                generation: self.generation,
                status,
            },
        );
    }

    fn output(&mut self, id: &str, sink: &EventSink, data: String) {
        let offset = self.history.end_offset;
        self.history.push(&data);
        emit(
            sink,
            SessionEvent::Output {
                id: id.into(),
                generation: self.generation,
                offset,
                data,
            },
        );
    }
}

type Slot = Arc<Mutex<State>>;
type Sink = Arc<dyn Fn(SessionEvent) + Send + Sync>;
type EventSink = RwLock<Option<Sink>>;

#[derive(Default)]
pub struct Supervisor {
    slots: Mutex<HashMap<String, Slot>>,
    sink: Arc<EventSink>,
    next_generation: AtomicU32,
    history_dir: Option<PathBuf>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.locked()
}

fn emit(sink: &EventSink, event: SessionEvent) {
    let sink = sink.read_lock().clone();
    if let Some(sink) = sink {
        sink(event);
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(5),
        cols: cols.max(20),
        pixel_width: 0,
        pixel_height: 0,
    }
}

struct Process {
    child: Box<dyn Child + Send + Sync>,
    reader: File,
}

impl Supervisor {
    pub fn new(history_dir: PathBuf) -> Self {
        Self {
            history_dir: Some(history_dir),
            ..Self::default()
        }
    }

    pub fn set_sink(&self, sink: impl Fn(SessionEvent) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }

    fn history_path(&self, id: &str) -> Option<PathBuf> {
        self.history_dir
            .as_ref()
            .map(|dir| dir.join(sha256_text(id)))
    }

    fn slot(&self, id: &str, persist: bool) -> Slot {
        lock(&self.slots)
            .entry(id.into())
            .or_insert_with(|| {
                let path = if persist { self.history_path(id) } else { None };
                Arc::new(Mutex::new(State {
                    generation: 0,
                    status: SessionStatus::Stopped,
                    history: History::load(path),
                    live: None,
                }))
            })
            .clone()
    }

    fn existing_slot(&self, id: &str) -> Option<Slot> {
        lock(&self.slots).get(id).cloned()
    }

    pub fn status(&self, id: &str) -> SessionStatus {
        self.existing_slot(id)
            .map_or(SessionStatus::Stopped, |slot| lock(&slot).status.clone())
    }

    pub fn snapshot(&self, id: &str, persist: bool) -> TerminalSnapshot {
        lock(&self.slot(id, persist)).snapshot()
    }

    /// Start only from the state the caller observed. Repeated attach requests
    /// for that generation cannot replace the resulting process, even if it
    /// has already exited. A launch failure remains visible until retried.
    pub fn attach(
        &self,
        id: &str,
        generation: u32,
        retry: bool,
        spec: impl FnOnce() -> Result<LaunchSpec>,
    ) -> (TerminalSnapshot, bool) {
        let slot = self.slot(id, true);
        let mut state = lock(&slot);
        let can_start = match state.status {
            SessionStatus::Stopped | SessionStatus::Exited { .. } => true,
            SessionStatus::Failed { .. } => retry,
            _ => false,
        };
        let started = generation == state.generation
            && can_start
            && self.start_locked(id, &slot, &mut state, spec).is_ok();
        (state.snapshot(), started)
    }

    pub fn start(&self, id: &str, spec: impl FnOnce() -> Result<LaunchSpec>) -> Result<()> {
        let slot = self.slot(id, false);
        let mut state = lock(&slot);
        if state.status.is_live() {
            return Err(Error::Conflict("This session is already running".into()));
        }
        self.start_locked(id, &slot, &mut state, spec)
    }

    fn start_locked(
        &self,
        id: &str,
        slot: &Slot,
        state: &mut State,
        spec: impl FnOnce() -> Result<LaunchSpec>,
    ) -> Result<()> {
        state.generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
        state.set_status(id, &self.sink, SessionStatus::Starting);
        let result = spec().and_then(|spec| self.spawn(id, slot, state, spec));
        if let Err(error) = &result {
            state.set_status(
                id,
                &self.sink,
                SessionStatus::Failed {
                    message: error.to_string(),
                },
            );
        }
        result
    }

    fn spawn(&self, id: &str, slot: &Slot, state: &mut State, spec: LaunchSpec) -> Result<()> {
        if !spec.cwd.is_dir() {
            return Err(Error::invalid(format!(
                "The workspace folder {} does not exist",
                spec.cwd.display()
            )));
        }
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| Error::invalid("empty command"))?;
        let pair = native_pty_system()
            .openpty(size(spec.cols, spec.rows))
            .map_err(|error| Error::Internal(format!("Could not open a terminal: {error}")))?;
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| Error::Internal("The terminal has no file descriptor".into()))?;
        // The reader keeps its own descriptor as the supervisor may drop the
        // writable end during archive or deletion.
        let reader = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .map(File::from)
            .map_err(|error| Error::io("open terminal reader", error))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| Error::Internal(error.to_string()))?;
        let (send, receive) = mpsc::channel();
        let worker_id = id.to_string();
        let generation = state.generation;
        let worker_slot = slot.clone();
        let sink = self.sink.clone();
        std::thread::Builder::new()
            .name(format!("pty-{id}"))
            .spawn(move || {
                if let Ok(process) = receive.recv() {
                    supervise(&worker_id, generation, &worker_slot, &sink, process);
                }
            })
            .map_err(|error| Error::io("start terminal supervisor", error))?;

        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(&spec.cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| Error::Internal(format!("Could not start {program}: {error}")))?;
        drop(pair.slave);
        let pid = child.process_id().unwrap_or_default();
        state.live = Some(Live {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
            killer: Some(child.clone_killer()),
            pid,
        });
        if state.history.path.is_some() {
            if !state.history.data.is_empty() {
                state.history.push(SHELL_BOUNDARY);
            }
        } else {
            state.history = History::load(None);
        }
        // The receiver can only go away once the worker has received Process.
        let _ = send.send(Process { child, reader });
        state.set_status(id, &self.sink, SessionStatus::Running { pid });
        Ok(())
    }

    pub fn write(&self, id: &str, generation: u32, data: &str) -> Result<()> {
        let writer = self
            .existing_slot(id)
            .and_then(|slot| {
                let state = lock(&slot);
                state
                    .live
                    .as_ref()
                    .filter(|live| state.generation == generation && live.killer.is_some())
                    .map(|live| live.writer.clone())
            })
            .ok_or_else(|| {
                Error::Conflict("This terminal generation is no longer running".into())
            })?;
        let mut writer = lock(&writer);
        writer
            .write_all(data.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(|error| Error::io("write to terminal", error))
    }

    pub fn resize(&self, id: &str, generation: u32, cols: u16, rows: u16) -> Result<()> {
        if let Some(slot) = self.existing_slot(id) {
            let state = lock(&slot);
            if let Some(live) = state
                .live
                .as_ref()
                .filter(|_| state.generation == generation)
            {
                live.master
                    .resize(size(cols, rows))
                    .map_err(|error| Error::Internal(format!("resize: {error}")))?;
            }
        }
        Ok(())
    }

    pub fn stop(&self, id: &str) -> Result<()> {
        if let Some(slot) = self.existing_slot(id)
            && let Some(killer) = lock(&slot)
                .live
                .as_mut()
                .and_then(|live| live.killer.as_mut())
        {
            killer
                .kill()
                .map_err(|error| Error::io("stop session", error))?;
        }
        Ok(())
    }

    /// Archive a terminal without discarding its saved history.
    pub fn deactivate(&self, id: &str) {
        if let Some(slot) = self.existing_slot(id) {
            let mut state = lock(&slot);
            terminate(&mut state);
            if state.history.path.is_none() {
                state.history = History::load(None);
            }
            state.generation = self.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
            state.set_status(id, &self.sink, SessionStatus::Stopped);
        }
    }

    /// Retire the process before deleting its history, so a late read cannot
    /// recreate the file after deletion.
    pub fn forget(&self, id: &str) -> Result<()> {
        if let Some(slot) = lock(&self.slots).remove(id) {
            terminate(&mut lock(&slot));
        }
        if let Some(path) = self.history_path(id)
            && let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(Error::path("remove terminal history", &path, error));
        }
        Ok(())
    }

    pub fn stop_all(&self) {
        for (_, slot) in lock(&self.slots).drain() {
            terminate(&mut lock(&slot));
        }
    }
}

fn terminate(state: &mut State) {
    if let Some(live) = state.live.take()
        && live.killer.is_some()
        && let Ok(pid) = i32::try_from(live.pid)
        && pid > 0
    {
        // portable-pty launches the child in its own session/process group.
        // The group and child both use this ID until the worker reaps it.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
}

/// Poll the master so we can reap the shell even if another process still
/// has the slave open. After exit, drain the remaining output before the
/// exit event, within the same generation.
fn supervise(id: &str, generation: u32, slot: &Slot, sink: &EventSink, process: Process) {
    let Process {
        mut child,
        mut reader,
    } = process;
    let mut decoder = Utf8Stream::default();
    let mut buffer = [0; 16 * 1024];
    let mut reader_open = true;
    let mut exit = None;
    let mut tail = 0;
    loop {
        {
            let mut state = lock(slot);
            if state.generation != generation || state.live.is_none() {
                drop(state);
                let _ = child.wait();
                return;
            }
            if exit.is_none() {
                exit = match child.try_wait() {
                    Ok(Some(code)) => Some(SessionStatus::Exited {
                        code: Some(code.exit_code() as i32),
                    }),
                    Ok(None) => None,
                    Err(error) => Some(SessionStatus::Failed {
                        message: format!("Could not wait for the terminal: {error}"),
                    }),
                };
                if exit.is_some() {
                    state.live.as_mut().unwrap().killer = None;
                }
            }
            if (!reader_open || tail >= 128)
                && let Some(status) = exit
            {
                state.live = None;
                state.set_status(id, sink, status);
                return;
            }
        }
        let mut fd = libc::pollfd {
            fd: if reader_open { reader.as_raw_fd() } else { -1 },
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut fd, 1, if exit.is_some() { 0 } else { 50 }) };
        if ready <= 0 {
            if ready < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if ready < 0 || exit.is_some() {
                reader_open = false;
            }
            continue;
        }
        match reader.read(&mut buffer) {
            Ok(0) => reader_open = false,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => reader_open = false,
            Ok(count) => {
                let data = decoder.push(&buffer[..count]);
                if exit.is_some() {
                    tail += 1;
                }
                let mut state = lock(slot);
                if !data.is_empty() && state.generation == generation && state.live.is_some() {
                    state.output(id, sink, data);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn wait_for(mut done: impl FnMut() -> bool) {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(10), "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn spec(argv: &[&str]) -> LaunchSpec {
        LaunchSpec {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            cwd: std::env::temp_dir(),
            env: vec![("GREETING".into(), "hello-pty".into())],
            cols: 80,
            rows: 24,
        }
    }

    #[test]
    fn runs_a_process_streams_output_and_reports_exit_in_order() {
        let supervisor = Supervisor::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        supervisor.set_sink(move |event| seen.lock().unwrap().push(event));
        supervisor
            .start("s1", || {
                Ok(spec(&["sh", "-c", "printf \"$GREETING\"; exit 7"]))
            })
            .unwrap();
        wait_for(|| matches!(supervisor.status("s1"), SessionStatus::Exited { .. }));
        let snapshot = supervisor.snapshot("s1", false);
        assert_eq!(snapshot.status, SessionStatus::Exited { code: Some(7) });
        assert_eq!(snapshot.data, "hello-pty");
        assert_eq!(snapshot.end_offset, snapshot.data.len() as u64);
        let events = events.lock().unwrap();
        assert!(matches!(
            &events[0],
            SessionEvent::Status {
                status: SessionStatus::Starting,
                ..
            }
        ));
        assert!(matches!(
            &events[1],
            SessionEvent::Status {
                status: SessionStatus::Running { .. },
                ..
            }
        ));
        assert!(
            matches!(&events[2], SessionEvent::Output { generation, offset: 0, .. } if *generation == snapshot.generation)
        );
        assert!(matches!(
            &events[3],
            SessionEvent::Status {
                status: SessionStatus::Exited { .. },
                ..
            }
        ));
    }

    #[test]
    fn concurrent_attach_starts_once_and_cannot_restart_an_exited_generation() {
        let supervisor = Supervisor::default();
        let results = std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        supervisor.attach("s2", 0, false, || Ok(spec(&["sh", "-c", "printf once"])))
                    })
                })
                .collect();
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|(_, started)| *started).count(), 1);
        let generation = results[0].0.generation;
        assert!(
            results
                .iter()
                .all(|(snapshot, _)| snapshot.generation == generation)
        );
        wait_for(|| matches!(supervisor.status("s2"), SessionStatus::Exited { .. }));
        let (old, restarted) = supervisor.attach("s2", 0, false, || panic!("stale attach started"));
        assert!(!restarted);
        assert_eq!(old.generation, generation);
        let (next, started) = supervisor.attach("s2", generation, false, || Ok(spec(&["cat"])));
        assert!(started);
        assert!(next.generation > generation);
        assert!(supervisor.write("s2", generation, "stale\n").is_err());
        supervisor.write("s2", next.generation, "ping\n").unwrap();
        wait_for(|| supervisor.snapshot("s2", true).data.contains("ping"));
        supervisor.resize("s2", next.generation, 120, 40).unwrap();
        supervisor.stop("s2").unwrap();
        wait_for(|| !supervisor.status("s2").is_live());
    }

    #[test]
    fn a_failed_attach_requires_an_explicit_retry() {
        let supervisor = Supervisor::default();
        let (failed, started) = supervisor.attach("s3", 0, false, || {
            let mut spec = spec(&["true"]);
            spec.cwd = PathBuf::from("/definitely/not/here");
            Ok(spec)
        });
        assert!(!started);
        assert!(matches!(failed.status, SessionStatus::Failed { .. }));
        let (still_failed, started) = supervisor.attach("s3", failed.generation, false, || {
            panic!("unexpected retry")
        });
        assert!(!started);
        assert_eq!(still_failed, failed);
        let (retried, started) =
            supervisor.attach("s3", failed.generation, true, || Ok(spec(&["true"])));
        assert!(started && retried.generation > failed.generation);
        wait_for(|| !supervisor.status("s3").is_live());
    }

    #[test]
    fn persisted_scrollback_is_private_bounded_and_removed_on_forget() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let supervisor = Supervisor::new(dir.path().to_owned());
        let (_, started) = supervisor.attach("s4", 0, false, || Ok(spec(&[
            "sh", "-c", "i=0; while [ $i -lt 24000 ]; do printf 'é字 abcdefghijklmnopqrstuvwxyz\\n'; i=$((i+1)); done; printf marker",
        ])));
        assert!(started);
        wait_for(|| !supervisor.status("s4").is_live());
        let snapshot = supervisor.snapshot("s4", true);
        assert!(snapshot.start_offset > 0);
        assert!(snapshot.data.ends_with("marker"));
        assert_eq!(
            snapshot.end_offset - snapshot.start_offset,
            snapshot.data.len() as u64
        );
        let path = supervisor.history_path("s4").unwrap();
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
        assert!(fs::metadata(&path)?.len() <= SCROLLBACK_LIMIT as u64);
        supervisor.deactivate("s4");
        let archived = supervisor.snapshot("s4", true);
        assert_eq!(archived.status, SessionStatus::Stopped);
        assert!(archived.generation > snapshot.generation);
        assert_eq!(archived.data, snapshot.data);
        let reopened = Supervisor::new(dir.path().to_owned());
        assert_eq!(reopened.snapshot("s4", true).data, snapshot.data);
        let (_, started) = reopened.attach("s4", 0, false, || Ok(spec(&["cat"])));
        assert!(started);
        assert!(reopened.snapshot("s4", true).data.contains("marker"));
        reopened.forget("s4")?;
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn scrollback_trims_at_a_line_start() {
        let mut buffer = String::new();
        let line = "\x1b[38;5;208mcoloured\x1b[0m output line\r\n";
        for _ in 0..(3 * SCROLLBACK_LIMIT / line.len()) {
            push_scrollback(&mut buffer, line);
        }
        assert!(buffer.len() <= SCROLLBACK_LIMIT);
        assert!(buffer.starts_with("\x1b[38;5;208m"), "{:?}", &buffer[..20]);
    }

    #[test]
    fn scrollback_is_bounded_on_char_boundaries() {
        let mut buffer = String::new();
        for _ in 0..(SCROLLBACK_LIMIT / 3 + 10) {
            push_scrollback(&mut buffer, "é字");
        }
        assert!(buffer.len() <= SCROLLBACK_LIMIT);
    }

    #[test]
    fn terminal_uses_the_login_shell_with_fallbacks() {
        let only = |present: &'static [&'static str]| {
            move |path: &Path| present.iter().any(|p| Path::new(p) == path)
        };
        assert_eq!(
            pick_shell(Some("/usr/bin/zsh"), only(&["/usr/bin/zsh", "/bin/bash"])),
            PathBuf::from("/usr/bin/zsh")
        );
        assert_eq!(
            pick_shell(Some("zsh"), only(&["/bin/bash"])),
            PathBuf::from("/bin/bash")
        );
        assert_eq!(
            pick_shell(Some("/usr/bin/fish"), only(&["/bin/bash"])),
            PathBuf::from("/bin/bash")
        );
        assert_eq!(pick_shell(None, only(&[])), PathBuf::from("/bin/sh"));
    }
}
