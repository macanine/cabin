//! Guest half of the experimental native CLI. Runs as the guest user.

use crate::native_channel::*;
use crate::paths::AppPaths;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

/// After the job exits its output drains until this long passes with nothing
/// to read. Something it left running may hold the pipe open for ever.
const DRAIN_QUIET: Duration = Duration::from_secs(1);
/// Read after the exit at most: more than a pipe holds (1 MiB by default), so
/// the job's own output is always complete.
const DRAIN_BYTES: usize = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(50);

#[derive(Clone, Copy)]
enum Lane {
    Exec,
    Sync,
    Watch,
}

pub fn serve(which: &str, device: &Path, paths: &AppPaths) -> io::Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        return Err(io::Error::other(
            "native channel must run as the guest user, not root",
        ));
    }
    let lane = match which {
        "exec" => Lane::Exec,
        "sync" => Lane::Sync,
        "watch" => Lane::Watch,
        _ => return Err(io::Error::other("expected exec, sync or watch")),
    };
    let mut reader = OpenOptions::new().read(true).write(true).open(device)?;
    let writer = Mutex::new(reader.try_clone()?);
    let mut body = vec![];
    loop {
        match read_frame(&mut reader, &mut body) {
            Ok(START) => {
                let result = serde_json::from_slice::<Request>(&body)
                    .map_err(io::Error::other)
                    .and_then(|request| {
                        if request.version != VERSION {
                            return Err(io::Error::other("native protocol version mismatch"));
                        }
                        run(request.job, lane, paths, &mut reader, &writer)
                    });
                if let Err(error) = result {
                    let _ = send(&writer, FAILURE, error.to_string().as_bytes());
                }
            }
            Ok(_) => {} // A closing frame or residue from a disconnected client.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
                ) =>
            {
                // The virtio port returns EOF while QEMU has no host client.
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }
}

fn send(out: &Mutex<File>, kind: u8, body: &[u8]) -> io::Result<()> {
    frame(
        &mut *out.lock().unwrap_or_else(|e| e.into_inner()),
        kind,
        body,
    )
}

fn agent_kind(name: &str) -> io::Result<crate::model::AgentKind> {
    use crate::model::AgentKind::*;
    match name {
        "claude" => Ok(Claude),
        "codex" => Ok(Codex),
        "agy" => Ok(Antigravity),
        "opencode" => Ok(Opencode),
        "pi" => Ok(Pi),
        _ => Err(io::Error::other("unknown native agent")),
    }
}

fn run(
    job: Job,
    lane: Lane,
    paths: &AppPaths,
    reader: &mut File,
    writer: &Mutex<File>,
) -> io::Result<()> {
    match (job, lane) {
        (
            Job::Agent {
                name,
                argv,
                cwd,
                tty,
            },
            Lane::Exec,
        ) => {
            let kind = agent_kind(&name)?;
            let path = paths.child_path();
            let binary = crate::agents::find_binary_on_path(&path, kind).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "{name} is not installed in the Cabin VM. Open Cabin → Agents → Setup."
                    ),
                )
            })?;
            let cwd = crate::sync::expand_home(&cwd);
            if !cwd.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("guest project directory does not exist: {}", cwd.display()),
                ));
            }
            if let Some(size) = tty {
                let pair = native_pty_system()
                    .openpty(pty_size(
                        size.cols,
                        size.rows,
                        size.pixel_width,
                        size.pixel_height,
                    ))
                    .map_err(io::Error::other)?;
                let mut cmd = CommandBuilder::new(binary);
                cmd.cwd(&cwd);
                cmd.args(argv);
                cmd.env("PATH", &path);
                cmd.env("TERM", size.term);
                if let Some(color) = size.color_term {
                    cmd.env("COLORTERM", color);
                }
                for (key, value) in crate::agents::adapter(kind).launch_env() {
                    cmd.env(key, value);
                }
                                // portable-pty's writer injects LF + VEOF from Drop. A plain
                // duplicate closes without ever synthesizing input bytes.
                let fd = pair.master.as_raw_fd().ok_or_else(|| io::Error::other("PTY master has no descriptor"))?;
                let input = clone_master_fd(fd)?;
                let output = clone_master_fd(fd)?;
                let mut child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
                drop(pair.slave);
                let pid = child.process_id();
                pump(
                    reader,
                    writer,
                    input,
                    output,
                    None::<File>,
                    pid,
                    false,
                    move |cols, rows, width, height| {
                        let _ = pair.master.resize(pty_size(cols, rows, width, height));
                    },
                    || child.wait().map(|s| s.exit_code().min(255) as u8),
                )
            } else {
                let mut cmd = Command::new(binary);
                cmd.args(argv)
                    .current_dir(cwd)
                    .env("PATH", path)
                    .envs(crate::agents::adapter(kind).launch_env());
                run_pipes(cmd, reader, writer, false)
            }
        }
        (Job::Sync { operation, root }, lane)
            if matches!(
                (operation.as_str(), lane),
                ("watch", Lane::Watch) | ("scan" | "pack" | "apply", Lane::Sync)
            ) =>
        {
            let mut cmd = Command::new(std::env::current_exe()?);
            cmd.args(["sync", &operation, &root]);
            run_pipes(cmd, reader, writer, matches!(lane, Lane::Watch))
        }
        _ => Err(io::Error::other("request not allowed on this channel")),
    }
}

fn clone_master_fd(fd: RawFd) -> io::Result<File> {
    // The master outlives this borrow. try_clone_to_owned also sets CLOEXEC.
    unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned().map(File::from)
}

fn pty_size(cols: u16, rows: u16, pixel_width: u16, pixel_height: u16) -> PtySize {
    PtySize {
        cols: cols.max(1),
        rows: rows.max(1),
        pixel_width,
        pixel_height,
    }
}

fn run_pipes(
    mut cmd: Command,
    reader: &mut File,
    writer: &Mutex<File>,
    watch: bool,
) -> io::Result<()> {
    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    pump(
        reader,
        writer,
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        child.stderr.take(),
        Some(child.id()),
        watch,
        |_, _, _, _| {},
        || {
            child.wait().map(|s| {
                s.code()
                    .or_else(|| s.signal().map(|signal| 128 + signal))
                    .unwrap_or(1) as u8
            })
        },
    )
}

/// Whether `fd` has data or hung up within `POLL`.
fn readable(fd: RawFd) -> io::Result<bool> {
    let mut poll = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    match unsafe { libc::poll(&mut poll, 1, POLL.as_millis() as libc::c_int) } {
        -1 => match io::Error::last_os_error() {
            error if error.kind() == io::ErrorKind::Interrupted => Ok(false),
            error => Err(error),
        },
        ready => Ok(ready > 0),
    }
}

fn output(
    mut source: impl Read + AsRawFd,
    writer: &Mutex<File>,
    kind: u8,
    chunk: usize,
    finished: &AtomicBool,
) -> io::Result<()> {
    let mut body = vec![0; chunk];
    let mut quiet_since: Option<Instant> = None;
    let mut drained = 0;
    loop {
        // Read before polling: an exit seen here cannot hide earlier output.
        let exited = finished.load(Ordering::Acquire);
        if !readable(source.as_raw_fd())? {
            if exited && quiet_since.get_or_insert_with(Instant::now).elapsed() >= DRAIN_QUIET {
                return Ok(());
            }
            continue;
        }
        quiet_since = None;
        match source.read(&mut body) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                send(writer, kind, &body[..n])?;
                if exited {
                    drained += n;
                    if drained >= DRAIN_BYTES {
                        return Ok(());
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(()), // PTY hangup.
            Err(error) => return Err(error),
        }
    }
}

fn kill_job(pid: Option<u32>) {
    if let Some(pid) = pid {
        // Both kinds of child have their own process group.
        unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
    }
}

#[allow(clippy::too_many_arguments)]
fn pump(
    reader: &mut File,
    writer: &Mutex<File>,
    input: impl Write + Send,
    stdout: impl Read + AsRawFd + Send,
    stderr: Option<impl Read + AsRawFd + Send>,
    pid: Option<u32>,
    watch: bool,
    resize: impl Fn(u16, u16, u16, u16) + Send,
    wait: impl FnOnce() -> io::Result<u8>,
) -> io::Result<()> {
    let finished = AtomicBool::new(false);
    let tty = stderr.is_none();
    std::thread::scope(|scope| {
        let finished = &finished;
        let inbound = scope.spawn(move || {
            let mut input = Some(input);
            let mut body = vec![];
            loop {
                match read_frame(reader, &mut body) {
                    Ok(INPUT) => {
                        if let Some(out) = &mut input {
                            if out.write_all(&body).is_err() {
                                input = None;
                            }
                        }
                    }
                    Ok(EOF) if !tty => {
                        input = None;
                        if watch && !finished.load(Ordering::Acquire) {
                            kill_job(pid);
                        }
                    }
                    Ok(RESIZE) if tty && matches!(body.len(), 4 | 8) => {
                        resize(
                            u16::from_be_bytes([body[0], body[1]]),
                            u16::from_be_bytes([body[2], body[3]]),
                            body.get(4..6)
                                .map_or(0, |v| u16::from_be_bytes([v[0], v[1]])),
                            body.get(6..8)
                                .map_or(0, |v| u16::from_be_bytes([v[0], v[1]])),
                        );
                    }
                    Ok(_) => break,
                    Err(_) => break,
                }
            }
            if !finished.load(Ordering::Acquire) {
                kill_job(pid);
            }
        });
        let ready = send(writer, READY, &[]);
        let out = scope.spawn(|| output(stdout, writer, OUT, if tty { 8192 } else { MAX }, finished));
        let err = stderr.map(|s| scope.spawn(move || output(s, writer, ERR, 8192, finished)));
        let status = wait();
        finished.store(true, Ordering::Release);
        let sent = (|| {
            out.join()
                .map_err(|_| io::Error::other("native output worker stopped"))??;
            if let Some(err) = err {
                err.join()
                    .map_err(|_| io::Error::other("native error worker stopped"))??;
            }
            ready?;
            send(writer, EXIT, &[status?])
        })();
        // The host acknowledges the exit or closes the port. Do not read the next
        // start until the previous reader has relinquished the device.
        let _ = inbound.join();
        sent?;
        send(writer, CLOSED, &[])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    /// Run `script` on one end of a channel; the test is the host on the other.
    fn job(script: &str) -> (UnixStream, mpsc::Receiver<io::Result<()>>) {
        let (host, guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        let (done, result) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = File::from(OwnedFd::from(guest));
            let writer = Mutex::new(reader.try_clone().unwrap());
            let _ = done.send(run_pipes(cmd, &mut reader, &writer, false));
        });
        (host, result)
    }

    /// Everything up to the exit: its output and its code.
    fn until_exit(host: &mut UnixStream) -> (Vec<u8>, u8) {
        let (mut out, mut body) = (vec![], vec![]);
        loop {
            match read_frame(host, &mut body).expect("the channel went quiet") {
                OUT => out.extend_from_slice(&body),
                EXIT => return (out, body[0]),
                _ => {}
            }
        }
    }

    fn started(host: &mut UnixStream, expected: &[u8]) {
        let (mut out, mut body) = (vec![], vec![]);
        while !out.ends_with(expected) {
            if read_frame(host, &mut body).unwrap() == OUT {
                out.extend_from_slice(&body);
            }
        }
    }

    #[test]
    fn a_job_reports_its_output_and_exit_then_closes() {
        let (mut host, result) = job("printf hello; exit 7");
        assert_eq!(until_exit(&mut host), (b"hello".to_vec(), 7));
        frame(&mut host, CLOSE, &[]).unwrap();
        assert_eq!(read_frame(&mut host, &mut vec![]).unwrap(), CLOSED);
        result.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    }

    #[test]
    fn large_output_survives_the_exit() {
        let (mut host, _result) = job("head -c 3000000 /dev/zero");
        let (out, code) = until_exit(&mut host);
        assert_eq!((out.len(), code), (3_000_000, 0));
    }

    #[test]
    fn a_background_child_holding_the_pipe_does_not_delay_the_exit() {
        let begun = Instant::now();
        let (mut host, _result) = job("sleep 15 & printf done");
        assert_eq!(until_exit(&mut host), (b"done".to_vec(), 0));
        assert!(begun.elapsed() < Duration::from_secs(8), "{:?}", begun.elapsed());
    }

    #[test]
    fn a_background_child_that_keeps_writing_does_not_delay_the_exit() {
        let begun = Instant::now();
        // Ends on its own when the read end closes.
        let (mut host, _result) = job("yes & exit 3");
        assert_eq!(until_exit(&mut host).1, 3);
        assert!(begun.elapsed() < Duration::from_secs(12), "{:?}", begun.elapsed());
    }

    #[test]
    fn a_vanished_host_ends_the_job() {
        let (mut host, result) = job("printf up; sleep 30");
        started(&mut host, b"up");
        drop(host);
        // Reporting the exit fails; what matters is that the lane is free again.
        assert!(result.recv_timeout(Duration::from_secs(8)).unwrap().is_err());
    }

    #[test]
    fn an_oversized_frame_ends_the_job() {
        let (mut host, _result) = job("printf up; sleep 30");
        started(&mut host, b"up");
        host.write_all(&[INPUT, 0xff, 0xff, 0xff, 0xff]).unwrap();
        assert_eq!(until_exit(&mut host).1, 128 + libc::SIGKILL as u8);
    }

    #[test]
    fn a_resize_during_the_exit_is_harmless() {
        let (mut host, guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let (seen, sizes) = mpsc::channel();
        let (exit, exited) = mpsc::channel();
        let (_keep, output) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = File::from(OwnedFd::from(guest));
            let writer = Mutex::new(reader.try_clone().unwrap());
            pump(&mut reader, &writer, io::sink(), output, None::<File>, None, false,
                move |cols, rows, width, height| { let _ = seen.send((cols, rows, width, height)); },
                move || { let _ = exited.recv(); Ok(0) })
        });
        frame(&mut host, RESIZE, &[0, 80, 0, 24]).unwrap();
        assert_eq!(sizes.recv_timeout(Duration::from_secs(5)).unwrap(), (80, 24, 0, 0));
        exit.send(()).unwrap();
        frame(&mut host, RESIZE, &[0, 100, 0, 30, 3, 32, 1, 224]).unwrap();
        frame(&mut host, RESIZE, &[1, 2, 3]).unwrap(); // Malformed: ends the input.
        assert_eq!(until_exit(&mut host).1, 0);
        assert_eq!(sizes.recv_timeout(Duration::from_secs(5)).unwrap(), (100, 30, 800, 480));
        worker.join().unwrap().unwrap();
    }
}
