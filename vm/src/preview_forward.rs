//! Browser previews of dev servers running in the VM. Each previewed guest
//! port gets one long-lived `ssh -N -L 127.0.0.1:<local>:localhost:<port>`
//! (the way an editor's remote port forwarding works), so every HTTP and
//! WebSocket connection the preview opens (hot reload included) rides one
//! SSH connection instead of a handshake each. The Mac side listens on
//! loopback only.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::util::message;
use crate::vm::Runtime;
use crate::{Error, Result};

const READY_TIMEOUT: Duration = Duration::from_secs(8);

struct Forward {
    local: u16,
    pid: u32,
    child: Child,
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

type Registry = HashMap<(PathBuf, u16), Forward>;
static FORWARDS: OnceLock<Mutex<Registry>> = OnceLock::new();

fn forwards() -> Result<std::sync::MutexGuard<'static, Registry>> {
    FORWARDS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| Error::Message("Browser previews are unavailable".into()))
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

fn alive(forward: &mut Forward) -> bool {
    matches!(forward.child.try_wait(), Ok(None))
}

/// The local URL for the guest's `port`, starting its forward if needed.
pub fn start(cfg: &Config, port: u16) -> Result<String> {
    if port == 0 {
        return message("Choose a port to preview");
    }
    let runtime = Runtime::new(cfg);
    let pid = runtime
        .pid()
        .filter(|_| runtime.running())
        .ok_or_else(|| Error::Message("Start the machine to preview its servers".into()))?;
    let key = (cfg.instance_dir(), port);
    let mut registry = forwards()?;
    if let Some(forward) = registry.get_mut(&key)
        && forward.pid == pid
        && alive(forward)
    {
        return Ok(url(forward.local));
    }
    registry.remove(&key);
    let mut last = String::new();
    // The free port is only reserved until ssh binds it; retry a lost race.
    for _ in 0..3 {
        let local = free_port()?;
        let mut argv = runtime.ssh_argv(false)?;
        let destination = argv
            .pop()
            .ok_or_else(|| Error::Message("Missing SSH target".into()))?;
        argv.extend([
            "-N".into(),
            "-T".into(),
            "-o".into(),
            "ExitOnForwardFailure=yes".into(),
            "-o".into(),
            "ServerAliveInterval=15".into(),
            "-L".into(),
            format!("127.0.0.1:{local}:localhost:{port}"),
            destination,
        ]);
        let child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut forward = Forward { local, pid, child };
        match wait_ready(&mut forward) {
            Ok(()) => {
                registry.insert(key, forward);
                return Ok(url(local));
            }
            Err(error) => last = error,
        }
    }
    message(format!("Could not forward port {port}: {last}"))
}

fn wait_ready(forward: &mut Forward) -> std::result::Result<(), String> {
    let address = SocketAddr::from(([127, 0, 0, 1], forward.local));
    let deadline = Instant::now() + READY_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(Some(_)) = forward.child.try_wait() {
            let mut stderr = String::new();
            if let Some(mut pipe) = forward.child.stderr.take() {
                let _ = std::io::Read::read_to_string(&mut pipe, &mut stderr);
            }
            return Err(stderr.trim().lines().last().unwrap_or("ssh exited").to_string());
        }
        if TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() {
            // Don't let a chatty stderr pipe fill up and stall ssh.
            if let Some(pipe) = forward.child.stderr.take() {
                thread::spawn(move || std::io::copy(&mut { pipe }, &mut std::io::sink()));
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(80));
    }
    Err("timed out".into())
}

fn url(local: u16) -> String {
    format!("http://127.0.0.1:{local}/")
}

/// Close one preview's forward.
pub fn close(cfg: &Config, port: u16) -> Result<()> {
    forwards()?.remove(&(cfg.instance_dir(), port));
    Ok(())
}

/// Close every forward of this machine (it is stopping).
pub fn stop(cfg: &Config) -> Result<()> {
    let dir = cfg.instance_dir();
    forwards()?.retain(|(instance, _), _| *instance != dir);
    Ok(())
}
