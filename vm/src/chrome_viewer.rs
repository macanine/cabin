//! The in-app desktop bridge. VNC uses an SSH channel to guest loopback. Each
//! SPICE channel uses its own connection to the private QEMU Unix socket.

use std::collections::HashMap;
use std::io::{self, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::{fs::FileTypeExt, net::UnixStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::{Engine, engine::general_purpose::STANDARD};
use sha1::{Digest, Sha1};

use crate::config::Config;
use crate::util::message;
use crate::vm::Runtime;
use crate::{Error, Result};

const MAX_FRAME: u64 = 64 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 24;
const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, PartialEq, Eq)]
enum Target {
    Vnc {
        ssh: Vec<String>,
        log: PathBuf,
    },
    Spice {
        socket: PathBuf,
        parent_origin: String,
    },
}

struct Bridge {
    url: String,
    pid: u32,
    target: Target,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

type Registry = HashMap<(PathBuf, bool), Bridge>;
static BRIDGES: OnceLock<Mutex<Registry>> = OnceLock::new();

fn bridges() -> Result<std::sync::MutexGuard<'static, Registry>> {
    BRIDGES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| Error::Message("The desktop viewer is unavailable".into()))
}

fn token() -> io::Result<String> {
    let mut random = [0; 32];
    crate::util::random_bytes(&mut random);
    Ok(crate::util::hex_lower(&random))
}

fn app_origin(origin: &str) -> bool {
    matches!(
        origin,
        "tauri://localhost"
            | "http://tauri.localhost"
            | "https://tauri.localhost"
            | "http://localhost:1420"
            | "http://127.0.0.1:1420"
            | "null"
    )
}

pub fn start(cfg: &Config) -> Result<String> {
    let runtime = Runtime::new(cfg);
    if !runtime.running() {
        return message("Start the machine to view Chrome");
    }
    if crate::desktop::enabled(cfg) {
        crate::desktop::ensure_wayvnc(cfg, &runtime)?;
    }
    let mut ssh = runtime.ssh_argv(false)?;
    let destination = ssh
        .pop()
        .ok_or_else(|| Error::Message("Missing SSH target".into()))?;
    ssh.extend([
        "-T".into(),
        "-W".into(),
        "127.0.0.1:5900".into(),
        destination,
    ]);
    crate::util::ensure_dir(&cfg.instance_logs_dir())?;
    start_bridge(
        cfg,
        Target::Vnc {
            ssh,
            log: cfg.instance_logs_dir().join("chrome-viewer.log"),
        },
    )
}

pub fn start_spice(cfg: &Config, parent_origin: Option<&str>) -> Result<String> {
    if !cfg.desktop_spice
        || cfg.device_enabled()
        || cfg!(not(unix)) // QEMU ships no SPICE backend on Windows hosts
    {
        return message("SPICE is unavailable for this desktop");
    }
    if !Runtime::new(cfg).running() {
        return message("Start the machine to view Chrome");
    }
    let parent_origin = parent_origin.unwrap_or("tauri://localhost");
    if !app_origin(parent_origin) {
        return message("Invalid SPICE viewer origin");
    }
    crate::vm::check_spice_dir(cfg)?;
    let socket = cfg.spice_socket();
    #[cfg(unix)]
    if !std::fs::symlink_metadata(&socket)?.file_type().is_socket() {
        return message("QEMU's SPICE socket is unavailable");
    }
    start_bridge(
        cfg,
        Target::Spice {
            socket,
            parent_origin: parent_origin.into(),
        },
    )
}

fn start_bridge(cfg: &Config, target: Target) -> Result<String> {
    let runtime = Runtime::new(cfg);
    let pid = runtime.pid().ok_or_else(|| Error::Message("The machine is not running".into()))?;
    let spice = matches!(target, Target::Spice { .. });
    let key = (cfg.instance_dir(), spice);
    let mut registry = bridges()?;
    if let Some(bridge) = registry.get(&key)
        && bridge.target == target
        && bridge.pid == pid
        && !bridge.thread.is_finished()
    {
        return Ok(bridge.url.clone());
    }
    if let Some(bridge) = registry.remove(&key) {
        stop_bridge(bridge);
    }
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.set_nonblocking(true)?;
    let authority = listener.local_addr()?.to_string();
    let token = token()?;
    let url = match &target {
        Target::Vnc { .. } => format!("ws://{authority}/{token}/ws"),
        Target::Spice { parent_origin, .. } => {
            let mut url = url::Url::parse(&format!("http://{authority}/{token}/viewer.html"))
                .map_err(|error| Error::Message(error.to_string()))?;
            url.query_pairs_mut()
                .append_pair("parentOrigin", parent_origin);
            url.into()
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker_target = target.clone();
    let thread = thread::Builder::new()
        .name("chrome-viewer".into())
        .spawn(move || {
            let running = || runtime.pid() == Some(pid) && runtime.alive();
            serve(listener, &worker_stop, worker_target, &authority, &token, running);
        })?;
    registry.insert(
        key,
        Bridge {
            url: url.clone(),
            pid,
            target,
            stop,
            thread,
        },
    );
    Ok(url)
}

fn stop_bridge(bridge: Bridge) {
    bridge.stop.store(true, Ordering::Relaxed);
    let _ = bridge.thread.join();
}

pub fn stop(cfg: &Config) -> Result<()> {
    let mut registry = bridges()?;
    for spice in [false, true] {
        if let Some(bridge) = registry.remove(&(cfg.instance_dir(), spice)) {
            stop_bridge(bridge);
        }
    }
    Ok(())
}

struct Peer {
    client: TcpStream,
    closed: AtomicBool,
    upstream: Mutex<Option<Upstream>>,
}

enum Upstream {
    #[cfg(unix)]
    Unix(UnixStream),
    Ssh(Child),
}

impl Drop for Upstream {
    fn drop(&mut self) {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => {
                let _ = socket.shutdown(Shutdown::Both);
            }
            Self::Ssh(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

impl Peer {
    fn attach(&self, upstream: Upstream) -> io::Result<()> {
        let mut slot = self
            .upstream
            .lock()
            .map_err(|_| io::Error::other("Viewer upstream lock poisoned"))?;
        if self.closed.load(Ordering::Relaxed) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "Viewer closed",
            ));
        }
        *slot = Some(upstream);
        Ok(())
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        let _ = self.client.shutdown(Shutdown::Both);
        if let Ok(mut slot) = self.upstream.lock() {
            slot.take();
        }
    }
}

fn serve(
    listener: TcpListener,
    stop: &AtomicBool,
    target: Target,
    authority: &str,
    token: &str,
    running: impl Fn() -> bool,
) {
    let mut workers: Vec<(Arc<Peer>, JoinHandle<()>)> = Vec::new();
    let path = Arc::new(format!("/{token}/"));
    let authority = Arc::new(authority.to_string());
    let target = Arc::new(target);
    let mut next_check = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if Instant::now() >= next_check {
            if !running() {
                break;
            }
            next_check = Instant::now() + Duration::from_secs(1);
        }
        let mut index = 0;
        while index < workers.len() {
            if workers[index].1.is_finished() {
                let (_, worker) = workers.swap_remove(index);
                let _ = worker.join();
            } else {
                index += 1;
            }
        }
        match listener.accept() {
            Ok((client, _)) if workers.len() < MAX_CONNECTIONS => {
                let Ok(clone) = client.try_clone() else {
                    continue;
                };
                let peer = Arc::new(Peer {
                    client: clone,
                    closed: false.into(),
                    upstream: Mutex::new(None),
                });
                let (worker_peer, target, authority, path) = (
                    peer.clone(),
                    target.clone(),
                    authority.clone(),
                    path.clone(),
                );
                if let Ok(worker) =
                    thread::Builder::new()
                        .name("desktop-channel".into())
                        .spawn(move || {
                            let _ = connection(client, &worker_peer, &target, &authority, &path);
                            worker_peer.close();
                        })
                {
                    workers.push((peer, worker));
                }
            }
            Ok(_) => thread::sleep(Duration::from_millis(20)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(_) => break,
        }
    }
    drop(listener);
    for (peer, _) in &workers {
        peer.close();
    }
    for (_, worker) in workers {
        let _ = worker.join();
    }
}

struct Request<'a> {
    uri: &'a str,
    headers: Vec<(&'a str, &'a str)>,
}

impl<'a> Request<'a> {
    fn parse(text: &'a str) -> Option<Self> {
        let mut lines = text.split("\r\n");
        let mut first = lines.next()?.split_ascii_whitespace();
        if first.next()? != "GET" {
            return None;
        }
        let uri = first.next()?;
        if first.next()? != "HTTP/1.1" || first.next().is_some() {
            return None;
        }
        let headers = lines
            .take_while(|line| !line.is_empty())
            .map(|line| line.split_once(':'))
            .collect::<Option<Vec<_>>>()?;
        Some(Self { uri, headers })
    }

    fn header(&self, name: &str) -> Option<&'a str> {
        let mut matches = self
            .headers
            .iter()
            .filter(|(key, _)| key.eq_ignore_ascii_case(name));
        let (_, value) = matches.next()?;
        matches.next().is_none().then(|| value.trim())
    }

    fn offers(&self, name: &str, value: &str) -> bool {
        self.header(name).is_some_and(|header| {
            header
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case(value))
        })
    }

    fn websocket_key(&self) -> Option<&'a str> {
        let key = self.header("Sec-WebSocket-Key")?;
        (self.offers("Upgrade", "websocket")
            && self.offers("Connection", "upgrade")
            && self.header("Sec-WebSocket-Version") == Some("13")
            && STANDARD.decode(key).is_ok_and(|bytes| bytes.len() == 16))
        .then_some(key)
    }
}

fn read_request(client: &mut impl Read) -> io::Result<String> {
    let deadline = Instant::now() + HTTP_TIMEOUT;
    let mut bytes = Vec::new();
    while bytes.len() < 8192 && Instant::now() < deadline {
        let mut byte = [0];
        client.read_exact(&mut byte)?;
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            return String::from_utf8(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Invalid viewer HTTP request",
    ))
}

fn connection(
    mut client: TcpStream,
    peer: &Peer,
    target: &Target,
    authority: &str,
    prefix: &str,
) -> io::Result<()> {
    client.set_nonblocking(false)?;
    client.set_read_timeout(Some(HTTP_TIMEOUT))?;
    client.set_write_timeout(Some(HTTP_TIMEOUT))?;
    let mut reader = BufReader::new(client.try_clone()?);
    let text = read_request(&mut reader)?;
    let forbidden = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let Some(request) = Request::parse(&text).filter(|req| req.header("Host") == Some(authority))
    else {
        return client.write_all(forbidden);
    };
    let Some(path) = request
        .uri
        .split('?')
        .next()
        .and_then(|uri| uri.strip_prefix(prefix))
    else {
        return client.write_all(forbidden);
    };
    if path != "ws" {
        return match target {
            Target::Spice { .. } => serve_asset(client, path, authority),
            Target::Vnc { .. } => client.write_all(forbidden),
        };
    }
    let spice = matches!(target, Target::Spice { .. });
    let origin = request.header("Origin");
    let binary = request.offers("Sec-WebSocket-Protocol", "binary");
    let origin_allowed = if spice {
        binary && origin == Some(format!("http://{authority}").as_str())
    } else {
        origin.is_none_or(app_origin)
    };
    let Some(key) = request.websocket_key().filter(|_| origin_allowed) else {
        return client.write_all(forbidden);
    };
    let (mut upstream_reader, mut upstream_writer) = connect_upstream(peer, target)?;
    let mut hash = Sha1::new();
    hash.update(key.as_bytes());
    hash.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    let accept = STANDARD.encode(hash.finalize());
    let protocol = if binary {
        "Sec-WebSocket-Protocol: binary\r\n"
    } else {
        ""
    };
    write!(
        client,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n{protocol}\r\n"
    )?;
    client.set_read_timeout(None)?;
    let writer = Arc::new(Mutex::new(client));
    let out_writer = writer.clone();
    let output = thread::Builder::new()
        .name("desktop-frames".into())
        .spawn(move || {
            let mut buffer = [0; 65536];
            while let Ok(len) = upstream_reader.read(&mut buffer) {
                if len == 0 || write_frame(&out_writer, 2, &buffer[..len]).is_err() {
                    break;
                }
            }
            if let Ok(socket) = out_writer.lock() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        })?;
    let result = forward_frames(&mut reader, &writer, &mut upstream_writer);
    drop(upstream_writer);
    peer.close();
    let _ = output.join();
    result
}

type UpstreamIo = (Box<dyn Read + Send>, Box<dyn Write>);

fn connect_upstream(peer: &Peer, target: &Target) -> io::Result<UpstreamIo> {
    match target {
        #[cfg(unix)]
        Target::Spice { socket, .. } => {
            use socket2::{Domain, SockAddr, Socket, Type};
            let upstream = Socket::new(Domain::UNIX, Type::STREAM, None)?;
            upstream.connect_timeout(&SockAddr::unix(socket)?, HTTP_TIMEOUT)?;
            let stream = UnixStream::from(upstream);
            stream.set_write_timeout(Some(HTTP_TIMEOUT))?;
            peer.attach(Upstream::Unix(stream.try_clone()?))?;
            Ok((Box::new(stream.try_clone()?), Box::new(stream)))
        }
        #[cfg(not(unix))]
        Target::Spice { .. } => Err(io::Error::other("SPICE requires a Unix host")),
        Target::Vnc { ssh, log } => {
            let stderr = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log)?;
            let mut ssh_command = Command::new(&ssh[0]);
            ssh_command.args(&ssh[1..]);
            crate::util::no_window(&mut ssh_command);
            let mut child = ssh_command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::from(stderr))
                .spawn()?;
            let stdin = child.stdin.take();
            let stdout = child.stdout.take();
            peer.attach(Upstream::Ssh(child))?;
            let stdin = stdin.ok_or_else(|| io::Error::other("SSH input unavailable"))?;
            let stdout = stdout.ok_or_else(|| io::Error::other("SSH output unavailable"))?;
            Ok((Box::new(stdout), Box::new(stdin)))
        }
    }
}

fn write_frame(writer: &Mutex<TcpStream>, opcode: u8, bytes: &[u8]) -> io::Result<()> {
    let mut header = [0; 10];
    header[0] = 0x80 | opcode;
    let size = if bytes.len() < 126 {
        header[1] = bytes.len() as u8;
        2
    } else if bytes.len() <= u16::MAX as usize {
        header[1] = 126;
        header[2..4].copy_from_slice(&(bytes.len() as u16).to_be_bytes());
        4
    } else {
        header[1] = 127;
        header[2..].copy_from_slice(&(bytes.len() as u64).to_be_bytes());
        10
    };
    let mut socket = writer
        .lock()
        .map_err(|_| io::Error::other("Viewer writer lock poisoned"))?;
    socket.write_all(&header[..size])?;
    socket.write_all(bytes)
}

fn forward_frames(
    client: &mut impl Read,
    writer: &Mutex<TcpStream>,
    upstream: &mut impl Write,
) -> io::Result<()> {
    let mut fragmented = false;
    let mut bytes = [0; 65536];
    loop {
        let mut header = [0; 2];
        client.read_exact(&mut header)?;
        let opcode = header[0] & 15;
        let final_frame = header[0] & 128 != 0;
        let invalid =
            || io::Error::new(io::ErrorKind::InvalidData, "Invalid viewer WebSocket frame");
        let mut length = u64::from(header[1] & 127);
        if header[0] & 0x70 != 0
            || header[1] & 128 == 0
            || (opcode >= 8 && (!final_frame || length > 125))
        {
            return Err(invalid());
        }
        length = match length {
            126 => {
                let mut size = [0; 2];
                client.read_exact(&mut size)?;
                u64::from(u16::from_be_bytes(size))
            }
            127 => {
                let mut size = [0; 8];
                client.read_exact(&mut size)?;
                u64::from_be_bytes(size)
            }
            _ => length,
        };
        if length > MAX_FRAME {
            return Err(invalid());
        }
        let mut mask = [0; 4];
        client.read_exact(&mut mask)?;
        match opcode {
            0 if fragmented => fragmented = !final_frame,
            2 if !fragmented => fragmented = !final_frame,
            8..=10 => {
                let payload = &mut bytes[..length as usize];
                read_masked(client, payload, mask, 0)?;
                match opcode {
                    8 => return write_frame(writer, 8, payload),
                    9 => write_frame(writer, 10, payload)?,
                    _ => {}
                }
                continue;
            }
            _ => return Err(invalid()),
        }
        let mut offset = 0;
        while offset < length {
            let size = (length - offset).min(bytes.len() as u64) as usize;
            read_masked(client, &mut bytes[..size], mask, offset as usize)?;
            upstream.write_all(&bytes[..size])?;
            offset += size as u64;
        }
    }
}

fn read_masked(
    client: &mut impl Read,
    bytes: &mut [u8],
    mask: [u8; 4],
    offset: usize,
) -> io::Result<()> {
    client.read_exact(bytes)?;
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte ^= mask[(offset + index) % 4];
    }
    Ok(())
}

fn serve_asset(mut client: TcpStream, path: &str, authority: &str) -> io::Result<()> {
    let Some(bytes) = spice_asset(path) else {
        return client.write_all(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    };
    let content_type = if path.ends_with(".js") {
        "text/javascript"
    } else {
        "text/html; charset=utf-8"
    };
    write!(
        client,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; connect-src ws://{authority}; img-src blob: data:; media-src blob:; base-uri 'none'; form-action 'none'\r\n\r\n",
        bytes.len()
    )?;
    client.write_all(bytes)
}

fn spice_asset(path: &str) -> Option<&'static [u8]> {
    macro_rules! assets {
        ($($file:literal),* $(,)?) => {
            match path {
                $($file => Some(include_bytes!(concat!("../assets/spice-html5/", $file))),)*
                _ => None,
            }
        };
    }
    assets!(
        "viewer.html",
        "viewer.js",
        "src/atKeynames.js",
        "src/bitmap.js",
        "src/code_to_scancode.js",
        "src/cursor.js",
        "src/display.js",
        "src/enums.js",
        "src/filexfer.js",
        "src/inputs.js",
        "src/lz.js",
        "src/main.js",
        "src/playback.js",
        "src/png.js",
        "src/port.js",
        "src/quic.js",
        "src/resize.js",
        "src/simulatecursor.js",
        "src/spicearraybuffer.js",
        "src/spiceconn.js",
        "src/spicemsg.js",
        "src/spicetype.js",
        "src/thirdparty/jsbn.js",
        "src/thirdparty/prng4.js",
        "src/thirdparty/rng.js",
        "src/thirdparty/rsa.js",
        "src/thirdparty/sha1.js",
        "src/ticket.js",
        "src/utils.js",
        "src/webm.js",
        "src/wire.js",
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;

    fn client(authority: &str) -> BufReader<TcpStream> {
        let socket = TcpStream::connect(authority).unwrap();
        socket.set_read_timeout(Some(HTTP_TIMEOUT)).unwrap();
        BufReader::new(socket)
    }

    fn request(
        socket: &mut BufReader<TcpStream>,
        authority: &str,
        path: &str,
        origin: &str,
        protocol: &str,
    ) -> String {
        write!(socket.get_mut(), "GET {path} HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: MDEyMzQ1Njc4OWFiY2RlZg==\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n").unwrap();
        let mut response = String::new();
        loop {
            let start = response.len();
            if socket.read_line(&mut response).unwrap() == 0 || &response[start..] == "\r\n" {
                return response;
            }
        }
    }

    #[test]
    fn spice_requires_its_token_origin_and_binary_and_closes_live_channels() {
        let socket = PathBuf::from(format!("/tmp/va-ws-{}-{}.sock", std::process::id(), crate::util::localstamp()));
        let upstream = UnixListener::bind(&socket).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let authority = listener.local_addr().unwrap().to_string();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let target = Target::Spice { socket: socket.clone(), parent_origin: "tauri://localhost".into() };
        let worker_authority = authority.clone();
        let server = thread::spawn(move || {
            serve(
                listener,
                &worker_stop,
                target,
                &worker_authority,
                "secret",
                || true,
            )
        });
        let origin = format!("http://{authority}");
        for (path, from, protocol) in [
            ("/wrong/ws", origin.as_str(), "binary"),
            ("/secret/ws", "http://localhost:1420", "binary"),
            ("/secret/ws", origin.as_str(), "text"),
        ] {
            let response = request(&mut client(&authority), &authority, path, from, protocol);
            assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        }
        let css = request(&mut client(&authority), &authority, "/secret/src/main.js", &origin, "binary");
        assert!(css.contains("Content-Type: text/javascript"));
        let missing = request(&mut client(&authority), &authority, "/secret/../viewer.html", &origin, "binary");
        assert!(missing.starts_with("HTTP/1.1 404"));

        let mut channels = Vec::new();
        for byte in *b"mdic" {
            let mut viewer = client(&authority);
            let response = request(&mut viewer, &authority, "/secret/ws", &origin, "binary");
            assert!(response.starts_with("HTTP/1.1 101"), "{response}");
            assert!(response.contains("Sec-WebSocket-Protocol: binary"));
            let (mut channel, _) = upstream.accept().unwrap();
            channel.set_read_timeout(Some(HTTP_TIMEOUT)).unwrap();
            viewer.get_mut().write_all(&[0x82, 0x81, 1, 2, 3, 4, byte ^ 1]).unwrap();
            let mut data = [0];
            channel.read_exact(&mut data).unwrap();
            assert_eq!(data, [byte]);
            channel.write_all(&[byte]).unwrap();
            let mut frame = [0; 3];
            viewer.read_exact(&mut frame).unwrap();
            assert_eq!(frame, [0x82, 1, byte]);
            channels.push((viewer, channel));
        }
        let _partial_request = client(&authority);
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        for (mut viewer, mut channel) in channels {
            assert_eq!(viewer.read(&mut [0]).unwrap(), 0);
            assert_eq!(channel.read(&mut [0]).unwrap(), 0);
        }
        std::fs::remove_file(socket).unwrap();
    }
}
