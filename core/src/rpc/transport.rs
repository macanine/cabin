use std::fs::{self, File, OpenOptions, Permissions, TryLockError};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak, mpsc};
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::app::App;
use crate::error::{Error, Result};
use crate::util::LockExt;

use super::{PROTOCOL, VERSION, build_id, handle_line};

const OUTPUT_QUEUE: usize = 256;
const MAX_INFLIGHT: usize = 64;
const MAX_CLIENTS: usize = 16;
// Four 8 MiB chat attachments use about 43 MiB with base64 encoding.
const MAX_REQUEST_LINE: u64 = 48 * 1024 * 1024;
// Charge large requests separately so they leave slots available for small ones.
const SMALL_REQUEST: usize = 1024 * 1024;
const LARGE_REQUEST_BYTES: usize = 128 * 1024 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

fn line_of(value: Value) -> Arc<str> {
    let mut line = value.to_string();
    line.push('\n');
    line.into()
}

/// The reader and writer own the socket. Requests and subscriptions can only
/// hold the output queue and signal those workers to disconnect.
#[derive(Clone)]
struct Out {
    id: u64,
    queue: Arc<Mutex<Option<mpsc::SyncSender<Arc<str>>>>>,
    socket: Weak<UnixStream>,
}

impl Out {
    fn new(id: u64, socket: &Arc<UnixStream>, capacity: usize) -> (Self, mpsc::Receiver<Arc<str>>) {
        let (queue, lines) = mpsc::sync_channel(capacity);
        let out = Self {
            id,
            queue: Arc::new(Mutex::new(Some(queue))),
            socket: Arc::downgrade(socket),
        };
        (out, lines)
    }

    fn send(&self, line: Arc<str>) -> bool {
        let mut queue = self.queue.locked();
        let Some(sender) = queue.as_ref() else {
            return false;
        };
        if sender.try_send(line).is_ok() {
            return true;
        }
        queue.take();
        drop(queue);
        self.shutdown();
        false
    }

    fn abort(&self) {
        self.queue.locked().take();
        self.shutdown();
    }

    fn shutdown(&self) {
        if let Some(socket) = self.socket.upgrade() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }
}

#[derive(Default)]
struct Clients(Mutex<Vec<Out>>);

impl Clients {
    fn register(&self, out: &Out) -> Subscription<'_> {
        self.0.locked().push(out.clone());
        Subscription {
            clients: self,
            id: out.id,
        }
    }

    fn broadcast(&self, line: Arc<str>) {
        let snapshot = self.0.locked().clone();
        for out in snapshot {
            out.send(line.clone());
        }
    }
}

struct Subscription<'a> {
    clients: &'a Clients,
    id: u64,
}

impl Drop for Subscription<'_> {
    fn drop(&mut self) {
        self.clients.0.locked().retain(|out| out.id != self.id);
    }
}

struct CloseSocket(Arc<UnixStream>);

impl Drop for CloseSocket {
    fn drop(&mut self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

struct Server {
    app: Arc<App>,
    clients: Arc<Clients>,
    client_slots: Arc<Semaphore>,
    request_slots: Arc<Semaphore>,
    request_bytes: Arc<Semaphore>,
    next_id: AtomicU64,
    hello: Arc<str>,
}

impl Server {
    fn new(app: Arc<App>) -> Self {
        Self {
            app,
            clients: Arc::new(Clients::default()),
            client_slots: Arc::new(Semaphore::new(MAX_CLIENTS)),
            request_slots: Arc::new(Semaphore::new(MAX_INFLIGHT)),
            request_bytes: Arc::new(Semaphore::new(LARGE_REQUEST_BYTES)),
            next_id: AtomicU64::new(1),
            hello: line_of(json!({
                "hello": { "version": VERSION, "protocol": PROTOCOL, "build": build_id() }
            })),
        }
    }

    fn accept(self: &Arc<Self>, stream: UnixStream) -> io::Result<bool> {
        let Ok(permit) = self.client_slots.clone().try_acquire_owned() else {
            return Ok(false);
        };
        let server = self.clone();
        thread::Builder::new()
            .name("rpc-client".into())
            .spawn(move || {
                let permit = Arc::new(permit);
                if let Err(error) = server.serve_client(stream, &permit) {
                    cabin_diagnostics::event(
                        cabin_diagnostics::Level::Warn,
                        "rpc",
                        "client_setup_failed",
                        format_args!("{error}"),
                    );
                }
            })?;
        Ok(true)
    }

    fn serve_client(
        &self,
        stream: UnixStream,
        permit: &Arc<OwnedSemaphorePermit>,
    ) -> io::Result<()> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let socket = Arc::new(stream);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (out, lines) = Out::new(id, &socket, OUTPUT_QUEUE);
        let writer_permit = permit.clone();
        let close = CloseSocket(socket.clone());
        let hello = self.hello.clone();
        thread::Builder::new()
            .name("rpc-writer".into())
            .spawn(move || {
                let _permit = writer_permit;
                let mut writer = &*close.0;
                if writer.write_all(hello.as_bytes()).is_ok() {
                    for line in lines {
                        if writer.write_all(line.as_bytes()).is_err() {
                            break;
                        }
                    }
                }
                drop(close);
            })?;

        let _subscription = self.clients.register(&out);
        let mut reader = BufReader::new(&*socket);
        if self.read_requests(&mut reader, &out).is_err() {
            out.abort();
        }
        // Drop the event subscription on input EOF. In-flight requests keep
        // the output queue open until their replies have been sent.
        Ok(())
    }

    fn read_requests(&self, reader: &mut impl BufRead, out: &Out) -> io::Result<()> {
        while let Some(line) = read_request_line(reader, MAX_REQUEST_LINE)? {
            if !line.trim().is_empty() && !self.start_request(line, out) {
                break;
            }
        }
        Ok(())
    }

    fn start_request(&self, line: String, out: &Out) -> bool {
        // A long install and terminal input run concurrently. Reserve the slots
        // on the reader before starting a worker, even for pipelined requests.
        let Ok(permit) = self.request_slots.clone().try_acquire_owned() else {
            return unavailable(out, &line, "too many requests are already running");
        };
        let large_request = if line.len() > SMALL_REQUEST {
            let Ok(bytes) = self
                .request_bytes
                .clone()
                .try_acquire_many_owned(line.len() as u32)
            else {
                drop(permit);
                return unavailable(out, &line, "too many large requests are already running");
            };
            Some(bytes)
        } else {
            None
        };
        let line: Arc<str> = line.into();
        let request = line.clone();
        let app = self.app.clone();
        let reply = out.clone();
        let worker = thread::Builder::new()
            .name("rpc-request".into())
            .spawn(move || {
                let _permits = (permit, large_request);
                if let Some(response) = handle_line(&app, &request) {
                    reply.send(line_of(response));
                }
            });
        // Builder drops the closure and its permits if the OS refuses the spawn.
        match worker {
            Ok(_) => true,
            Err(_) => unavailable(out, &line, "could not start the request worker"),
        }
    }
}

fn read_request_line(reader: &mut impl BufRead, limit: u64) -> io::Result<Option<String>> {
    let mut line = String::new();
    let count = reader.take(limit + 1).read_line(&mut line)? as u64;
    if count > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "RPC request line exceeds the size limit",
        ));
    }
    Ok((count != 0).then_some(line))
}

fn unavailable(out: &Out, line: &str, message: &str) -> bool {
    // Deserialize just the ID so a rejected request does not build a second
    // copy of large params on the reader thread.
    #[derive(Deserialize)]
    struct RequestId {
        id: Value,
    }
    let id = serde_json::from_str::<RequestId>(line).map_or(Value::Null, |request| request.id);
    out.send(line_of(
        json!({ "id": id, "error": { "code": "unavailable", "message": message } }),
    ))
}

/// Serialize daemon startups at this path before probing or unlinking a socket.
fn lock_listener(socket: &Path) -> Result<File> {
    let mut lock = socket.as_os_str().to_os_string();
    lock.push(".lock");
    let path = Path::new(&lock);
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| Error::path("open RPC socket lock", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| Error::path("inspect RPC socket lock", path, error))?;
    if !metadata.is_file() || metadata.uid() != effective_uid() {
        return Err(Error::Conflict(format!(
            "RPC socket lock is not a regular file owned by this user: {}",
            path.display()
        )));
    }
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(Error::Conflict(format!(
            "a daemon already owns {}",
            socket.display()
        ))),
        Err(TryLockError::Error(error)) => Err(Error::path("lock RPC socket", path, error)),
    }
}

struct BoundListener {
    listener: UnixListener,
    _lock: File,
}

fn effective_uid() -> u32 {
    // SAFETY: geteuid has no arguments and reads the process credentials.
    unsafe { libc::geteuid() }
}

fn bind_listener(socket: &Path) -> Result<BoundListener> {
    if let Some(parent) = socket
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        crate::fsx::ensure_dir(parent)?;
    }
    let lock = lock_listener(socket)?;
    match fs::symlink_metadata(socket) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() || metadata.uid() != effective_uid() {
                return Err(Error::Conflict(format!(
                    "RPC socket path is not a socket owned by this user: {}",
                    socket.display()
                )));
            }
            // An older daemon may already be listening without the lock file.
            match UnixStream::connect(socket) {
                Ok(_) => {
                    return Err(Error::Conflict(format!(
                        "a daemon already listens on {}",
                        socket.display()
                    )));
                }
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {}
                Err(error) => return Err(Error::path("check existing RPC socket", socket, error)),
            }
            let current = fs::symlink_metadata(socket)
                .map_err(|error| Error::path("inspect RPC socket", socket, error))?;
            if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
                return Err(Error::Conflict(format!(
                    "RPC socket changed while checking it: {}",
                    socket.display()
                )));
            }
            fs::remove_file(socket)
                .map_err(|error| Error::path("remove stale socket", socket, error))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(Error::path("inspect RPC socket", socket, error)),
    }
    let listener =
        UnixListener::bind(socket).map_err(|error| Error::path("listen on", socket, error))?;
    fs::set_permissions(socket, Permissions::from_mode(0o600))
        .map_err(|error| Error::path("secure", socket, error))?;
    Ok(BoundListener {
        listener,
        _lock: lock,
    })
}

/// Serve the app on a Unix socket until the process exits. Every client gets
/// the hello line, its responses, and every `AppEvent`. Output per client is
/// queued and bounded; concurrent requests are capped.
pub fn serve(app: Arc<App>, socket: &Path) -> Result<()> {
    let bound = bind_listener(socket)?;
    let server = Arc::new(Server::new(app));
    let clients = server.clients.clone();
    server
        .app
        .set_event_sink(move |event| clients.broadcast(line_of(json!({ "event": event }))));

    loop {
        match bound.listener.accept() {
            Ok((stream, _)) => {
                if let Err(error) = server.accept(stream) {
                    cabin_diagnostics::event(
                        cabin_diagnostics::Level::Warn,
                        "rpc",
                        "client_worker_spawn_failed",
                        format_args!("{error}"),
                    );
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::ConnectionReset
                ) => {}
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::EMFILE | libc::ENFILE | libc::ENOBUFS | libc::ENOMEM)
                ) =>
            {
                // Open descriptors may become available as existing clients disconnect.
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(Error::path("accept RPC client on", socket, error)),
        }
    }
}

/// Bridge stdin/stdout to the daemon socket (`cabind connect`).
pub fn connect(socket: &Path) -> Result<()> {
    let stream = UnixStream::connect(socket).map_err(|error| {
        Error::Unavailable(format!(
            "cabind is not running ({}: {error})",
            socket.display()
        ))
    })?;
    let to_daemon = stream
        .try_clone()
        .map_err(|error| Error::io("clone socket", error))?;
    // Poll an unbuffered handle to stdin so its readiness matches the bytes available.
    let stdin = io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
        .map_err(|error| Error::io("duplicate RPC input", error))?;
    let (done, notify) =
        UnixStream::pair().map_err(|error| Error::io("open RPC output notification", error))?;
    let reader = thread::Builder::new()
        .name("rpc-connect-output".into())
        .spawn(move || {
            let _notify = notify;
            let close = CloseSocket(Arc::new(stream));
            let mut stdout = io::stdout().lock();
            io::copy(&mut &*close.0, &mut stdout)?;
            stdout.flush()
        })
        .map_err(|error| Error::io("start the RPC output worker", error))?;
    let input = copy_input(stdin, &to_daemon, &done);
    let _ = to_daemon.shutdown(if input.is_ok() {
        Shutdown::Write
    } else {
        Shutdown::Both
    });
    let output = reader
        .join()
        .map_err(|_| Error::Internal("the RPC output worker stopped unexpectedly".into()))?;
    output.map_err(|error| Error::io("copy RPC output", error))?;
    input.map_err(|error| Error::io("copy RPC input", error))?;
    Ok(())
}

fn copy_input(
    mut input: impl Read + AsRawFd,
    mut socket: &UnixStream,
    done: &UnixStream,
) -> io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: done.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let mut buffer = [0; 8192];
    loop {
        // SAFETY: both descriptors stay open for the call and the array has two entries.
        if unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) } < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if fds[0].revents != 0 {
            return Ok(());
        }
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => socket.write_all(&buffer[..count])?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::tests::app;
    use std::io::{Cursor, Seek};
    use std::time::Instant;

    const TIMEOUT: Duration = Duration::from_secs(5);

    #[test]
    fn request_line_limit_keeps_framing_and_rejects_an_unterminated_oversize_line() -> io::Result<()>
    {
        let mut reader = BufReader::with_capacity(2, Cursor::new(b"123\nxy\r\nz"));
        assert_eq!(read_request_line(&mut reader, 4)?.as_deref(), Some("123\n"));
        assert_eq!(
            read_request_line(&mut reader, 4)?.as_deref(),
            Some("xy\r\n")
        );
        assert_eq!(read_request_line(&mut reader, 4)?.as_deref(), Some("z"));
        assert!(read_request_line(&mut reader, 4)?.is_none());

        let mut reader = BufReader::with_capacity(2, Cursor::new(b"123456789"));
        assert_eq!(
            read_request_line(&mut reader, 4).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(reader.stream_position()?, 5);
        Ok(())
    }

    #[test]
    fn stalled_output_closes_the_socket_without_blocking_other_clients() -> Result<()> {
        let (socket, mut peer) = UnixStream::pair()?;
        let socket = Arc::new(socket);
        let (out, lines) = Out::new(1, &socket, 1);
        let (healthy_socket, _peer) = UnixStream::pair()?;
        let healthy_socket = Arc::new(healthy_socket);
        let (healthy, healthy_lines) = Out::new(2, &healthy_socket, 2);
        let clients = Clients::default();
        let _stalled_subscription = clients.register(&out);
        let _healthy_subscription = clients.register(&healthy);

        clients.broadcast("event\n".into());
        assert!(!out.send("response\n".into()));
        peer.set_read_timeout(Some(TIMEOUT))?;
        assert_eq!(peer.read(&mut [0])?, 0);
        assert_eq!(&*lines.try_recv().unwrap(), "event\n");
        assert_eq!(lines.try_recv(), Err(mpsc::TryRecvError::Disconnected));

        clients.broadcast("next event\n".into());
        assert_eq!(&*healthy_lines.try_recv().unwrap(), "event\n");
        assert_eq!(&*healthy_lines.try_recv().unwrap(), "next event\n");
        Ok(())
    }

    #[test]
    fn request_overload_replies_with_the_id_and_keeps_serving() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let server = Arc::new(Server::new(app(dir.path())));
        let all_slots = server
            .request_slots
            .clone()
            .try_acquire_many_owned(MAX_INFLIGHT as u32)
            .unwrap();
        let (stream, mut peer) = UnixStream::pair()?;
        peer.set_read_timeout(Some(TIMEOUT))?;
        assert!(server.accept(stream)?);
        let mut lines = BufReader::new(peer.try_clone()?).lines();
        assert_eq!(next_message(&mut lines)["hello"]["protocol"], PROTOCOL);
        writeln!(
            peer,
            r#"{{"id":"busy","method":"clock","params":{{"nested":[1,2]}}}}"#
        )?;
        let busy = next_message(&mut lines);
        assert_eq!(busy["id"], "busy");
        assert_eq!(busy["error"]["code"], "unavailable");
        drop(all_slots);

        let large_budget = server
            .request_bytes
            .clone()
            .try_acquire_many_owned(LARGE_REQUEST_BYTES as u32)
            .unwrap();
        writeln!(
            peer,
            r#"{{"id":1,"method":"clock","params":"{}"}}"#,
            "a".repeat(SMALL_REQUEST)
        )?;
        let busy = next_message(&mut lines);
        assert_eq!(busy["id"], 1);
        assert_eq!(busy["error"]["code"], "unavailable");
        writeln!(peer, r#"{{"id":2,"method":"clock"}}"#)?;
        peer.shutdown(Shutdown::Write)?;
        let clock = next_message(&mut lines);
        assert_eq!(clock["id"], 2);
        assert!(clock["result"]["nowMs"].is_number());
        assert!(lines.next().is_none());
        drop(large_budget);
        Ok(())
    }

    #[test]
    fn client_slots_cover_both_workers_and_are_reusable() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let server = Arc::new(Server::new(app(dir.path())));
        let mut admitted = Vec::new();
        for _ in 0..MAX_CLIENTS {
            let (stream, peer) = UnixStream::pair()?;
            assert!(server.accept(stream)?);
            admitted.push(peer);
        }
        let (stream, mut rejected) = UnixStream::pair()?;
        rejected.set_read_timeout(Some(TIMEOUT))?;
        assert!(!server.accept(stream)?);
        assert_eq!(rejected.read(&mut [0])?, 0, "no hello writer was started");

        drop(admitted.pop());
        let start = Instant::now();
        while server.client_slots.available_permits() == 0 {
            assert!(start.elapsed() < TIMEOUT, "client slot was not released");
            thread::sleep(Duration::from_millis(5));
        }
        let (stream, newcomer) = UnixStream::pair()?;
        newcomer.set_read_timeout(Some(TIMEOUT))?;
        assert!(server.accept(stream)?);
        assert_eq!(
            next_message(&mut BufReader::new(newcomer).lines())["hello"]["protocol"],
            PROTOCOL
        );
        Ok(())
    }

    #[test]
    fn listener_preserves_regular_files_symlinks_and_live_sockets() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("agentd.sock");
        fs::write(&socket, "keep")?;
        assert!(bind_listener(&socket).is_err());
        assert_eq!(fs::read_to_string(&socket)?, "keep");
        fs::remove_file(&socket)?;

        let target = dir.path().join("target");
        fs::write(&target, "keep")?;
        std::os::unix::fs::symlink(&target, &socket)?;
        assert!(bind_listener(&socket).is_err());
        assert_eq!(fs::read_link(&socket)?, target);
        fs::remove_file(&socket)?;

        let legacy = UnixListener::bind(&socket)?;
        assert!(bind_listener(&socket).is_err());
        UnixStream::connect(&socket)?;
        drop(legacy);

        let _bound = bind_listener(&socket)?;
        assert!(bind_listener(&socket).is_err());
        assert_eq!(fs::metadata(&socket)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn bridge_input_exits_when_output_closes_even_if_input_stays_open() -> io::Result<()> {
        let (input, mut sender) = UnixStream::pair()?;
        let (output, mut receiver) = UnixStream::pair()?;
        let (done, notify) = UnixStream::pair()?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || tx.send(copy_input(input, &output, &done)));
        sender.write_all(b"request\n")?;
        let mut data = [0; 8];
        receiver.set_read_timeout(Some(TIMEOUT))?;
        receiver.read_exact(&mut data)?;
        assert_eq!(&data, b"request\n");

        drop(notify);
        rx.recv_timeout(TIMEOUT).unwrap()?;
        Ok(())
    }

    fn next_message(lines: &mut impl Iterator<Item = io::Result<String>>) -> Value {
        serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap()
    }
}
