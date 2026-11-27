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

/// Adaptive sleep while no host client is connected to the virtio port: stays
/// responsive (2 ms) right after activity for back-to-back sync jobs, then
/// ramps down to 50 ms to avoid burning guest CPU and host VM exits while idle.
fn idle_backoff(elapsed: Duration) -> Duration {
    if elapsed < Duration::from_secs(3) {
        Duration::from_millis(2)
    } else if elapsed < Duration::from_secs(10) {
        Duration::from_millis(15)
    } else {
        Duration::from_millis(50)
    }
}

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
    let mut idle_since: Option<Instant> = None;
    loop {
        match read_frame(&mut reader, &mut body) {
            Ok(START) => {
                idle_since = None;
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
            Ok(_) => {
                idle_since = None; // A closing frame or residue from a disconnected client.
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::InvalidData
                ) =>
            {
                // The virtio port returns EOF while QEMU has no host client.
                let since = idle_since.get_or_insert_with(Instant::now);
                std::thread::sleep(idle_backoff(since.elapsed()));
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

/// Parse `export LANG='...'` and `export TZ='...'` from `environment.sh`
/// without invoking a shell.
fn read_guest_environment(paths: &AppPaths) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(paths.environment_script()) else {
        return Vec::new();
    };
    parse_environment_script(&text)
}

fn parse_environment_script(text: &str) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("export ") else {
            continue;
        };
        let Some((key, raw)) = rest.split_once('=') else {
            continue;
        };
        if !matches!(key, "LANG" | "TZ") {
            continue;
        }
        let unquoted = raw
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .map(|s| s.replace(r"'\''", "'"))
            .unwrap_or_else(|| raw.to_string());
        if !unquoted.is_empty() && !unquoted.bytes().any(|b| b == 0) {
            vars.push((key.to_string(), unquoted));
        }
    }
    vars
}

#[allow(clippy::too_many_arguments)]
fn spawn_process(
    binary: &Path,
    argv: Vec<String>,
    cwd: &Path,
    path_env: &str,
    extra_env: Vec<(String, String)>,
    forwarded_env: Vec<(String, String)>,
    tty: Option<Size>,
    reader: &mut File,
    writer: &Mutex<File>,
) -> io::Result<()> {
    let safe_forwarded: Vec<(String, String)> = forwarded_env
        .into_iter()
        .filter(|(k, v)| is_safe_forwarded_env(k, v))
        .collect();
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
        cmd.cwd(cwd);
        cmd.args(argv);
        cmd.env("PATH", path_env);
        cmd.env("TERM", size.term);
        if let Some(color) = size.color_term {
            cmd.env("COLORTERM", color);
        }
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        for (key, value) in safe_forwarded {
            cmd.env(key, value);
        }
        // portable-pty's writer injects LF + VEOF from Drop. A plain
        // duplicate closes without ever synthesizing input bytes.
        let fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| io::Error::other("PTY master has no descriptor"))?;
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
            .env("PATH", path_env)
            .envs(extra_env)
            .envs(safe_forwarded);
        run_pipes(cmd, reader, writer, false)
    }
}

fn require_guest_dir(cwd: &str) -> io::Result<std::path::PathBuf> {
    let cwd = crate::sync::expand_home(cwd);
    if !cwd.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("guest project directory does not exist: {}", cwd.display()),
        ));
    }
    Ok(cwd)
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
                env,
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
            let cwd = require_guest_dir(&cwd)?;
            let mut extra_env = read_guest_environment(paths);
            extra_env.extend(crate::agents::adapter(kind).launch_env());
            spawn_process(
                &binary, argv, &cwd, &path, extra_env, env, tty, reader, writer,
            )
        }
        (
            Job::Shell {
                argv,
                cwd,
                tty,
                env,
            },
            Lane::Exec,
        ) => {
            let path = paths.child_path();
            let shell = crate::sessions::login_shell();
            let cwd = require_guest_dir(&cwd)?;
            let mut extra_env = read_guest_environment(paths);
            extra_env.push(("SHELL".into(), shell.display().to_string()));
            let args = if argv.is_empty() {
                vec!["-l".into()]
            } else {
                argv
            };
            spawn_process(
                &shell, args, &cwd, &path, extra_env, env, tty, reader, writer,
            )
        }
        (
            Job::Exec {
                program,
                argv,
                cwd,
                tty,
                env,
            },
            Lane::Exec,
        ) => {
            if program.is_empty() || program.bytes().any(|b| b == 0) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "command name must not be empty",
                ));
            }
            let path = paths.child_path();
            let cwd = require_guest_dir(&cwd)?;
            let extra_env = read_guest_environment(paths);
            spawn_process(
                Path::new(&program),
                argv,
                &cwd,
                &path,
                extra_env,
                env,
                tty,
                reader,
                writer,
            )
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
    unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .map(File::from)
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
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        kill_job(Some(child.id()), false);
        let _ = child.wait();
        return Err(io::Error::other("native command did not provide its pipes"));
    };
    pump(
        reader,
        writer,
        stdin,
        stdout,
        Some(stderr),
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
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
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

fn kill_job(pid: Option<u32>, graceful: bool) {
    if let Some(pid) = pid {
        let pgid = -(pid as libc::pid_t);
        if graceful {
            // Give interactive TUIs and shells a brief window to flush state
            // before forcing the lane clear.
            unsafe {
                libc::kill(pgid, libc::SIGHUP);
                libc::kill(pgid, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                if unsafe { libc::kill(pgid, 0) } != 0 {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Both kinds of child have their own process group.
        unsafe { libc::kill(pgid, libc::SIGKILL) };
    }
}

fn observe_job_exit(pid: u32, tty: bool) -> io::Result<u8> {
    // Keep the leader's PID reserved until the input worker finishes any
    // process-group cancellation it has started.
    let mut info = std::mem::MaybeUninit::uninit();
    loop {
        if unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        } == 0
        {
            // SAFETY: waitid returned a child status with WEXITED and no WNOHANG.
            let info = unsafe { info.assume_init() };
            let status = unsafe { info.si_status() };
            return Ok(if info.si_code == libc::CLD_EXITED {
                status as u8
            } else if tty {
                1 // Keep portable-pty's status for a guest-side signal.
            } else {
                (128 + status) as u8
            });
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
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
            let mut group = pid;
            loop {
                match read_frame(reader, &mut body) {
                    Ok(INPUT) => {
                        if let Some(out) = &mut input
                            && out.write_all(&body).is_err()
                        {
                            input = None;
                        }
                    }
                    Ok(EOF) if !tty => {
                        input = None;
                        if watch && !finished.load(Ordering::Acquire) {
                            kill_job(group.take(), false);
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
                    Ok(CANCEL) if body.is_empty() => {
                        kill_job(group.take(), tty);
                        return;
                    }
                    Ok(_) => break,
                    Err(_) => break,
                }
            }
            if !finished.load(Ordering::Acquire) {
                kill_job(group.take(), tty);
            }
        });
        let ready = send(writer, READY, &[]);
        let out =
            scope.spawn(|| output(stdout, writer, OUT, if tty { 8192 } else { MAX }, finished));
        let err = stderr.map(|s| scope.spawn(move || output(s, writer, ERR, 8192, finished)));
        let (status, reap) = if let Some(pid) = pid {
            (observe_job_exit(pid, tty), Some(wait))
        } else {
            (wait(), None)
        };
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
        // A CANCEL can still arrive after EXIT. The reader owns process-group
        // signaling, so wait for it to finish before reaping the group leader.
        let inbound = inbound.join();
        let reaped = reap.map(|wait| wait()).transpose();
        sent?;
        inbound.map_err(|_| io::Error::other("native input worker stopped"))?;
        reaped?;
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
        host.set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
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
        result
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
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
        assert!(
            begun.elapsed() < Duration::from_secs(8),
            "{:?}",
            begun.elapsed()
        );
    }

    #[test]
    fn a_background_child_that_keeps_writing_does_not_delay_the_exit() {
        let begun = Instant::now();
        // Ends on its own when the read end closes.
        let (mut host, _result) = job("yes & exit 3");
        assert_eq!(until_exit(&mut host).1, 3);
        assert!(
            begun.elapsed() < Duration::from_secs(12),
            "{:?}",
            begun.elapsed()
        );
    }

    #[test]
    fn a_vanished_host_ends_the_job() {
        let (mut host, result) = job("printf up; sleep 30");
        started(&mut host, b"up");
        drop(host);
        // Reporting the exit fails; what matters is that the lane is free again.
        assert!(
            result
                .recv_timeout(Duration::from_secs(8))
                .unwrap()
                .is_err()
        );
    }

    #[test]
    fn an_oversized_frame_ends_the_job() {
        let (mut host, _result) = job("printf up; sleep 30");
        started(&mut host, b"up");
        host.write_all(&[INPUT, 0xff, 0xff, 0xff, 0xff]).unwrap();
        assert_eq!(until_exit(&mut host).1, 128 + libc::SIGKILL as u8);
    }

    #[test]
    fn cancellation_acknowledges_after_interactive_group_cleanup() {
        let (mut host, worker) = interactive_job(
            r#"
                trap 'exit 0' HUP TERM
                sh -c 'trap "" HUP; trap "sleep 0.05; printf flushed; exit 0" TERM; printf ready; while :; do :; done' &
                wait
            "#,
            None,
        );
        started(&mut host, b"ready");
        frame(&mut host, CANCEL, &[]).unwrap();
        assert_eq!(until_exit(&mut host).0, b"flushed");
        assert_eq!(read_frame(&mut host, &mut vec![]).unwrap(), CLOSED);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn cancellation_after_exit_still_waits_for_interactive_group_cleanup() {
        let dir = tempfile::tempdir().unwrap();
        let (mut host, worker) = interactive_job(
            r#"
                sh -c 'trap "" HUP; trap "printf flushed > flushed; exit 0" TERM; touch ready; while :; do :; done' >/dev/null &
                while [ ! -e ready ]; do sleep 0.01; done
                exit 0
            "#,
            Some(dir.path().to_path_buf()),
        );
        assert_eq!(until_exit(&mut host).1, 0);
        frame(&mut host, CANCEL, &[]).unwrap();
        assert_eq!(read_frame(&mut host, &mut vec![]).unwrap(), CLOSED);
        assert_eq!(
            std::fs::read(dir.path().join("flushed")).unwrap(),
            b"flushed"
        );
        worker.join().unwrap().unwrap();
    }

    fn interactive_job(
        script: &'static str,
        cwd: Option<std::path::PathBuf>,
    ) -> (UnixStream, std::thread::JoinHandle<io::Result<()>>) {
        let (host, guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = File::from(OwnedFd::from(guest));
            let writer = Mutex::new(reader.try_clone().unwrap());
            let mut shell = Command::new("sh");
            if let Some(cwd) = cwd {
                shell.current_dir(cwd);
            }
            let mut child = shell
                .args(["-c", script])
                .process_group(0)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            pump(
                &mut reader,
                &writer,
                io::sink(),
                child.stdout.take().unwrap(),
                None::<File>,
                Some(child.id()),
                false,
                |_, _, _, _| {},
                || child.wait().map(|status| status.code().unwrap_or(1) as u8),
            )
        });
        (host, worker)
    }

    #[test]
    fn a_resize_during_the_exit_is_harmless() {
        let (mut host, guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let (seen, sizes) = mpsc::channel();
        let (exit, exited) = mpsc::channel();
        let (_keep, output) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut reader = File::from(OwnedFd::from(guest));
            let writer = Mutex::new(reader.try_clone().unwrap());
            pump(
                &mut reader,
                &writer,
                io::sink(),
                output,
                None::<File>,
                None,
                false,
                move |cols, rows, width, height| {
                    let _ = seen.send((cols, rows, width, height));
                },
                move || {
                    let _ = exited.recv();
                    Ok(0)
                },
            )
        });
        frame(&mut host, RESIZE, &[0, 80, 0, 24]).unwrap();
        assert_eq!(
            sizes.recv_timeout(Duration::from_secs(5)).unwrap(),
            (80, 24, 0, 0)
        );
        exit.send(()).unwrap();
        frame(&mut host, RESIZE, &[0, 100, 0, 30, 3, 32, 1, 224]).unwrap();
        frame(&mut host, RESIZE, &[1, 2, 3]).unwrap(); // Malformed: ends the input.
        assert_eq!(until_exit(&mut host).1, 0);
        assert_eq!(
            sizes.recv_timeout(Duration::from_secs(5)).unwrap(),
            (100, 30, 800, 480)
        );
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn idle_backoff_ramps_down_after_quiet_periods() {
        assert_eq!(
            idle_backoff(Duration::from_millis(0)),
            Duration::from_millis(2)
        );
        assert_eq!(
            idle_backoff(Duration::from_secs(2)),
            Duration::from_millis(2)
        );
        assert_eq!(
            idle_backoff(Duration::from_secs(5)),
            Duration::from_millis(15)
        );
        assert_eq!(
            idle_backoff(Duration::from_secs(15)),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn parses_lang_and_tz_from_environment_script() {
        let script = "# Generated by cabin.\nexport LANG='en_US.UTF-8'\nexport TZ='America/New_York'\nexport PATH='/evil'\nunset HTTP_PROXY\n";
        assert_eq!(
            parse_environment_script(script),
            vec![
                ("LANG".to_string(), "en_US.UTF-8".to_string()),
                ("TZ".to_string(), "America/New_York".to_string()),
            ]
        );
    }

    #[test]
    fn exec_job_runs_command_and_filters_unsafe_env() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path().join("data"), dir.path().join("home"));
        let (mut host, guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let worker = std::thread::spawn(move || {
            let mut reader = File::from(OwnedFd::from(guest));
            let writer = Mutex::new(reader.try_clone().unwrap());
            run(
                Job::Exec {
                    program: "sh".into(),
                    argv: vec![
                        "-c".into(),
                        "printf '%s|%s' \"$TERM_PROGRAM\" \"$LD_PRELOAD\"".into(),
                    ],
                    cwd,
                    tty: None,
                    env: vec![
                        ("TERM_PROGRAM".into(), "Ghostty".into()),
                        ("LD_PRELOAD".into(), "/tmp/evil.so".into()),
                    ],
                },
                Lane::Exec,
                &paths,
                &mut reader,
                &writer,
            )
        });
        let (out, code) = until_exit(&mut host);
        frame(&mut host, CLOSE, &[]).unwrap();
        assert_eq!(read_frame(&mut host, &mut vec![]).unwrap(), CLOSED);
        worker.join().unwrap().unwrap();
        assert_eq!(code, 0);
        assert_eq!(out, b"Ghostty|");
    }

    #[test]
    fn shell_job_executes_with_explicit_args() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(dir.path().join("data"), dir.path().join("home"));
        let (mut host, guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let cwd = dir.path().to_string_lossy().into_owned();
        let worker = std::thread::spawn(move || {
            let mut reader = File::from(OwnedFd::from(guest));
            let writer = Mutex::new(reader.try_clone().unwrap());
            run(
                Job::Shell {
                    argv: vec!["-c".into(), "pwd".into()],
                    cwd,
                    tty: None,
                    env: vec![],
                },
                Lane::Exec,
                &paths,
                &mut reader,
                &writer,
            )
        });
        let (out, code) = until_exit(&mut host);
        frame(&mut host, CLOSE, &[]).unwrap();
        assert_eq!(read_frame(&mut host, &mut vec![]).unwrap(), CLOSED);
        worker.join().unwrap().unwrap();
        assert_eq!(code, 0);
        let actual = String::from_utf8(out).unwrap();
        assert_eq!(
            Path::new(actual.trim()).canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }
}
