//! A line-oriented child process for the ACP transport. Each child gets its
//! own process group so a stop also reaches MCP servers and tools it started.

use crate::util::LockExt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::{Error, Result};

type LineHandler = Arc<dyn Fn(&str) + Send + Sync>;
type ExitHandler = Arc<dyn Fn(Option<i32>, &str, bool) + Send + Sync>;

const LINE_LIMIT: usize = 16 * 1024 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_POLL: Duration = Duration::from_millis(25);

pub struct StdioProcess {
    pid: u32,
    stdin: Mutex<Option<ChildStdin>>,
    child: Arc<Mutex<AgentChild>>,
    stopping: Arc<AtomicBool>,
    exit: Arc<ExitNotice>,
}

struct ExitNotice {
    sent: AtomicBool,
    handler: ExitHandler,
}

impl ExitNotice {
    fn claim(&self) -> bool {
        !self.sent.swap(true, Ordering::AcqRel)
    }

    fn send(&self, code: Option<i32>, detail: &str, expected: bool) {
        if self.claim() {
            (self.handler)(code, detail, expected);
        }
    }
}

struct AgentChild {
    process: Child,
    reaped: bool,
}

impl AgentChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.process.try_wait();
        self.reaped |= !matches!(status, Ok(None));
        status
    }

    fn kill(&mut self) {
        if !self.reaped {
            // The leader may already have exited, but its unreaped pid still
            // identifies the group whose descendants can own the output pipes.
            signal_group(self.process.id(), libc::SIGKILL);
            let _ = self.process.kill();
        }
    }
}

pub struct StdioSpec<'a> {
    pub argv: Vec<String>,
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
}

impl StdioProcess {
    pub fn spawn(
        spec: StdioSpec<'_>,
        on_line: LineHandler,
        on_exit: ExitHandler,
    ) -> Result<Arc<Self>> {
        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| Error::Internal("empty agent command".into()))?;
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(spec.cwd)
            .envs(spec.env.iter().map(|(key, value)| (key, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command
            .spawn()
            .map_err(|error| Error::io(format!("start {program}"), error))?;
        let (Some(stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            kill_and_reap(&mut child);
            return Err(Error::Internal("Missing agent stdio pipes".into()));
        };
        if let Err(error) = nonblocking(&stdin) {
            kill_and_reap(&mut child);
            return Err(Error::Internal(format!(
                "Could not configure agent stdin: {error}"
            )));
        }
        let pid = child.id();
        let child = Arc::new(Mutex::new(AgentChild {
            process: child,
            reaped: false,
        }));
        let stopping = Arc::new(AtomicBool::new(false));
        let process_stopping = stopping.clone();
        let tail = Arc::new(Mutex::new(String::new()));
        let exit = Arc::new(ExitNotice {
            sent: AtomicBool::new(false),
            handler: on_exit,
        });

        let capture = {
            let tail = tail.clone();
            std::thread::Builder::new().spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(read) = stderr.read(&mut buf) {
                    if read == 0 {
                        break;
                    }
                    let mut text = tail.locked();
                    text.push_str(&String::from_utf8_lossy(&buf[..read]));
                    if text.len() > 8_000 {
                        let mut cut = text.len() - 8_000;
                        while !text.is_char_boundary(cut) {
                            cut += 1;
                        }
                        *text = text[cut..].to_string();
                    }
                }
            })
        };
        if let Err(error) = capture {
            kill_and_reap(&mut child.locked().process);
            return Err(Error::Internal(format!(
                "Could not start agent stderr reader: {error}"
            )));
        }
        let reader = {
            let child = child.clone();
            let stopping = stopping.clone();
            let tail = tail.clone();
            let exit = exit.clone();
            std::thread::Builder::new().spawn(move || {
                let mut reader = BufReader::with_capacity(64 * 1024, stdout);
                let mut line = Vec::new();
                let fault = loop {
                    line.clear();
                    match reader
                        .by_ref()
                        .take((LINE_LIMIT + 1) as u64)
                        .read_until(b'\n', &mut line)
                    {
                        Ok(0) => break None,
                        Ok(_) => {}
                        Err(error) => break Some(format!("Could not read agent output: {error}")),
                    }
                    if line.len() > LINE_LIMIT {
                        break Some("Agent emitted a line over 16 MiB".into());
                    }
                    if let Ok(text) = std::str::from_utf8(&line)
                        && !text.trim().is_empty()
                    {
                        let text = text.trim_end_matches(['\r', '\n']);
                        on_line(text);
                    }
                };
                // EOF or a protocol read error ends the transport. A process can
                // close stdout and keep running, so never wait for it indefinitely.
                drop(reader);
                let code = finish(&child).and_then(|status| status.code());
                let expected = stopping.load(Ordering::Acquire);
                let stderr = fault.unwrap_or_else(|| tail.locked().trim().to_string());
                exit.send(code, &stderr, expected);
            })
        };
        if let Err(error) = reader {
            kill_and_reap(&mut child.locked().process);
            return Err(Error::Internal(format!(
                "Could not start agent stdout reader: {error}"
            )));
        }
        Ok(Arc::new(Self {
            pid,
            stdin: Mutex::new(Some(stdin)),
            child,
            stopping: process_stopping,
            exit,
        }))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn write_value(&self, value: &Value, deadline: Option<Instant>) -> Result<()> {
        let own_deadline = Instant::now() + WRITE_TIMEOUT;
        let deadline = deadline.map_or(own_deadline, |deadline| deadline.min(own_deadline));
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        self.write_raw(&line, deadline)
    }

    fn write_raw(&self, text: &str, deadline: Instant) -> Result<()> {
        let mut stdin = loop {
            let remaining = self.write_time_left(deadline)?;
            match self.stdin.try_lock() {
                Ok(stdin) => break stdin,
                Err(TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(TryLockError::WouldBlock) => std::thread::sleep(remaining.min(WRITE_POLL)),
            }
        };
        let stream = stdin
            .as_mut()
            .ok_or_else(|| Error::Unavailable("The agent process is closing".into()))?;
        let mut remaining = text.as_bytes();
        let result = (|| {
            while !remaining.is_empty() {
                let time = self.write_time_left(deadline)?;
                match stream.write(remaining) {
                    Ok(0) => {
                        return Err(Error::io(
                            "write to the agent",
                            io::ErrorKind::WriteZero.into(),
                        ));
                    }
                    Ok(written) => remaining = &remaining[written..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        poll_writable(stream, time.min(WRITE_POLL))
                            .map_err(|error| Error::io("poll agent stdin", error))?;
                    }
                    Err(error) => return Err(Error::io("write to the agent", error)),
                }
            }
            Ok(())
        })();
        if self.stopping.load(Ordering::Acquire) || self.exit.sent.load(Ordering::Acquire) {
            stdin.take();
        } else if let Err(error) = &result
            && (remaining.len() != text.len() || matches!(error, Error::Io { .. }))
        {
            // A truncated JSON line makes this pipe unusable for later requests.
            let pipe = stdin.take();
            drop(stdin);
            self.fail_transport(&error.to_string());
            drop(pipe);
        }
        result
    }

    fn write_time_left(&self, deadline: Instant) -> Result<Duration> {
        if self.stopping.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if self.exit.sent.load(Ordering::Acquire) {
            return Err(Error::Unavailable("The agent process is closing".into()));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Unavailable(
                "The agent did not read the request in time".into(),
            ));
        }
        Ok(remaining)
    }

    pub fn fail_transport(&self, detail: &str) {
        if self.stopping.load(Ordering::Acquire) || !self.exit.claim() {
            return;
        }
        self.child.locked().kill();
        // Notify the peer directly. A descendant can keep stdout open after
        // the provider has exited, so the reader cannot own this notification.
        (self.exit.handler)(None, detail, false);
        self.close_stdin();
    }

    pub fn stop(&self) {
        if self.stopping.swap(true, Ordering::AcqRel) {
            return;
        }
        self.close_stdin();
        finish(&self.child);
    }

    fn close_stdin(&self) {
        // An active writer sees the stop or exit flag and closes the pipe.
        match self.stdin.try_lock() {
            Ok(mut stdin) => {
                stdin.take();
            }
            Err(TryLockError::Poisoned(error)) => {
                error.into_inner().take();
            }
            Err(TryLockError::WouldBlock) => {}
        }
    }
}

fn nonblocking(stream: &ChildStdin) -> io::Result<()> {
    // SAFETY: the descriptor is live for the duration of both fcntl calls.
    let fd = stream.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn poll_writable(stream: &ChildStdin, timeout: Duration) -> io::Result<()> {
    let mut fd = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: libc::POLLOUT,
        revents: 0,
    };
    // SAFETY: poll sees one initialized entry and does not retain its pointer.
    if unsafe { libc::poll(&mut fd, 1, timeout.as_millis().max(1) as i32) } < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Ok(())
}

fn finish(child: &Mutex<AgentChild>) -> Option<ExitStatus> {
    for (timeout, signal) in [(700, libc::SIGTERM), (900, libc::SIGKILL)] {
        let deadline = Instant::now() + Duration::from_millis(timeout);
        loop {
            {
                let mut child = child.locked();
                match child.try_wait() {
                    Ok(Some(status)) => return Some(status),
                    // A lost child is no longer ours to signal.
                    Err(_) => return None,
                    Ok(None) if Instant::now() >= deadline => {
                        if signal == libc::SIGKILL {
                            child.kill();
                        } else {
                            signal_group(child.process.id(), signal);
                        }
                        break;
                    }
                    Ok(None) => {}
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    loop {
        let status = child.locked().try_wait().ok()?;
        if status.is_some() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill_and_reap(child: &mut Child) {
    signal_group(child.id(), libc::SIGKILL);
    let _ = child.kill();
    let _ = child.wait();
}

fn signal_group(pid: u32, signal: i32) {
    // SAFETY: `kill` only receives an integer process-group id and signal.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn provider(
        script: &str,
        on_line: LineHandler,
        on_exit: ExitHandler,
    ) -> Result<Arc<StdioProcess>> {
        StdioProcess::spawn(
            StdioSpec {
                argv: ["/bin/sh", "-c", script].map(str::to_owned).to_vec(),
                cwd: Path::new("/"),
                env: &[],
            },
            on_line,
            on_exit,
        )
    }

    #[test]
    fn stdout_eof_does_not_wait_for_a_running_provider() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let process = provider(
            "exec 1>&-; exec sleep 30",
            Arc::new(|_| {}),
            Arc::new(move |_, _, expected| {
                let _ = tx.send(expected);
            }),
        )?;
        assert_eq!(rx.recv_timeout(Duration::from_secs(4)), Ok(false));
        process.stop();
        Ok(())
    }

    #[test]
    fn a_large_request_streams_when_the_provider_drains_the_pipe() -> Result<()> {
        let payload = serde_json::json!({"data": "x".repeat(12 * 1024 * 1024)});
        let bytes = payload.to_string().len() + 1;
        let (tx, rx) = mpsc::channel();
        let process = provider(
            &format!("head -c {bytes} >/dev/null && printf 'read\\n'"),
            Arc::new(move |line| {
                let _ = tx.send(line.to_owned());
            }),
            Arc::new(|_, _, _| {}),
        )?;
        let written = process.write_value(&payload, Some(Instant::now() + Duration::from_secs(10)));
        process.stop();
        written?;
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3)).as_deref(),
            Ok("read")
        );
        Ok(())
    }

    #[test]
    fn an_incomplete_write_times_out_and_closes_the_connection() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let process = provider(
            "exec sleep 30",
            Arc::new(|_| {}),
            Arc::new(move |_, message, expected| {
                let _ = tx.send((message.to_owned(), expected));
            }),
        )?;
        let result = process.write_raw(
            &"x".repeat(1024 * 1024),
            Instant::now() + Duration::from_millis(200),
        );
        let exit = rx.recv_timeout(Duration::from_secs(3));
        process.stop();
        assert!(
            matches!(result, Err(Error::Unavailable(message)) if message.contains("read the request in time"))
        );
        assert!(
            matches!(exit, Ok((message, false)) if message.contains("read the request in time"))
        );
        Ok(())
    }

    #[test]
    fn stopping_the_provider_interrupts_a_stalled_write() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let process = provider(
            "head -c 1 >/dev/null; printf 'ready\\n'; exec sleep 30",
            Arc::new(move |_| {
                let _ = tx.send(());
            }),
            Arc::new(|_, _, _| {}),
        )?;
        let writer = process.clone();
        let worker = std::thread::spawn(move || {
            writer.write_raw(
                &"x".repeat(1024 * 1024),
                Instant::now() + Duration::from_secs(10),
            )
        });
        let ready = rx.recv_timeout(Duration::from_secs(3));
        process.stop();
        assert_eq!(ready, Ok(()));
        assert!(matches!(worker.join().unwrap(), Err(Error::Cancelled)));
        Ok(())
    }

    #[test]
    fn waiting_for_the_writer_uses_the_caller_deadline() -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let process = provider(
            "IFS= read -r line && printf '%s\\n' \"$line\"",
            Arc::new(move |line| {
                let _ = tx.send(line.to_owned());
            }),
            Arc::new(|_, _, _| {}),
        )?;
        let held = process.stdin.locked();
        let timed_out =
            process.write_raw("discarded\n", Instant::now() + Duration::from_millis(50));
        drop(held);
        let sent = process.write_raw("sent\n", Instant::now() + Duration::from_secs(3));
        process.stop();
        assert!(matches!(timed_out, Err(Error::Unavailable(_))));
        sent?;
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(3)).as_deref(),
            Ok("sent")
        );
        Ok(())
    }
}
