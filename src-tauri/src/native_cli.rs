//! Native host CLI and the desktop broker that owns project sync.

use crate::host::Host;
use crate::host_cli::{HostCliState, cli_state_dir, create_private_dir};
use crate::sync::SyncManager;
use agent_core::model::{SyncProjectInput, SyncState};
use agent_core::native_channel::*;
use agent_core::sync::{SyncMode, SyncRules};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicI32, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
enum BeginKind {
    #[default]
    Agent,
    Shell,
}

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
        #[serde(default)]
        kind: BeginKind,
    },
    Status {
        host: PathBuf,
    },
    List,
}

const PROPOSE: u8 = 32;
const PROGRESS: u8 = 33;
const BROKER_REPLY_TIMEOUT: Duration = Duration::from_secs(150);
const FINAL_SYNC_HEARTBEAT: Duration = Duration::from_secs(15);
const GUEST_CANCEL_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Serialize, Deserialize)]
struct Proposal {
    host: PathBuf,
    guest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct BeginReady {
    cwd: String,
    #[serde(default)]
    forward_terminal_env: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CliProjectInfo {
    name: String,
    host_path: String,
    guest_path: String,
    mode: String,
    paused: bool,
    state: String,
    conflicts: usize,
    active: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CliStatusReply {
    enabled: bool,
    allow_shell: bool,
    channels_ready: bool,
    active_project: Option<String>,
    current: Option<CliProjectInfo>,
    proposed_host: String,
    proposed_guest: String,
}

fn production_dir() -> PathBuf {
    vm_core::paths::StatePaths::new(crate::data_dir().join("vm"))
        .instance(vm_core::machine::NAME)
        .cli_dir()
}

/// Bind a host folder from the desktop Settings UI using the same rules as `cabin bind`.
pub(crate) fn bind_from_desktop(
    manager: &Arc<SyncManager>,
    host: PathBuf,
    guest: Option<String>,
) -> Result<String, String> {
    let proposal = proposed_project(&host);
    let managed = guest.is_none();
    let dest = guest.unwrap_or(proposal.guest);
    bind(
        manager,
        if managed { proposal.host } else { host },
        dest.clone(),
        managed,
    )?;
    Ok(dest)
}

pub fn run(args: Vec<String>) -> Result<u8, String> {
    let Some((command, argv)) = args.split_first() else {
        return Ok(help());
    };
    if matches!(command.as_str(), "--help" | "-h" | "help") {
        return Ok(help());
    }
    if matches!(command.as_str(), "--version" | "-V" | "version") {
        println!("cabin {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if command == "__sync" {
        return sync_worker(argv);
    }
    let host = std::env::current_dir()
        .and_then(|p| p.canonicalize())
        .map_err(|e| e.to_string())?;
    if command == "status" {
        if !argv.is_empty() {
            return Err("usage: cabin status".into());
        }
        return run_status(host);
    }
    if matches!(command.as_str(), "list" | "ls") {
        if !argv.is_empty() {
            return Err("usage: cabin list".into());
        }
        return run_list();
    }
    if command == "bind" {
        let guest = match argv {
            [] => None,
            [guest] => Some(guest.clone()),
            _ => return Err("usage: cabin bind [absolute guest directory]".into()),
        };
        let proposal = proposed_project(&host);
        if std::env::var_os("CABIN_CLI_CHANNEL_DIR").is_some() {
            let dir = channel_dir().map_err(|e| e.to_string())?;
            let _owner = lock_native_project(&dir).map_err(|e| e.to_string())?;
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
            let _owner = lock_native_project(&dir).map_err(|e| e.to_string())?;
            unbind(&SyncManager::open(dir.join("projects")), &host)?;
        } else {
            broker_request(&BrokerRequest::Unbind { host }, READY)?;
        }
        return Ok(0);
    }

    enum Invocation<'a> {
        Agent(&'a str, &'a [String]),
        Shell(&'a [String]),
        Exec(&'a str, &'a [String]),
    }

    let invocation = match command.as_str() {
        "claude" | "codex" | "agy" | "opencode" | "pi" => Invocation::Agent(command, argv),
        "shell" | "sh" => Invocation::Shell(argv),
        "exec" | "run" => {
            let Some((program, rest)) = argv.split_first() else {
                return Err("usage: cabin exec <command> [arguments...]".into());
            };
            Invocation::Exec(program, rest)
        }
        _ => {
            return Err(format!(
                "unknown command: {command} (run 'cabin --help' for usage)"
            ));
        }
    };

    let begin_kind = match invocation {
        Invocation::Agent(_, _) => BeginKind::Agent,
        Invocation::Shell(_) | Invocation::Exec(_, _) => BeginKind::Shell,
    };

    let exec = |dir: &Path, cwd: String, forward_env: bool| {
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
        let env = if forward_env {
            collect_forwarded_env()
        } else {
            Vec::new()
        };
        let job = match invocation {
            Invocation::Agent(name, args) => Job::Agent {
                name: name.to_string(),
                argv: args.to_vec(),
                cwd,
                tty,
                env,
            },
            Invocation::Shell(args) => Job::Shell {
                argv: args.to_vec(),
                cwd,
                tty,
                env,
            },
            Invocation::Exec(program, args) => Job::Exec {
                program: program.to_string(),
                argv: args.to_vec(),
                cwd,
                tty,
                env,
            },
        };
        stream(dir, "exec", job, interactive)
    };

    if std::env::var_os("CABIN_CLI_CHANNEL_DIR").is_some() {
        let dir = channel_dir().map_err(|e| e.to_string())?;
        let _owner = lock_native_project(&dir).map_err(|e| e.to_string())?;
        let manager = SyncManager::open(dir.join("projects"));
        let (id, cwd) = mapped_project(&manager, host)?;
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let initial = manager.connect_native(&id, executable, dir.clone(), false)?;
        let (exit, reconcile) = match await_initial(initial, None) {
            Ok(()) => exec(&dir, cwd, true),
            Err(error) => (Err(error), false),
        };
        return finish_command(exit, manager.finish_native(&id, reconcile));
    }

    let (mut broker, mut body) = broker_request(
        &BrokerRequest::Begin {
            host: host.clone(),
            kind: begin_kind,
        },
        READY,
    )?;
    if let Some(proposal) = decode_proposal(&body) {
        if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
            return Err(
                "No Two-way project here. Run 'cabin bind' from the project root first; no files were copied."
                    .into(),
            );
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
        (broker, body) = broker_request(
            &BrokerRequest::Begin {
                host,
                kind: begin_kind,
            },
            READY,
        )?;
    }
    let ready = decode_begin_ready(&body)?;
    let (exit, reconcile) = match channel_dir() {
        Ok(dir) => exec(&dir, ready.cwd, ready.forward_terminal_env),
        Err(error) => (Err(error.to_string()), false),
    };
    let finish = frame(&mut broker, if reconcile { CLOSE } else { CANCEL }, &[])
        .map_err(|e| e.to_string())
        .and_then(|()| broker_reply(&mut broker, CLOSED).map(|_| ()));
    finish_command(exit, finish)
}

fn run_status(host: PathBuf) -> Result<u8, String> {
    let (_, body) = broker_request(&BrokerRequest::Status { host }, READY)?;
    let status: CliStatusReply = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    println!("Cabin CLI {}", env!("CARGO_PKG_VERSION"));
    println!(
        "  Passthrough: {}",
        if status.enabled {
            "enabled"
        } else {
            "disabled (enable in Cabin Settings → Host CLI)"
        }
    );
    println!(
        "  Shell exec:  {}",
        if status.allow_shell {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "  VM channels: {}",
        if !status.enabled {
            "disabled"
        } else if status.channels_ready {
            "ready (virtio-serial)"
        } else {
            "unavailable (start or restart VM in Cabin)"
        }
    );
    if let Some(active) = &status.active_project {
        println!("  Active job:  {active}");
    }
    if let Some(project) = status.current {
        println!(
            "  Project:     {} ↔ VM: {}",
            project.host_path, project.guest_path
        );
        println!(
            "  Sync:        {} · {}{}",
            project.mode,
            project.state,
            if project.conflicts > 0 {
                format!(" ({} conflict(s))", project.conflicts)
            } else {
                String::new()
            }
        );
    } else {
        println!("  Project:     not bound");
        println!(
            "  Suggested:   {} ↔ VM: {}",
            status.proposed_host, status.proposed_guest
        );
        println!("  Run 'cabin bind' or bind the folder in Cabin Settings → Host CLI.");
    }
    Ok(0)
}

fn run_list() -> Result<u8, String> {
    let (_, body) = broker_request(&BrokerRequest::List, READY)?;
    let projects: Vec<CliProjectInfo> = serde_json::from_slice(&body).map_err(|e| e.to_string())?;
    if projects.is_empty() {
        println!("No synced projects configured. Run 'cabin bind' inside a project folder.");
        return Ok(0);
    }
    for project in projects {
        let marker = if project.active { "* " } else { "  " };
        let conflict = if project.conflicts > 0 {
            format!(", {} conflict(s)", project.conflicts)
        } else {
            String::new()
        };
        println!(
            "{marker}{} ↔ VM: {}  [{}, {}{conflict}]",
            project.host_path, project.guest_path, project.mode, project.state
        );
    }
    Ok(0)
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
    .0
}

fn collect_forwarded_env() -> Vec<(String, String)> {
    ALLOWED_FORWARDED_ENV
        .iter()
        .filter_map(|&key| {
            let value = std::env::var(key).ok()?;
            is_safe_forwarded_env(key, &value).then(|| (key.to_string(), value))
        })
        .collect()
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

fn decode_begin_ready(body: &[u8]) -> Result<BeginReady, String> {
    if let Ok(ready) = serde_json::from_slice::<BeginReady>(body) {
        return Ok(ready);
    }
    let cwd = String::from_utf8(body.to_vec()).map_err(|e| e.to_string())?;
    Ok(BeginReady {
        cwd,
        forward_terminal_env: true,
    })
}

fn unbind(manager: &SyncManager, host: &Path) -> Result<(), String> {
    manager.require_registry()?;
    let root = host.canonicalize().map_err(|e| e.to_string())?;
    let list = manager.list();
    if let Some(view) = list
        .iter()
        .find(|v| root == Path::new(&v.project.host_path))
    {
        return manager.remove(&view.project.id);
    }
    if let Some(parent) = list.iter().find(|v| root.starts_with(&v.project.host_path)) {
        return Err(format!(
            "run 'cabin unbind' from the bound project root: {}",
            parent.project.host_path
        ));
    }
    Err("run cabin unbind from the root of a bound project".into())
}

const NO_PROJECT: &str = "No Two-way project here";

fn mapped_project(manager: &SyncManager, host: PathBuf) -> Result<(String, String), String> {
    manager.require_registry()?;
    let host = host.canonicalize().map_err(|e| e.to_string())?;
    let (project, relative) = manager
        .list()
        .into_iter()
        .find_map(|view| {
            let relative = host.strip_prefix(&view.project.host_path).ok()?;
            Some((view.project, relative))
        })
        .ok_or(NO_PROJECT)?;
    let cwd = Path::new(&project.guest_path).join(relative);
    let cwd = cwd
        .to_str()
        .ok_or("guest working directory is not UTF-8")?
        .to_owned();
    Ok((project.id, cwd))
}

fn finish_command(exit: Result<u8, String>, finish: Result<(), String>) -> Result<u8, String> {
    match (exit, finish) {
        (Ok(code), Ok(())) => Ok(code),
        (Ok(code), Err(error)) => {
            eprintln!("cabin: final sync: {error}");
            Ok(if code == 0 { 1 } else { code })
        }
        (Err(error), Ok(())) => Err(error),
        (Err(error), Err(cleanup)) => Err(format!("{error}\nSync cleanup also failed: {cleanup}")),
    }
}

fn help() -> u8 {
    println!(
        "Usage: cabin <claude|codex|agy|opencode|pi> [agent arguments...]\n\
         \x20      cabin shell [shell arguments...]        (alias: sh)\n\
         \x20      cabin exec <command> [arguments...]     (alias: run)\n\
         \x20      cabin bind [guest-directory]\n\
         \x20      cabin unbind\n\
         \x20      cabin status | list\n\
         \x20      cabin --version\n\n\
         Start Cabin desktop and its VM first. Configure launcher and passthrough\n\
         options in Cabin Settings → Host CLI."
    );
    0
}

fn broker_request(request: &BrokerRequest, expected: u8) -> Result<(UnixStream, Vec<u8>), String> {
    let dir =
        private_dir(&cli_state_dir()).map_err(|_| "Open Cabin desktop before using the CLI")?;
    let mut socket = UnixStream::connect(dir.join("host.sock"))
        .map_err(|e| format!("Connect to Cabin desktop: {e}"))?;
    socket
        .set_read_timeout(Some(BROKER_REPLY_TIMEOUT))
        .map_err(|e| e.to_string())?;
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let message = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    frame(&mut socket, START, &message).map_err(|e| e.to_string())?;
    let result = broker_reply(&mut socket, expected)?;
    Ok((socket, result))
}

fn broker_reply(socket: &mut UnixStream, expected: u8) -> Result<Vec<u8>, String> {
    let mut body = Vec::new();
    let show_progress = io::stderr().is_terminal();
    let mut printed_progress = false;
    let clear_progress = |printed: &mut bool| {
        if *printed {
            let _ = write!(io::stderr(), "\r\x1b[2K");
            let _ = io::stderr().flush();
            *printed = false;
        }
    };
    loop {
        match read_frame(socket, &mut body).map_err(|e| {
            clear_progress(&mut printed_progress);
            e.to_string()
        })? {
            PROGRESS => {
                if show_progress {
                    let text = String::from_utf8_lossy(&body);
                    let _ = write!(io::stderr(), "\r\x1b[2Kcabin: {text}");
                    let _ = io::stderr().flush();
                    printed_progress = true;
                }
            }
            kind if kind == expected => {
                clear_progress(&mut printed_progress);
                return Ok(body);
            }
            PROPOSE if expected == READY => {
                clear_progress(&mut printed_progress);
                body.insert(0, PROPOSE);
                return Ok(body);
            }
            FAILURE => {
                clear_progress(&mut printed_progress);
                return Err(String::from_utf8_lossy(&body).into_owned());
            }
            _ => {
                clear_progress(&mut printed_progress);
                return Err("invalid reply from Cabin desktop".into());
            }
        }
    }
}

fn bind_broker(path: &Path) -> io::Result<UnixListener> {
    match path.symlink_metadata() {
        Ok(meta) if meta.file_type().is_socket() => match UnixStream::connect(path) {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "Cabin CLI broker is already running",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => std::fs::remove_file(path)?,
            Err(e) => return Err(e),
        },
        Ok(_) => return Err(io::Error::other("Cabin CLI broker path is not a socket")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    UnixListener::bind(path)
}

struct BrokerLifetime {
    path: PathBuf,
    state: Arc<HostCliState>,
}

impl Drop for BrokerLifetime {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        self.state.set_broker_running(false);
    }
}

pub(crate) fn start_broker(host: &Arc<Host>, cli_state: Arc<HostCliState>) -> io::Result<()> {
    let _starting = cli_state.lock_broker_start();
    if cli_state.broker_running() {
        return Ok(());
    }
    let dir = cli_state_dir();
    create_private_dir(&dir)?;
    let path = private_dir(&dir)?.join("host.sock");
    let exe = std::env::current_exe()?;
    let listener = bind_broker(&path)?;
    let lifetime = BrokerLifetime {
        path,
        state: cli_state.clone(),
    };
    let host = Arc::downgrade(host);
    cli_state.set_broker_running(true);
    std::thread::Builder::new()
        .name("cabin-cli-broker".into())
        .spawn(move || {
            let lifetime = lifetime;
            for connection in listener.incoming() {
                let mut socket = match connection {
                    Ok(socket) => socket,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        cabin_diagnostics::event(
                            cabin_diagnostics::Level::Warn,
                            "cli",
                            "broker_accept_failed",
                            format_args!("{error}"),
                        );
                        break;
                    }
                };
                let Some(host) = host.upgrade() else { break };
                // One native session at a time. Separate the accept loop so a second
                // request receives an immediate busy response rather than hanging.
                let cli_state = lifetime.state.clone();
                let exe = exe.clone();
                let _ = std::thread::Builder::new()
                    .name("cabin-cli-client".into())
                    .spawn(move || {
                        if let Err(error) = serve_broker(&host, &cli_state, exe, &mut socket) {
                            let _ = frame(&mut socket, FAILURE, error.as_bytes());
                        }
                    });
            }
        })?;
    Ok(())
}

fn project_info(view: &agent_core::model::SyncView, active_id: Option<&str>) -> CliProjectInfo {
    let mode = match view.project.mode {
        SyncMode::Safe => "Safe",
        SyncMode::TwoWay => "Two-way",
        SyncMode::Mirror => "Mirror",
    };
    let state = match view.status.state {
        SyncState::Offline => "offline",
        SyncState::Scanning => "scanning",
        SyncState::Syncing => "syncing",
        SyncState::Watching => "watching",
        SyncState::Paused => "paused",
        SyncState::Error => "error",
    };
    CliProjectInfo {
        name: view.project.name.clone(),
        host_path: view.project.host_path.clone(),
        guest_path: view.project.guest_path.clone(),
        mode: mode.into(),
        paused: view.project.paused,
        state: state.into(),
        conflicts: view.status.conflicts.len(),
        active: active_id == Some(view.project.id.as_str()),
    }
}

fn serve_broker(
    host_state: &Host,
    cli_state: &HostCliState,
    executable: PathBuf,
    socket: &mut UnixStream,
) -> Result<(), String> {
    let manager = &host_state.sync;
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
    let config = cli_state.config();
    match serde_json::from_slice(&body).map_err(|e| e.to_string())? {
        BrokerRequest::Status { host } => {
            let active_id = manager.active_native_project();
            let list = manager.list();
            let canonical = host.canonicalize().unwrap_or_else(|_| host.clone());
            let current = list
                .iter()
                .find(|v| canonical.starts_with(&v.project.host_path))
                .map(|v| project_info(v, active_id.as_deref()));
            let active_project = active_id.as_deref().and_then(|id| {
                list.iter()
                    .find(|v| v.project.id == id)
                    .map(|v| v.project.host_path.clone())
            });
            let proposal = proposed_project(&canonical);
            let reply = CliStatusReply {
                enabled: config.enabled,
                allow_shell: config.allow_shell,
                channels_ready: config.enabled && host_state.ready_native_channels().is_ok(),
                active_project,
                current,
                proposed_host: proposal.host.display().to_string(),
                proposed_guest: proposal.guest,
            };
            let payload = serde_json::to_vec(&reply).map_err(|e| e.to_string())?;
            frame(socket, READY, &payload).map_err(socket_error)
        }
        BrokerRequest::List => {
            let active_id = manager.active_native_project();
            let list: Vec<CliProjectInfo> = manager
                .list()
                .iter()
                .map(|v| project_info(v, active_id.as_deref()))
                .collect();
            let payload = serde_json::to_vec(&list).map_err(|e| e.to_string())?;
            frame(socket, READY, &payload).map_err(socket_error)
        }
        BrokerRequest::Bind { host, guest } => {
            if !config.enabled {
                return Err(
                    "Host CLI passthrough is disabled. Enable it in Cabin Settings → Host CLI."
                        .into(),
                );
            }
            let dest = bind_from_desktop(manager, host, guest)?;
            frame(socket, READY, dest.as_bytes()).map_err(socket_error)
        }
        BrokerRequest::Unbind { host } => {
            if !config.enabled {
                return Err(
                    "Host CLI passthrough is disabled. Enable it in Cabin Settings → Host CLI."
                        .into(),
                );
            }
            unbind(manager, &host)?;
            frame(socket, READY, &[]).map_err(socket_error)
        }
        BrokerRequest::Begin { host, kind } => {
            if !config.enabled {
                return Err(
                    "Host CLI passthrough is disabled. Enable it in Cabin Settings → Host CLI."
                        .into(),
                );
            }
            if kind == BeginKind::Shell && !config.allow_shell {
                return Err(
                    "Shell passthrough is disabled. Enable 'Allow shell & command execution' in Cabin Settings → Host CLI."
                        .into(),
                );
            }
            let dir = channel_dir_for_broker()?;
            let _project = lock_native_project(&dir).map_err(|e| e.to_string())?;
            let dir = host_state.ready_native_channels().map_err(|e| e.message)?;
            let (id, cwd) = match mapped_project(manager, host.clone()) {
                Ok(mapped) => mapped,
                Err(error) if error == NO_PROJECT => {
                    if !config.auto_bind_prompt {
                        return Err(
                            "No Two-way project here. Run 'cabin bind' or bind this folder in Cabin Settings → Host CLI."
                                .into(),
                        );
                    }
                    let proposal =
                        serde_json::to_vec(&proposed_project(&host)).map_err(|e| e.to_string())?;
                    return frame(socket, PROPOSE, &proposal).map_err(socket_error);
                }
                Err(error) => return Err(error),
            };
            let ready = serde_json::to_vec(&BeginReady {
                cwd,
                forward_terminal_env: config.forward_terminal_env,
            })
            .map_err(|e| e.to_string())?;
            if peer_disconnected(socket) {
                return Err("CLI client disconnected before initial sync".into());
            }
            let initial = manager.connect_native(&id, executable, dir, true)?;
            let _ = frame(socket, PROGRESS, b"Syncing project...");
            let initial = await_initial(initial, Some(socket));
            let result = initial.and_then(|()| {
                frame(socket, READY, &ready).map_err(socket_error)?;
                read_broker_close(socket).map_err(socket_error)
            });
            let reconcile = matches!(result, Ok(true));
            let finish = || manager.finish_native(&id, reconcile);
            let finished = if reconcile {
                with_final_sync_progress(socket, FINAL_SYNC_HEARTBEAT, finish)
            } else {
                finish()
            };
            result?;
            finished?;
            frame(socket, CLOSED, &[]).map_err(socket_error)
        }
    }
}

fn with_final_sync_progress(
    socket: &UnixStream,
    interval: Duration,
    finish: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    std::thread::scope(|scope| {
        let (stop, stopped) = mpsc::channel::<()>();
        let heartbeat = std::thread::Builder::new()
            .name("cabin-cli-final-sync-progress".into())
            .spawn_scoped(scope, move || {
                let mut socket = socket;
                loop {
                    if frame(&mut socket, PROGRESS, b"Finishing project sync...").is_err() {
                        let _ = socket.shutdown(Shutdown::Both);
                        break;
                    }
                    if !matches!(
                        stopped.recv_timeout(interval),
                        Err(mpsc::RecvTimeoutError::Timeout)
                    ) {
                        break;
                    }
                }
            });
        // The reconciliation owns the lease even if the client or progress worker goes away.
        let finished = finish();
        drop(stop);
        if let Ok(heartbeat) = heartbeat {
            let _ = heartbeat.join();
        }
        finished
    })
}

fn read_broker_close(socket: &mut UnixStream) -> io::Result<bool> {
    // An active job may run indefinitely. Once the close message starts, the
    // client has a bounded time to deliver its empty frame.
    socket.set_read_timeout(None)?;
    let mut kind = [0];
    socket.read_exact(&mut kind)?;
    let reconcile = match kind[0] {
        CLOSE => true,
        // The execution lane did not confirm it was safe to run final sync.
        CANCEL => false,
        _ => return Err(io::Error::other("invalid Cabin CLI close")),
    };
    socket.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut size = [0; 4];
    socket.read_exact(&mut size)?;
    if size != [0; 4] {
        return Err(io::Error::other("invalid Cabin CLI close"));
    }
    Ok(reconcile)
}

fn peer_disconnected(socket: &UnixStream) -> bool {
    let mut fd = libc::pollfd {
        fd: socket.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let n = unsafe { libc::poll(&mut fd, 1, 0) };
    if n <= 0 {
        return false;
    }
    if fd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return true;
    }
    if fd.revents & libc::POLLIN != 0 {
        let mut byte = 0u8;
        let peek = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        return peek == 0;
    }
    false
}

fn await_initial(
    initial: mpsc::Receiver<Result<(), String>>,
    client: Option<&UnixStream>,
) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if client.is_some_and(peer_disconnected) {
            return Err("CLI client disconnected during initial sync".into());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("initial sync did not complete in time".into());
        }
        let timeout = if client.is_some() {
            remaining.min(Duration::from_millis(100))
        } else {
            remaining
        };
        match initial.recv_timeout(timeout) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("initial sync stopped before reporting a result".into());
            }
        }
    }
}

pub(crate) fn lock_native_project(dir: &Path) -> io::Result<File> {
    exclusive(&dir.join("project.lock")).map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            io::Error::new(
                error.kind(),
                "Another native Cabin CLI session or repair is active",
            )
        } else {
            error
        }
    })
}

fn channel_dir() -> io::Result<PathBuf> {
    let path = std::env::var_os("CABIN_CLI_CHANNEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(production_dir);
    private_dir(&path)
        .map_err(|e| io::Error::other(format!("Native channel directory {}: {e}", path.display())))
}

fn channel_dir_for_broker() -> Result<PathBuf, String> {
    private_dir(&production_dir())
        .map_err(|_| "Start or restart the VM from the current Cabin desktop app".into())
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
static RESIZE_PENDING: AtomicBool = AtomicBool::new(false);
static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

fn notify_wake(fd: RawFd) {
    if fd >= 0 {
        let byte = 1u8;
        let _ = unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }
}

extern "C" fn terminal_signal(signal: libc::c_int) {
    TERMINAL_SIGNAL.store(signal, Ordering::Relaxed);
    notify_wake(WAKE_FD.load(Ordering::Relaxed));
}

extern "C" fn resize_signal(_: libc::c_int) {
    RESIZE_PENDING.store(true, Ordering::Relaxed);
    notify_wake(WAKE_FD.load(Ordering::Relaxed));
}

fn configure_nonblock_cloexec(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let status = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if status < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, status | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Self-pipe that wakes the stdin `poll()` loop on signals, SIGWINCH, or exit
/// with zero periodic timer wakeups.
struct WakePipe {
    read: OwnedFd,
    write: OwnedFd,
}

impl WakePipe {
    fn open() -> io::Result<Self> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        configure_nonblock_cloexec(read.as_raw_fd())?;
        configure_nonblock_cloexec(write.as_raw_fd())?;
        WAKE_FD.store(write.as_raw_fd(), Ordering::SeqCst);
        Ok(Self { read, write })
    }

    fn read_fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    fn wake(&self) {
        notify_wake(self.write.as_raw_fd());
    }
}

impl Drop for WakePipe {
    fn drop(&mut self) {
        WAKE_FD.store(-1, Ordering::SeqCst);
    }
}

fn drain_pipe(fd: RawFd) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

struct SignalGuard(Vec<(libc::c_int, libc::sigaction)>);
impl SignalGuard {
    fn install(tty: bool) -> io::Result<Self> {
        TERMINAL_SIGNAL.store(0, Ordering::Relaxed);
        RESIZE_PENDING.store(false, Ordering::Relaxed);
        let mut guard = Self(Vec::new());
        let install_one = |guard: &mut Self,
                           signal: libc::c_int,
                           handler: extern "C" fn(libc::c_int)|
         -> io::Result<()> {
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            let mut previous = std::mem::MaybeUninit::uninit();
            action.sa_sigaction = handler as *const () as usize;
            unsafe { libc::sigemptyset(&mut action.sa_mask) };
            if unsafe { libc::sigaction(signal, &action, previous.as_mut_ptr()) } < 0 {
                return Err(io::Error::last_os_error());
            }
            guard.0.push((signal, unsafe { previous.assume_init() }));
            Ok(())
        };
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT] {
            install_one(&mut guard, signal, terminal_signal)?;
        }
        if tty {
            install_one(&mut guard, libc::SIGWINCH, resize_signal)?;
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

fn send_job_cancel(
    socket: &mut UnixStream,
    done: mpsc::Receiver<()>,
    timeout: Duration,
) -> io::Result<std::thread::JoinHandle<()>> {
    let deadline_socket = socket.try_clone()?;
    let watchdog = std::thread::Builder::new()
        .name("cabin-cli-cancel-deadline".into())
        .spawn(move || {
            if matches!(
                done.recv_timeout(timeout),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                let _ = deadline_socket.shutdown(Shutdown::Both);
            }
        })?;
    frame(socket, CANCEL, &[])?;
    Ok(watchdog)
}

fn stream(dir: &Path, lane: &str, job: Job, tty: bool) -> (Result<u8, String>, bool) {
    TERMINAL_SIGNAL.store(0, Ordering::Relaxed);
    let result = stream_job(dir, lane, job, tty);
    let acknowledged = result.is_ok();
    let signal = TERMINAL_SIGNAL.load(Ordering::Relaxed);
    let exit = if signal != 0 {
        if let Err(error) = result {
            eprintln!("cabin: guest did not acknowledge cancellation: {error}");
        }
        Ok((128 + signal) as u8)
    } else {
        result.map_err(|error| error.to_string())
    };
    (exit, acknowledged)
}

fn stream_job(dir: &Path, lane: &str, job: Job, tty: bool) -> io::Result<u8> {
    let _lock = exclusive(&dir.join(format!("{lane}.lock")))?;
    let path = dir.join(format!("{lane}.sock"));
    if !path.symlink_metadata()?.file_type().is_socket() {
        return Err(io::Error::other("native channel is not a socket"));
    }
    let mut sock = UnixStream::connect(path)?;
    sock.set_read_timeout(Some(Duration::from_secs(5)))?;
    sock.set_write_timeout(Some(Duration::from_secs(5)))?;
    // Drop order: `_raw` restores termios, `_signals` restores signal handlers,
    // then `wake` closes the pipe after no signal handler can reference `WAKE_FD`.
    let wake = WakePipe::open()?;
    let wake_read_fd = wake.read_fd();
    let _signals = SignalGuard::install(tty)?;
    let initial_size = match &job {
        Job::Agent {
            tty: Some(size), ..
        }
        | Job::Shell {
            tty: Some(size), ..
        }
        | Job::Exec {
            tty: Some(size), ..
        } => [size.cols, size.rows, size.pixel_width, size.pixel_height],
        _ => winsize(),
    };
    let request = Request {
        version: VERSION,
        job,
    };
    frame(&mut sock, START, &serde_json::to_vec(&request)?)?;
    let mut body = vec![];
    match read_frame(&mut sock, &mut body)? {
        READY if body.is_empty() => {}
        FAILURE => {
            return Err(io::Error::other(
                String::from_utf8_lossy(&body).into_owned(),
            ));
        }
        _ => return Err(io::Error::other("invalid response to native start")),
    }
    sock.set_read_timeout(None)?;
    let _raw = tty.then(Raw::enter).transpose()?;
    let stopped = Arc::new(AtomicBool::new(false));
    let mut tx = sock.try_clone()?;
    let done = Arc::clone(&stopped);
    let mut last = initial_size;
    let (closed, wait_for_close) = mpsc::channel();
    let input_worker = std::thread::Builder::new()
        .name("cabin-cli-input".into())
        .spawn(move || {
            let mut buf = [0; MAX];
            let mut stdin = libc::STDIN_FILENO;
            while !done.load(Ordering::Acquire) {
                let mut fds = [
                    libc::pollfd {
                        fd: stdin,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: wake_read_fd,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                if n < 0 {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        // Fall through to process pending signals or resize immediately.
                    } else {
                        let _ = tx.shutdown(Shutdown::Both);
                        break;
                    }
                }
                if fds[1].revents & libc::POLLIN != 0 {
                    drain_pipe(wake_read_fd);
                }
                if done.load(Ordering::Acquire) {
                    break;
                }
                if TERMINAL_SIGNAL.load(Ordering::Relaxed) != 0 {
                    return match send_job_cancel(&mut tx, wait_for_close, GUEST_CANCEL_TIMEOUT) {
                        Ok(watchdog) => Ok(Some(watchdog)),
                        Err(error) => {
                            let _ = tx.shutdown(Shutdown::Both);
                            Err(error)
                        }
                    };
                }
                if tty && RESIZE_PENDING.swap(false, Ordering::AcqRel) {
                    let size = winsize();
                    if size != last {
                        let mut dimensions = [0; 8];
                        for (bytes, value) in dimensions.as_chunks_mut::<2>().0.iter_mut().zip(size)
                        {
                            bytes.copy_from_slice(&value.to_be_bytes());
                        }
                        if frame(&mut tx, RESIZE, &dimensions).is_err() {
                            let _ = tx.shutdown(Shutdown::Both);
                            break;
                        }
                        last = size;
                    }
                }
                if fds[0].revents & libc::POLLNVAL != 0 {
                    if tty || frame(&mut tx, EOF, &[]).is_err() {
                        let _ = tx.shutdown(Shutdown::Both);
                        break;
                    }
                    stdin = -1;
                    continue;
                }
                if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) == 0 {
                    continue;
                }
                let capacity = if tty { 8192 } else { MAX };
                // Use the descriptor polled above, not stdin's line buffer.
                let read = unsafe { libc::read(stdin, buf.as_mut_ptr().cast(), capacity) };
                let count = if read < 0 {
                    if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    let _ = tx.shutdown(Shutdown::Both);
                    break;
                } else {
                    read as usize
                };
                if done.load(Ordering::Acquire) {
                    break;
                }
                if count == 0 {
                    if tty || frame(&mut tx, EOF, &[]).is_err() {
                        let _ = tx.shutdown(Shutdown::Both);
                        break;
                    }
                    // Keep listening for local termination signals after pipe EOF.
                    stdin = -1;
                    continue;
                }
                if frame(&mut tx, INPUT, &buf[..count]).is_err() {
                    let _ = tx.shutdown(Shutdown::Both);
                    break;
                }
            }
            Ok(None)
        })?;
    let result = (|| {
        let mut stdout = io::stdout().lock();
        let mut stderr = io::stderr().lock();
        loop {
            match read_frame(&mut sock, &mut body)? {
                OUT | ERR if TERMINAL_SIGNAL.load(Ordering::Relaxed) != 0 => {}
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
    stopped.store(true, Ordering::Release);
    wake.wake();
    if result.is_err() {
        let _ = sock.shutdown(Shutdown::Both);
    }
    let input_result = input_worker
        .join()
        .map_err(|_| io::Error::other("native input worker stopped"))?;
    let watchdog = input_result?;
    let result = result.and_then(|code| {
        if watchdog.is_none() {
            // The guest may have exited just as the signal woke the input worker.
            let close = if TERMINAL_SIGNAL.load(Ordering::Relaxed) == 0 {
                CLOSE
            } else {
                CANCEL
            };
            frame(&mut sock, close, &[])?;
        }
        sock.set_read_timeout(Some(Duration::from_secs(5)))?;
        if read_frame(&mut sock, &mut body)? != CLOSED || !body.is_empty() {
            return Err(io::Error::other("native channel did not acknowledge close"));
        }
        Ok(code)
    });
    let _ = sock.shutdown(Shutdown::Both);
    drop(closed);
    if let Some(watchdog) = watchdog {
        let _ = watchdog.join();
    }
    result
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

    #[test]
    fn begin_ready_supports_json_and_legacy_plain_cwd() {
        let json = serde_json::to_vec(&BeginReady {
            cwd: "~/projects/app".into(),
            forward_terminal_env: false,
        })
        .unwrap();
        assert_eq!(
            decode_begin_ready(&json).unwrap(),
            BeginReady {
                cwd: "~/projects/app".into(),
                forward_terminal_env: false,
            }
        );
        assert_eq!(
            decode_begin_ready(b"~/projects/legacy").unwrap(),
            BeginReady {
                cwd: "~/projects/legacy".into(),
                forward_terminal_env: true,
            }
        );
    }

    #[test]
    fn peer_disconnect_is_detected_without_blocking() {
        let (a, b) = UnixStream::pair().unwrap();
        assert!(!peer_disconnected(&a));
        drop(b);
        assert!(peer_disconnected(&a));
    }

    #[test]
    fn initial_sync_uses_the_cycle_result_and_detects_a_closed_worker() {
        let (send, receive) = mpsc::channel();
        send.send(Err("initial sync had partial failures".into()))
            .unwrap();
        assert_eq!(
            await_initial(receive, None).unwrap_err(),
            "initial sync had partial failures"
        );

        let (send, receive) = mpsc::channel();
        drop(send);
        assert_eq!(
            await_initial(receive, None).unwrap_err(),
            "initial sync stopped before reporting a result"
        );
    }

    #[test]
    fn initial_sync_stops_waiting_when_the_broker_peer_disconnects() {
        let (client, broker) = UnixStream::pair().unwrap();
        let (_send, receive) = mpsc::channel();
        let thread = std::thread::spawn(move || await_initial(receive, Some(&broker)));
        drop(client);
        assert_eq!(
            thread.join().unwrap().unwrap_err(),
            "CLI client disconnected during initial sync"
        );
    }

    #[test]
    fn broker_accepts_only_empty_close_frames() {
        let (mut client, mut broker) = UnixStream::pair().unwrap();
        frame(&mut client, CLOSE, &[]).unwrap();
        assert!(read_broker_close(&mut broker).unwrap());

        frame(&mut client, CANCEL, &[]).unwrap();
        assert!(!read_broker_close(&mut broker).unwrap());

        frame(&mut client, CLOSE, b"unexpected").unwrap();
        assert!(read_broker_close(&mut broker).is_err());
    }

    #[test]
    fn final_sync_sends_progress_until_the_result_is_ready() {
        let (mut client, mut broker) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        broker
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let (complete, work) = mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            with_final_sync_progress(&broker, Duration::from_millis(25), || {
                work.recv_timeout(Duration::from_secs(2))
                    .map_err(|e| e.to_string())
            })?;
            frame(&mut broker, CLOSED, &[]).map_err(|e| e.to_string())
        });

        let mut body = Vec::new();
        assert_eq!(read_frame(&mut client, &mut body).unwrap(), PROGRESS);
        assert_eq!(read_frame(&mut client, &mut body).unwrap(), PROGRESS);
        complete.send(()).unwrap();
        assert!(broker_reply(&mut client, CLOSED).unwrap().is_empty());
        server.join().unwrap().unwrap();
    }

    #[test]
    fn final_sync_runs_after_the_progress_peer_disconnects() {
        let (client, broker) = UnixStream::pair().unwrap();
        drop(client);
        let result = with_final_sync_progress(&broker, Duration::from_millis(25), || {
            Err("final sync failed".into())
        });
        assert_eq!(result.unwrap_err(), "final sync failed");
    }

    #[test]
    fn broker_lifetime_clears_readiness_and_removes_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let state = HostCliState::open();
        let path = dir.path().join("host.sock");
        drop(UnixListener::bind(&path).unwrap());
        let listener = bind_broker(&path).unwrap();
        assert_eq!(
            bind_broker(&path).unwrap_err().kind(),
            io::ErrorKind::AddrInUse
        );
        let lifetime = BrokerLifetime {
            path: path.clone(),
            state: state.clone(),
        };
        state.set_broker_running(true);
        assert!(state.broker_running());
        drop(listener);
        drop(lifetime);
        assert!(!state.broker_running());
        assert!(!path.exists());
    }

    #[test]
    fn active_native_project_blocks_repair_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let active = lock_native_project(dir.path()).unwrap();
        assert_eq!(
            lock_native_project(dir.path()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(active);
        assert!(lock_native_project(dir.path()).is_ok());
    }

    #[test]
    fn native_signal_waits_for_guest_exit_and_close_after_stdin_eof() {
        check_native_signal(true);
        check_native_signal(false);
    }

    #[test]
    fn cancellation_deadline_releases_a_silent_guest() {
        let (mut host, mut guest) = UnixStream::pair().unwrap();
        host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (_lifetime, closed) = mpsc::channel();
        let watchdog = send_job_cancel(&mut host, closed, Duration::from_millis(25)).unwrap();
        assert_eq!(read_frame(&mut guest, &mut vec![]).unwrap(), CANCEL);
        assert_eq!(host.read(&mut [0]).unwrap(), 0);
        watchdog.join().unwrap();
    }

    fn check_native_signal(acknowledge: bool) {
        const CHILD_TEST: &str = "native_cli::tests::run_native_signal_child";
        let dir = tempfile::tempdir().unwrap();
        let listener = UnixListener::bind(dir.path().join("exec.sock")).unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", CHILD_TEST, "--ignored"])
            .env("CABIN_TEST_SIGNAL_CHANNEL", dir.path())
            .env(
                "CABIN_TEST_SIGNAL_ACK",
                if acknowledge { "yes" } else { "no" },
            )
            .stdin(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let (done, result) = mpsc::channel();
        let pid = child.id() as libc::pid_t;
        let server = std::thread::spawn(move || -> io::Result<()> {
            let (mut guest, _) = listener.accept()?;
            guest.set_read_timeout(Some(Duration::from_secs(2)))?;
            let mut body = Vec::new();
            assert_eq!(read_frame(&mut guest, &mut body)?, START);
            frame(&mut guest, READY, &[])?;
            assert_eq!(read_frame(&mut guest, &mut body)?, EOF);
            assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
            assert_eq!(read_frame(&mut guest, &mut body)?, CANCEL);
            assert!(body.is_empty());
            assert!(!peer_disconnected(&guest));
            frame(&mut guest, EXIT, &[0])?;
            if acknowledge {
                // EXIT alone must not let the host release the job or send another close.
                guest.set_read_timeout(Some(Duration::from_millis(100)))?;
                assert!(
                    matches!(guest.read(&mut [0]), Err(error) if error.kind() == io::ErrorKind::WouldBlock || error.kind() == io::ErrorKind::TimedOut)
                );
                frame(&mut guest, CLOSED, &[])?;
                guest.set_read_timeout(Some(Duration::from_secs(2)))?;
                assert_eq!(guest.read(&mut [0])?, 0);
            }
            let _ = done.send(());
            Ok(())
        });
        let closed = result.recv_timeout(Duration::from_secs(5));
        if closed.is_err() {
            let _ = child.kill();
        }
        assert!(child.wait().unwrap().success());
        closed.unwrap();
        server.join().unwrap().unwrap();
    }

    #[test]
    #[ignore = "run by native_signal_waits_for_guest_exit_and_close_after_stdin_eof"]
    fn run_native_signal_child() {
        let Some(dir) = std::env::var_os("CABIN_TEST_SIGNAL_CHANNEL") else {
            return;
        };
        let job = Job::Exec {
            program: "sh".into(),
            argv: vec![],
            cwd: "/".into(),
            tty: None,
            env: vec![],
        };
        // The test harness already has its worker thread, so use the transport's
        // normal signal handler in this disposable process only.
        let (exit, acknowledged) = stream(Path::new(&dir), "exec", job, false);
        assert_eq!(exit.unwrap(), 128 + libc::SIGTERM as u8);
        assert_eq!(
            acknowledged,
            std::env::var("CABIN_TEST_SIGNAL_ACK").as_deref() == Ok("yes")
        );
    }
}
