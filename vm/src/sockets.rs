//! Where the host and QEMU meet: Unix domain sockets on macOS and Linux,
//! loopback TCP ports on Windows.
//!
//! QEMU's control channels (QMP, guest agent, serial console, the gateway
//! stream netdev) are sockets. Unix builds keep file sockets with the same
//! paths as before; Windows builds use 127.0.0.1 ports chosen when the
//! instance is prepared and persisted in `sockets.json` so every process
//! (the app, vmctl) agrees on them.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::util::{atomic_write_json, read_json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Unix(PathBuf),
    /// 127.0.0.1:<port>
    Tcp(u16),
}

impl Endpoint {
    /// Human-readable address for messages and recorded runtime state.
    pub fn describe(&self) -> String {
        match self {
            Self::Unix(path) => path.display().to_string(),
            Self::Tcp(port) => format!("127.0.0.1:{port}"),
        }
    }

    /// Open a connection to the endpoint (QMP, guest agent, console…).
    pub fn connect(&self, timeout: Duration) -> std::io::Result<DuplexStream> {
        let stream = match self {
            #[cfg(unix)]
            Self::Unix(path) => DuplexStream::Unix(UnixStream::connect(path)?),
            #[cfg(not(unix))]
            Self::Unix(path) => {
                let _ = path;
                return Err(std::io::Error::other(
                    "Unix sockets are not available on this platform",
                ));
            }
            Self::Tcp(port) => DuplexStream::Tcp(TcpStream::connect(("127.0.0.1", *port))?),
        };
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        Ok(stream)
    }
}

/// A connected byte stream to QEMU (or its console), either flavor.
pub enum DuplexStream {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Read for DuplexStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
            Self::Tcp(stream) => stream.read(buf),
        }
    }
}

impl Write for DuplexStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
            Self::Tcp(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
            Self::Tcp(stream) => stream.flush(),
        }
    }
}

impl DuplexStream {
    pub fn try_clone(&self) -> std::io::Result<Self> {
        Ok(match self {
            #[cfg(unix)]
            Self::Unix(stream) => Self::Unix(stream.try_clone()?),
            Self::Tcp(stream) => Self::Tcp(stream.try_clone()?),
        })
    }

    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_read_timeout(timeout),
            Self::Tcp(stream) => stream.set_read_timeout(timeout),
        }
    }

    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_write_timeout(timeout),
            Self::Tcp(stream) => stream.set_write_timeout(timeout),
        }
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_nonblocking(nonblocking),
            Self::Tcp(stream) => stream.set_nonblocking(nonblocking),
        }
    }

    pub fn shutdown_both(&self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
            Self::Tcp(stream) => stream.shutdown(Shutdown::Both),
        }
    }
}

/// `-qmp` argument for a listening endpoint.
pub fn qmp_listen_arg(endpoint: &Endpoint) -> String {
    match endpoint {
        Endpoint::Unix(path) => format!("unix:{},server=on,wait=off", path.display()),
        Endpoint::Tcp(port) => format!("tcp:127.0.0.1:{port},server=on,wait=off"),
    }
}

/// `-chardev socket…` argument for a listening endpoint (guest agent, console).
pub fn listen_chardev(endpoint: &Endpoint, id: &str, extra: &str) -> String {
    let address = match endpoint {
        Endpoint::Unix(path) => format!("path={}", path.display()),
        Endpoint::Tcp(port) => format!("host=127.0.0.1,port={port}"),
    };
    format!("socket,{address},server=on,wait=off{extra},id={id}")
}

/// `-netdev stream…` argument where QEMU connects to the gateway.
pub fn stream_netdev(endpoint: &Endpoint, id: &str, reconnect: &str) -> String {
    let address = match endpoint {
        Endpoint::Unix(path) => format!("addr.type=unix,addr.path={}", path.display()),
        Endpoint::Tcp(port) => format!("addr.type=inet,addr.host=127.0.0.1,addr.port={port}"),
    };
    format!("stream,id={id},server=off,{address},{reconnect}")
}

/// The instance's port set: persisted values, allocating free loopback ports
/// (and recording them) when this is the first use of the instance.
fn ports(cfg: &Config) -> Option<Ports> {
    load_ports(cfg).or_else(|| ensure_ports(cfg).ok())
}

/// The netdev endpoint QEMU dials for its data NIC.
pub fn net_endpoint(cfg: &Config) -> Endpoint {
    #[cfg(unix)]
    {
        Endpoint::Unix(cfg.net_socket())
    }
    #[cfg(windows)]
    {
        Endpoint::Tcp(ports(cfg).map_or(0, |ports| ports.net))
    }
}

pub fn qmp_endpoint(cfg: &Config) -> Endpoint {
    #[cfg(unix)]
    {
        Endpoint::Unix(cfg.qmp_socket())
    }
    #[cfg(windows)]
    {
        Endpoint::Tcp(ports(cfg).map_or(0, |ports| ports.qmp))
    }
}

pub fn qga_endpoint(cfg: &Config) -> Endpoint {
    #[cfg(unix)]
    {
        Endpoint::Unix(cfg.qga_socket())
    }
    #[cfg(windows)]
    {
        Endpoint::Tcp(ports(cfg).map_or(0, |ports| ports.qga))
    }
}

pub fn console_endpoint(cfg: &Config) -> Endpoint {
    #[cfg(unix)]
    {
        Endpoint::Unix(cfg.console_socket())
    }
    #[cfg(windows)]
    {
        Endpoint::Tcp(ports(cfg).map_or(0, |ports| ports.console))
    }
}

/// The persisted per-instance port set (`sockets.json`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Ports {
    pub qmp: u16,
    pub qga: u16,
    pub console: u16,
    pub net: u16,
}

fn ports_path(cfg: &Config) -> PathBuf {
    crate::paths::InstancePaths::new(cfg.instance_dir()).file("sockets.json")
}

fn load_ports(cfg: &Config) -> Option<Ports> {
    let value = read_json(&ports_path(cfg))?;
    let ports: Ports = serde_json::from_value(value).ok()?;
    if ports.qmp != 0 && ports.qga != 0 && ports.console != 0 && ports.net != 0 {
        Some(ports)
    } else {
        None
    }
}

/// Read the instance's ports, choosing free loopback ports and persisting
/// them when absent. Every channel of the instance uses the same set.
pub fn ensure_ports(cfg: &Config) -> std::io::Result<Ports> {
    if let Some(ports) = load_ports(cfg) {
        return Ok(ports);
    }
    #[cfg(unix)]
    {
        let _ = cfg;
        Err(std::io::Error::other("ports are only needed on Windows"))
    }
    #[cfg(windows)]
    {
        let listeners: Vec<TcpListener> = (0..4)
            .map(|_| TcpListener::bind(("127.0.0.1", 0)))
            .collect::<std::result::Result<_, _>>()?;
        let assigned: Vec<u16> = listeners
            .iter()
            .map(|listener| listener.local_addr().map(|addr| addr.port()))
            .collect::<std::result::Result<_, _>>()?;
        drop(listeners);
        if assigned.iter().any(|port| *port == 0) {
            return Err(std::io::Error::other("no free loopback ports"));
        }
        let ports = Ports {
            qmp: assigned[0],
            qga: assigned[1],
            console: assigned[2],
            net: assigned[3],
        };
        if let Err(error) = atomic_write_json(&ports_path(cfg), &serde_json::json!(ports), 0o600) {
            return Err(std::io::Error::other(error.to_string()));
        }
        Ok(ports)
    }
}

/// Remove the persisted port set (Unix ignores it; its sockets are files).
pub fn clear_ports(cfg: &Config) {
    if cfg!(windows) {
        let _ = std::fs::remove_file(ports_path(cfg));
    }
}

#[cfg(unix)]
impl From<std::os::unix::net::UnixStream> for DuplexStream {
    fn from(stream: std::os::unix::net::UnixStream) -> Self {
        Self::Unix(stream)
    }
}

impl From<TcpStream> for DuplexStream {
    fn from(stream: TcpStream) -> Self {
        Self::Tcp(stream)
    }
}
