//! Native host CLI and the desktop broker that owns project sync.

use crate::sync::SyncManager;
use agent_core::model::{SyncProjectInput, SyncState};
use agent_core::native_channel::*;
use agent_core::sync::{SyncMode, SyncRules};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicI32, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize)]
enum BrokerRequest {
    Bind {
        host: PathBuf,
        guest: Option<String>,
    },
    Unbind {
        host: PathBuf,
    },
    Begin {
        host: PathBuf,
    },
}

const PROPOSE: u8 = 32;

#[derive(Serialize, Deserialize)]
struct Proposal {
    host: PathBuf,
    guest: String,
}

#[cfg(target_os = "macos")]
pub(crate) fn maintain_launcher() -> io::Result<()> {
    use std::os::unix::fs::symlink;
    let dir = private_dir(&broker_dir())?;
    let exe = std::env::current_exe()?.canonicalize()?;
    let target = dir.join("desktop-cli");
    // Replace only our private, managed symlink. The public link stays stable
    // across desktop rebuilds, app updates, or moving the app bundle.
    match target.symlink_metadata() {
        Ok(meta) if !meta.file_type().is_symlink() => {
            return Err(io::Error::other(
                "Cabin's managed launcher path is occupied",
            ));
        }
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let temp = dir.join(format!("launcher-{}", std::process::id()));
    symlink(exe, &temp)?;
    std::fs::rename(temp, &target)?;
    let bin = dirs::home_dir()
        .ok_or_else(|| io::Error::other("No home directory for the Cabin launcher"))?
        .join(".local/bin");
    std::fs::create_dir_all(&bin)?;
    let command = bin.join("cabin");
    match symlink(&target, &command) {
        Ok(()) => Ok(()),
        Err(e)
            if e.kind() == io::ErrorKind::AlreadyExists
                && std::fs::read_link(&command).is_ok_and(|link| link == target) =>
        {
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
            e.kind(),
            format!(
                "{} already exists; Cabin did not replace it",
                command.display()
            ),
        )),
        Err(e) => Err(e),
    }
}

fn production_dir() -> PathBuf {
    vm_core::paths::StatePaths::new(crate::data_dir().join("vm"))
        .instance(vm_core::machine::NAME)
        .cli_dir()
}

fn broker_dir() -> PathBuf {
    crate::data_dir().join("cli")
}

pub fn run(args: Vec<String>) -> Result<u8, String> {
    let Some((command, argv)) = args.split_first() else {
        return Ok(help());
    };
    if matches!(command.as_str(), "--help" | "-h") {
        return Ok(help());
    }
    if command == "__sync" {
        return sync_worker(argv);
    }
    let host = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())?;
    if command == "bind" {
        let guest = match argv {
            [] => None,
            [guest] => Some(guest.clone()),
            _ => return Err("usage: cabin bind [absolute guest directory]".into()),
        };
        let proposal = proposed_project(&host);
        if std::env::var_os("CABIN_CLI_CHANNEL_DIR").is_some() {
            let dir = channel_dir().map_err(|e| e.to_string())?;
            let _owner = exclusive(&dir.join("project.lock")).map_err(|e| e.to_string())?;
            let dest = guest.clone().unwrap_or(proposal.guest);
            bind(
                &SyncManager::open(dir.join("projects")),
                if guest.is_none() { proposal.host } else { host },
                dest.clone(),
                guest.is_none(),
            )?;
            eprintln!("Cabin: Two-way sync configured → {dest}");
        } else {
            let (_, body) = broker_request(&BrokerRequest::Bind { host, guest }, READY)?;
            eprintln!(
                "Cabin: Two-way sync configured → {}",
                String::from_utf8_lossy(&body)
            );
        }
        return Ok(0);
    }
    if command == "unbind" {
        if !argv.is_empty() {
            return Err("usage: cabin unbind (from the project root)".into());
        }
        if std::env::var_os("CABIN_CLI_CHANNEL_DIR").is_some() {
            let dir = channel_dir().map_err(|e| e.to_string())?;
            let _owner = exclusive(&dir.join("project.lock")).map_err(|e| e.to_string())?;
            unbind(&SyncManager::open(dir.join("projects")), &host)?;
        } else {
            broker_request(&BrokerRequest::Unbind { host }, READY)?;
        }
        return Ok(0);
    }
    if !matches!(
        command.as_str(),
        "claude" | "codex" | "agy" | "opencode" | "pi"
    ) {
        return Err(format!("unknown command: {command}"));
    }
    let exec = |dir: &Path, cwd: String| {
        let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
        let tty = interactive.then(|| {
            let [cols, rows, pixel_width, pixel_height] = winsize();
            Size {
                cols,
                rows,
                term: std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
                pixel_width,
                pixel_height,
                color_term: std::env::var("COLORTERM").ok(),
            }
        });
        stream(
            dir,
            "exec",
            Job::Agent {
                name: command.clone(),
                argv: argv.to_vec(),
                cwd,
                tty,
            },
            interactive,
        )
        .map_err(|e| e.to_string())
    };
    if std::env::var_os("CABIN_CLI_CHANNEL_DIR").is_some() {
        let dir = channel_dir().map_err(|e| e.to_string())?;
        let _owner = exclusive(&dir.join("project.lock")).map_err(|e| e.to_string())?;
        let manager = SyncManager::open(dir.join("projects"));
        let (id, cwd) = mapped_project(&manager, host)?;
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        manager.connect_native(&id, executable, dir.clone(), false)?;
        let initial = await_initial(&manager, &id);
        let reconcile = initial.is_ok();
        let exit = initial.and_then(|()| exec(&dir, cwd));
        return finish_command(exit, finish(&manager, &id, reconcile));
    }
    let (mut broker, mut cwd) =
        broker_request(&BrokerRequest::Begin { host: host.clone() }, READY)?;
    if let Some(proposal) = decode_proposal(&cwd) {
        if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
            return Err("No Two-way project here. Run 'cabin bind' from the project root first; no files were copied.".into());
        }
        eprintln!(
            "Cabin will sync files in both directions (Two-way):\n  {}\n  ↔ VM: {}",
            proposal.host.display(),
            proposal.guest
        );
        eprint!("Enable sync and start {command}? [y/N] ");
        io::stderr().flush().map_err(|e| e.to_string())?;
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .map_err(|e| e.to_string())?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return Err("Cancelled; no files were copied".into());
        }
        broker_request(
            &BrokerRequest::Bind {
                host: proposal.host,
                guest: None,
            },
            READY,
        )?;
        (broker, cwd) = broker_request(&BrokerRequest::Begin { host }, READY)?;
    }
    let cwd = String::from_utf8(cwd).map_err(|e| e.to_string())?;
    let exit = channel_dir()
        .map_err(|e| e.to_string())
        .and_then(|dir| exec(&dir, cwd));
    let finish = frame(&mut broker, CLOSE, &[])
        .map_err(|e| e.to_string())
        .and_then(|()| broker_reply(&mut broker, CLOSED).map(|_| ()));
    finish_command(exit, finish)
}

pub fn sync_worker(args: &[String]) -> Result<u8, String> {
    let [operation, root] = args else {
        return Err("invalid sync request".into());
    };
    let dir = channel_dir().map_err(|error| error.to_string())?;
    let lane = if operation == "watch" {
        "watch"
    } else {
        "sync"
    };
    stream(
        &dir,
        lane,
        Job::Sync {
            operation: operation.clone(),
            root: root.clone(),
        },
        false,
    )
    .map_err(|e| e.to_string())
}

fn bind(
    manager: &Arc<SyncManager>,
    host: PathBuf,
    guest: String,
    managed: bool,
) -> Result<(), String> {
    let path = Path::new(
        guest
            .strip_prefix("~/")
            .filter(|_| managed)
            .unwrap_or(&guest),
    );
    if (!path.is_absolute() && !managed)
        || path.parent().is_none()
        || path.components().any(|p| p == Component::ParentDir)
    {
        return Err("choose an absolute guest project directory, not /".into());
    }
    if let Ok(data) = crate::data_dir().canonicalize()
        && (data.starts_with(&host) || host.starts_with(&data))
    {
        return Err(
            "Choose a project outside Cabin's data directory; do not bind a parent of it".into(),
        );
    }
    manager.create_cli_binding(SyncProjectInput {
        workspace_id: None,
        name: host
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        host_path: host.to_str().ok_or("project path is not UTF-8")?.to_owned(),
        guest_path: guest,
        mode: SyncMode::TwoWay,
        rules: SyncRules::default(),
    })?;
    Ok(())
}

fn proposed_project(host: &Path) -> Proposal {
    let root = host
        .ancestors()
        .find(|dir| dir.join(".git").symlink_metadata().is_ok())
        .unwrap_or(host);
    let mut name: String = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    if name.is_empty() {
        name = "project".into();
    }
    // Stable suffix separates same-named checkouts and doesn't depend on what
    // other projects are configured when the same directory gets rebound.
    let hash = root
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        });
    Proposal {
        host: root.to_path_buf(),
        guest: format!("~/projects/{name}-{hash:016x}"),
    }
}

fn decode_proposal(body: &[u8]) -> Option<Proposal> {
    body.strip_prefix(&[PROPOSE])
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
}

fn unbind(manager: &SyncManager, host: &Path) -> Result<(), String> {
    let root = host.canonicalize().map_err(|e| e.to_string())?;
    let id = manager
        .list()
        .into_iter()
        .find(|v| root == Path::new(&v.project.host_path))
        .ok_or("run cabin unbind from the root of a bound project")?
        .project
        .id;
    manager.remove(&id)
}

const NO_PROJECT: &str = "No Two-way project here";

fn mapped_project(manager: &SyncManager, host: PathBuf) -> Result<(String, String), String> {
    let host = host.canonicalize().map_err(|e| e.to_string())?;
    let project = manager
        .list()
        .into_iter()
        .map(|v| v.project)
        .find(|project| host.starts_with(&project.host_path))
        .ok_or(NO_PROJECT)?;
    let cwd = Path::new(&project.guest_path).join(host.strip_prefix(&project.host_path).unwrap());
    let cwd = cwd
        .to_str()
        .ok_or("guest working directory is not UTF-8")?
        .to_owned();
    Ok((project.id, cwd))
}

fn finish(manager: &Arc<SyncManager>, id: &str, reconcile: bool) -> Result<(), String> {
    let status = manager.finish_native(id, reconcile)?;
    if status.conflicts.is_empty() {
        Ok(())
    } else {
        Err("sync left conflicts; both versions were kept".into())
    }
}

fn finish_command(exit: Result<u8, String>, finish: Result<(), String>) -> Result<u8, String> {
    match (exit, finish) {
        (Ok(code), Ok(())) => Ok(code),
        (Ok(code), Err(error)) => {
            eprintln!("cabin: final sync: {error}");
            Ok(if code == 0 { 1 } else { code })
        }
        (Err(error), _) => Err(error),
    }
}

fn help() -> u8 {
    println!(
        "Usage: cabin bind [guest-directory]\n       cabin unbind\n       cabin <claude|codex|agy|opencode|pi> [agent arguments...]\n\nStart Cabin desktop and its VM first. Run from a project with Two-way sync. The first interactive run can set it up."
    );
    0
}

fn broker_request(request: &BrokerRequest, expected: u8) -> Result<(UnixStream, Vec<u8>), String> {
    let dir = private_dir(&broker_dir()).map_err(|_| "Open Cabin desktop before using the CLI")?;
    let mut socket = UnixStream::connect(dir.join("host.sock"))
        .map_err(|e| format!("Connect to Cabin desktop: {e}"))?;
    socket
        .set_read_timeout(Some(Duration::from_secs(150)))
        .map_err(|e| e.to_string())?;
    let message = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    frame(&mut socket, START, &message).map_err(|e| e.to_string())?;
    let result = broker_reply(&mut socket, expected)?;
    Ok((socket, result))
}

fn broker_reply(socket: &mut UnixStream, expected: u8) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    match read_frame(socket, &mut body).map_err(|e| e.to_string())? {
        kind if kind == expected => Ok(body),
        PROPOSE if expected == READY => {
            body.insert(0, PROPOSE);
            Ok(body)
        }
        FAILURE => Err(String::from_utf8_lossy(&body).into_owned()),
        _ => Err("invalid reply from Cabin desktop".into()),
    }
}

pub(crate) fn start_broker(manager: Arc<SyncManager>) -> io::Result<()> {
    let dir = broker_dir();
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let path = private_dir(&dir)?.join("host.sock");
    match path.symlink_metadata() {
        Ok(meta) if meta.file_type().is_socket() => match UnixStream::connect(&path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "Cabin CLI broker is already running",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => std::fs::remove_file(&path)?,
            Err(e) => return Err(e),
        },
        Ok(_) => return Err(io::Error::other("Cabin CLI broker path is not a socket")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(path)?;
    let exe = std::env::current_exe()?;
    std::thread::Builder::new()
        .name("cabin-cli-broker".into())
        .spawn(move || {
            for connection in listener.incoming() {
                let Ok(mut socket) = connection else { break };
                // One native session at a time. Separate the accept loop so a second
                // request receives an immediate busy response rather than hanging.
                let manager = manager.clone();
                let exe = exe.clone();
                let _ = std::thread::Builder::new()
                    .name("cabin-cli-client".into())
                    .spawn(move || {
                        if let Err(error) = serve_broker(&manager, exe, &mut socket) {
                            let _ = frame(&mut socket, FAILURE, error.as_bytes());
                        }
                    });
            }
        })?;
    Ok(())
}

fn serve_broker(
    manager: &Arc<SyncManager>,
    executable: PathBuf,
    socket: &mut UnixStream,
) -> Result<(), String> {
    let socket_error = |e: io::Error| e.to_string();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(socket_error)?;
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(socket_error)?;
    let mut body = Vec::new();
    if read_frame(socket, &mut body).map_err(socket_error)? != START {
        return Err("invalid Cabin CLI request".into());
    }
    match serde_json::from_slice(&body).map_err(|e| e.to_string())? {
        BrokerRequest::Bind { host, guest } => {
            let proposal = proposed_project(&host);
            let managed = guest.is_none();
            let dest = guest.unwrap_or(proposal.guest);
            bind(
                manager,
                if managed { proposal.host } else { host },
                dest.clone(),
                managed,
            )?;
            frame(socket, READY, dest.as_bytes()).map_err(socket_error)
        }
        BrokerRequest::Unbind { host } => {
            unbind(manager, &host)?;
            frame(socket, READY, &[]).map_err(socket_error)
        }
        BrokerRequest::Begin { host } => {
            let dir = private_dir(&production_dir())
                .map_err(|_| "Start or restart the VM from the current Cabin desktop app")?;
            if !dir
                .join("exec.sock")
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_socket())
            {
                return Err("Start or restart the VM to enable the native CLI channels".into());
            }
            let (id, cwd) = match mapped_project(manager, host.clone()) {
                Ok(mapped) => mapped,
                Err(error) if error == NO_PROJECT => {
                    let proposal =
                        serde_json::to_vec(&proposed_project(&host)).map_err(|e| e.to_string())?;
                    return frame(socket, PROPOSE, &proposal).map_err(socket_error);
                }
                Err(error) => return Err(error),
            };
            manager.connect_native(&id, executable, dir, true)?;
            if let Err(error) = await_initial(manager, &id) {
                let _ = finish(manager, &id, false);
                return Err(error);
            }
            let result = (|| {
                frame(socket, READY, cwd.as_bytes())?;
                socket.set_read_timeout(None)?;
                let closed = read_frame(socket, &mut body)?;
                if closed != CLOSE {
                    return Err(io::Error::other("invalid Cabin CLI close"));
                }
                Ok(())
            })();
            let finished = finish(manager, &id, true);
            result.map_err(socket_error)?;
            finished?;
            frame(socket, CLOSED, &[]).map_err(socket_error)
        }
    }
}

fn await_initial(manager: &SyncManager, id: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let status = manager
            .list()
            .into_iter()
            .find(|v| v.project.id == id)
            .ok_or("project removed")?
            .status;
        match status.state {
            SyncState::Watching if status.conflicts.is_empty() => return Ok(()),
            SyncState::Watching => {
                return Err("initial sync has conflicts; agent was not started".into());
            }
            SyncState::Error => {
                return Err(status.error.unwrap_or_else(|| "initial sync failed".into()));
            }
            _ if Instant::now() >= deadline => {
                return Err("initial sync did not complete in time".into());
            }
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn channel_dir() -> io::Result<PathBuf> {
    let path = std::env::var_os("CABIN_CLI_CHANNEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(production_dir);
    private_dir(&path)
        .map_err(|e| io::Error::other(format!("Native channel directory {}: {e}", path.display())))
}

fn private_dir(dir: &Path) -> io::Result<PathBuf> {
    let meta = dir.symlink_metadata()?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "channel directory must belong to you and have mode 0700",
        ));
    }
    dir.canonicalize()
}

fn exclusive(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.try_lock().map_err(io::Error::from)?;
    Ok(file)
}

fn winsize() -> [u16; 4] {
    let mut size = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    [
        if size.ws_col == 0 { 80 } else { size.ws_col },
        if size.ws_row == 0 { 24 } else { size.ws_row },
        size.ws_xpixel,
        size.ws_ypixel,
    ]
}

struct Raw(libc::termios);
impl Raw {
    fn enter() -> io::Result<Self> {
        let mut saved = std::mem::MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(0, saved.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        let saved = unsafe { saved.assume_init() };
        let mut raw = saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(saved))
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        // Discard unread TUI keystrokes on close instead of reinterpreting
        // them as commands in the caller's canonical shell. On Darwin this
        // also avoids PENDIN when switching the line discipline back.
        loop {
            let result = unsafe { libc::tcsetattr(0, libc::TCSAFLUSH, &self.0) };
            if result == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                break;
            }
        }
    }
}

static TERMINAL_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn terminal_signal(signal: libc::c_int) {
    TERMINAL_SIGNAL.store(signal, Ordering::Relaxed);
}

struct SignalGuard(Vec<(libc::c_int, libc::sigaction)>);
impl SignalGuard {
    fn install() -> io::Result<Self> {
        TERMINAL_SIGNAL.store(0, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut previous = std::mem::MaybeUninit::uninit();
            action.sa_sigaction = terminal_signal as *const () as usize;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(signal, &action, previous.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            guard.0.push((signal, unsafe { previous.assume_init() }));
        }
        Ok(guard)
    }
}
impl Drop for SignalGuard {
    fn drop(&mut self) {
        for (signal, previous) in &self.0 {
            unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
        }
    }
}

fn stream(dir: &Path, lane: &str, job: Job, tty: bool) -> io::Result<u8> {
    let _lock = exclusive(&dir.join(format!("{lane}.lock")))?;
    let path = dir.join(format!("{lane}.sock"));
    if !path.symlink_metadata()?.file_type().is_socket() {
        return Err(io::Error::other("native channel is not a socket"));
    }
    let mut sock = UnixStream::connect(path)?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))?;
    sock.set_write_timeout(Some(Duration::from_secs(5)))?;
    let initial_size = match &job {
        Job::Agent { tty: Some(size), .. } => [size.cols, size.rows, size.pixel_width, size.pixel_height],
        _ => winsize(),
    };
    let request = Request {
        version: VERSION,
        job,
    };
    frame(&mut sock, START, &serde_json::to_vec(&request)?)?;
    let mut body = vec![];
    match read_frame(&mut sock, &mut body)? {
        READY => {}
        FAILURE => {
            return Err(io::Error::other(
                String::from_utf8_lossy(&body).into_owned(),
            ));
        }
        _ => return Err(io::Error::other("invalid response to native start")),
    }
    sock.set_read_timeout(None)?;
    let _signals = tty.then(SignalGuard::install).transpose()?;
    let _raw = tty.then(Raw::enter).transpose()?;
    let stopped = Arc::new(AtomicBool::new(false));
    let writer = Arc::new(Mutex::new(sock.try_clone()?));
    let tx = Arc::clone(&writer);
    let done = Arc::clone(&stopped);
    let mut last = initial_size;
    let interrupt = sock.try_clone()?;
    let input_worker = std::thread::spawn(move || {
        let mut buf = [0; MAX];
        while !done.load(Ordering::Relaxed) {
            let mut fd = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut fd, 1, if tty { 30 } else { 100 }) };
            if tty {
                if TERMINAL_SIGNAL.load(Ordering::Relaxed) != 0 {
                    let _ = interrupt.shutdown(Shutdown::Both);
                    break;
                }
                let size = winsize();
                if size != last {
                    let mut dimensions = [0; 8];
                    for (bytes, value) in dimensions.chunks_exact_mut(2).zip(size) {
                        bytes.copy_from_slice(&value.to_be_bytes());
                    }
                    if frame(&mut *tx.lock().unwrap(), RESIZE, &dimensions).is_err() {
                        break;
                    }
                    last = size;
                }
            }
            if n <= 0 {
                continue;
            }
            let capacity = if tty { 8192 } else { MAX };
            // Use the descriptor polled above, not stdin's line buffer.
            let read = unsafe { libc::read(0, buf.as_mut_ptr().cast(), capacity) };
            let count = if read < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = interrupt.shutdown(Shutdown::Both);
                break;
            } else {
                read as usize
            };
            let mut tx = tx.lock().unwrap();
            if done.load(Ordering::Relaxed) {
                break;
            }
            if count == 0 {
                if tty {
                    let _ = interrupt.shutdown(Shutdown::Both);
                } else {
                    let _ = frame(&mut *tx, EOF, &[]);
                }
                break;
            }
            if frame(&mut *tx, INPUT, &buf[..count]).is_err() {
                let _ = interrupt.shutdown(Shutdown::Both);
                break;
            }
        }
    });
    let result = (|| {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        loop {
            match read_frame(&mut sock, &mut body)? {
                OUT => {
                    stdout.write_all(&body)?;
                    stdout.flush()?;
                }
                ERR => {
                    stderr.write_all(&body)?;
                    stderr.flush()?;
                }
                EXIT if body.len() == 1 => return Ok(body[0]),
                FAILURE => {
                    return Err(io::Error::other(
                        String::from_utf8_lossy(&body).into_owned(),
                    ));
                }
                _ => return Err(io::Error::other("invalid native channel response")),
            }
        }
    })();
    stopped.store(true, Ordering::Relaxed);
    let result = result.and_then(|code| {
        frame(&mut *writer.lock().unwrap(), CLOSE, &[])?;
        sock.set_read_timeout(Some(Duration::from_secs(5)))?;
        if read_frame(&mut sock, &mut body)? != CLOSED {
            return Err(io::Error::other("native channel did not acknowledge close"));
        }
        Ok(code)
    });
    let _ = sock.shutdown(Shutdown::Both);
    let _ = input_worker.join();
    let signal = if tty {
        TERMINAL_SIGNAL.load(Ordering::Relaxed)
    } else {
        0
    };
    if signal != 0 {
        Ok((128 + signal) as u8)
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_mapping_uses_repo_root_and_distinguishes_same_names() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let repo = first.path().join("my repo");
        let sub = repo.join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(repo.join(".git"), "gitdir: /some/worktree").unwrap();
        let at_root = proposed_project(&repo);
        let at_sub = proposed_project(&sub);
        assert_eq!(at_sub.host, repo);
        assert_eq!(at_sub.guest, at_root.guest);
        assert!(at_sub.guest.starts_with("~/projects/my-repo-"));
        assert_ne!(
            at_sub.guest,
            proposed_project(&second.path().join("my repo")).guest
        );
    }
}
