//! SSH into the VM over its only NIC.
//!
//! The VM has one network interface, wired to the Mac gateway (`netgw`).
//! The gateway also owns 10.0.3.1 on that link: this module runs a small
//! TCP stack (smoltcp) for that address and relays every connection accepted
//! on `127.0.0.1:<ssh port>` to the guest's port 22. Frames the guest sends
//! to the gateway's TCP endpoint (and its ARP replies) arrive here; frames
//! this stack produces go back to the guest through `send`.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use polling::{AsSource, Event, Events, PollMode, Poller};
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr};

use crate::Result;
use crate::netgw::{GATEWAY_IP, GATEWAY_MAC, GUEST_IP, MTU};
use crate::transport::wake_pair;

const GUEST_SSH: u16 = 22;
const BUFFER: usize = 256 * 1024;
const CHUNK: usize = 16 * 1024;
const MAX_RELAYS: usize = 128;
const MAX_QUEUED_FRAMES: usize = 2048;
const FRAME_BATCH: usize = 256;
const CONNECT_TIMEOUT: smoltcp::time::Duration = smoltcp::time::Duration::from_secs(30);

/// Frames in from the guest, frames out to the guest.
struct Link {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
}

struct Rx(Vec<u8>);
struct Tx<'a>(&'a mut Vec<Vec<u8>>);

impl RxToken for Rx {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}

impl TxToken for Tx<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let mut frame = vec![0; len];
        let result = f(&mut frame);
        self.0.push(frame);
        result
    }
}

impl Device for Link {
    type RxToken<'a> = Rx;
    type TxToken<'a> = Tx<'a>;

    fn receive(&mut self, _now: Instant) -> Option<(Rx, Tx<'_>)> {
        let frame = self.rx.pop_front()?;
        Some((Rx(frame), Tx(&mut self.tx)))
    }

    fn transmit(&mut self, _now: Instant) -> Option<Tx<'_>> {
        Some(Tx(&mut self.tx))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = MTU + 14;
        caps
    }
}

/// One relayed connection: a Mac-side TCP stream and a stack socket.
struct Relay {
    key: usize,
    interest: (bool, bool),
    handle: SocketHandle,
    connect_deadline: Instant,
    host: TcpStream,
    /// Bytes from the guest not yet written to the Mac side.
    to_host: Vec<u8>,
    /// Bytes from the Mac side not yet accepted by the stack.
    to_guest: Vec<u8>,
    host_eof: bool,
    host_shut: bool,
    failed: bool,
}

/// Handle to the relay thread; stops it when dropped.
pub struct SshForward {
    local: SocketAddr,
    frames: SyncSender<Vec<u8>>,
    wake: UdpSocket,
    wake_pending: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

fn port_unavailable(error: &std::io::Error) -> bool {
    // Winsock can report an excluded port as WSAEACCES instead of WSAEADDRINUSE.
    error.kind() == ErrorKind::AddrInUse || (cfg!(windows) && error.raw_os_error() == Some(10013))
}

impl SshForward {
    /// Start at the preferred port and keep the first available listener.
    /// Port zero asks the OS to choose a port, as before.
    pub fn start(mut listen: SocketAddr, send: impl Fn(&[u8]) + Send + 'static) -> Result<Self> {
        let listener = loop {
            match TcpListener::bind(listen) {
                Ok(listener) => break listener,
                Err(error) if listen.port() != 0 && port_unavailable(&error) => {
                    let Some(next) = listen.port().checked_add(1) else {
                        return Err(error.into());
                    };
                    listen.set_port(next);
                }
                Err(error) => return Err(error.into()),
            }
        };
        let local = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let (wake, wake_rx) = wake_pair()?;
        wake.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        let (frames, inbox) = mpsc::sync_channel(MAX_QUEUED_FRAMES);
        let wake_pending = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            let wake_pending = wake_pending.clone();
            std::thread::Builder::new()
                .name("vm-ssh-forward".into())
                .spawn(move || run(listener, inbox, wake_rx, wake_pending, stop, send))?
        };
        Ok(Self {
            local,
            frames,
            wake,
            wake_pending,
            stop,
            thread: Some(thread),
        })
    }

    /// Where the Mac side connects.
    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    /// A frame from the guest for the gateway's own TCP endpoint.
    pub fn deliver(&self, frame: Vec<u8>) {
        if self.frames.try_send(frame).is_ok() && !self.wake_pending.swap(true, Ordering::AcqRel) {
            let _ = self.wake.send(&[1]);
        }
    }
}

impl Drop for SshForward {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.send(&[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn now() -> Instant {
    Instant::now()
}

fn run(
    listener: TcpListener,
    inbox: Receiver<Vec<u8>>,
    wake: UdpSocket,
    wake_pending: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    send: impl Fn(&[u8]),
) {
    let mut link = Link {
        rx: VecDeque::new(),
        tx: Vec::new(),
    };
    let mut config = Config::new(HardwareAddress::Ethernet(EthernetAddress(GATEWAY_MAC)));
    config.random_seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let mut iface = Interface::new(config, &mut link, now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(GATEWAY_IP), 24));
    });
    let mut sockets = SocketSet::new(Vec::new());
    let mut relays: Vec<Relay> = Vec::new();
    let mut next_port: u16 = 49152;
    let mut next_key: usize = 2;
    let mut chunk = vec![0u8; CHUNK];
    let poller = Poller::new().ok();
    let mut events = Events::new();
    if let Some(poller) = &poller {
        // SAFETY: `listener` and `wake` outlive the poller and are deleted on exit.
        unsafe {
            let _ = poller.add_with_mode(
                &listener.source(),
                Event::new(0, true, false),
                PollMode::Level,
            );
            let _ =
                poller.add_with_mode(&wake.source(), Event::new(1, true, false), PollMode::Level);
        }
    }

    while !stop.load(Ordering::Relaxed) {
        // Clear the coalesced wake flag before draining inbox so any concurrent
        // `deliver()` either lands in this batch or posts a fresh wake byte.
        wake_pending.store(false, Ordering::Release);
        let mut byte = [0u8; 64];
        while wake.recv(&mut byte).is_ok() {}
        let mut drained = 0usize;
        for frame in inbox.try_iter().take(FRAME_BATCH) {
            link.rx.push_back(frame);
            drained += 1;
        }

        // New Mac-side connections.
        for _ in 0..MAX_RELAYS {
            let Ok((host, _)) = listener.accept() else {
                break;
            };
            if relays.len() >= MAX_RELAYS || host.set_nonblocking(true).is_err() {
                continue;
            }
            let _ = host.set_nodelay(true);
            let mut socket = tcp::Socket::new(
                tcp::SocketBuffer::new(vec![0; BUFFER]),
                tcp::SocketBuffer::new(vec![0; BUFFER]),
            );
            next_port = if next_port >= 65000 {
                49152
            } else {
                next_port + 1
            };
            if socket
                .connect(
                    iface.context(),
                    (IpAddress::Ipv4(GUEST_IP), GUEST_SSH),
                    next_port,
                )
                .is_err()
            {
                continue;
            }
            let key = next_key;
            next_key = next_key.wrapping_add(1).max(2);
            let interest = (false, false);
            if let Some(poller) = &poller {
                // SAFETY: `relay.host` is deleted from `poller` before being dropped.
                let added = unsafe {
                    poller.add_with_mode(
                        &host.source(),
                        Event::new(key, false, false),
                        PollMode::Level,
                    )
                };
                if added.is_err() {
                    continue;
                }
            }
            relays.push(Relay {
                key,
                interest,
                handle: sockets.add(socket),
                connect_deadline: now() + CONNECT_TIMEOUT,
                host,
                to_host: Vec::new(),
                to_guest: Vec::new(),
                host_eof: false,
                host_shut: false,
                failed: false,
            });
        }

        iface.poll(now(), &mut link, &mut sockets);
        for relay in &mut relays {
            pump(
                relay,
                sockets.get_mut::<tcp::Socket>(relay.handle),
                &mut chunk,
            );
        }
        iface.poll(now(), &mut link, &mut sockets);
        for frame in link.tx.drain(..) {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            send(&frame);
        }

        // Retire finished relays.
        let at = now();
        relays.retain(|relay| {
            let socket = sockets.get::<tcp::Socket>(relay.handle);
            let done = relay.failed
                || (socket.state() == tcp::State::SynSent && at >= relay.connect_deadline)
                || (!socket.is_open() && relay.to_host.is_empty())
                || (relay.host_shut && relay.host_eof && !socket.is_active());
            if done {
                if let Some(poller) = &poller {
                    let _ = poller.delete(relay.host.source());
                }
                let _ = relay.host.shutdown(Shutdown::Both);
                sockets.remove(relay.handle);
            }
            !done
        });

        if drained == FRAME_BATCH {
            // More guest frames may still be queued; keep pumping without sleeping.
            continue;
        }

        wait(
            poller.as_ref(),
            &mut events,
            &mut relays,
            &sockets,
            iface.poll_delay(now(), &sockets),
        );
    }

    if let Some(poller) = &poller {
        for relay in &relays {
            let _ = poller.delete(relay.host.source());
        }
        let _ = poller.delete(listener.source());
        let _ = poller.delete(wake.source());
    }
}

/// Move bytes both ways for one connection without blocking.
fn pump(relay: &mut Relay, socket: &mut tcp::Socket, chunk: &mut [u8]) {
    // Guest → Mac.
    while socket.can_recv() && relay.to_host.len() < BUFFER {
        match socket.recv_slice(chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => relay.to_host.extend_from_slice(&chunk[..n]),
        }
    }
    while !relay.to_host.is_empty() {
        match relay.host.write(&relay.to_host) {
            Ok(0) => break,
            Ok(n) => {
                relay.to_host.drain(..n);
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => break,
            Err(_) => {
                relay.failed = true;
                socket.abort();
                return;
            }
        }
    }
    if !socket.may_recv()
        && relay.to_host.is_empty()
        && !relay.host_shut
        && socket.state() != tcp::State::SynSent
    {
        let _ = relay.host.shutdown(Shutdown::Write);
        relay.host_shut = true;
    }

    // Mac → guest.
    if !relay.host_eof && relay.to_guest.is_empty() && socket.may_send() {
        match relay.host.read(chunk) {
            Ok(0) => relay.host_eof = true,
            Ok(n) => relay.to_guest.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {}
            Err(_) => {
                relay.failed = true;
                socket.abort();
                return;
            }
        }
    }
    if !relay.to_guest.is_empty()
        && socket.can_send()
        && let Ok(n) = socket.send_slice(&relay.to_guest)
    {
        relay.to_guest.drain(..n);
    }
    if relay.host_eof && relay.to_guest.is_empty() {
        socket.close();
    }
}

/// Sleep until a socket is ready, a guest frame arrives or the stack has a
/// timer due (capped so a stop request is noticed).
fn wait(
    poller: Option<&Poller>,
    events: &mut Events,
    relays: &mut [Relay],
    sockets: &SocketSet<'_>,
    delay: Option<smoltcp::time::Duration>,
) {
    let cap = Duration::from_millis(250);
    let timeout = delay
        .map(|d| Duration::from_micros(d.total_micros()).min(cap))
        .unwrap_or(cap);
    let Some(poller) = poller else {
        std::thread::sleep(Duration::from_millis(5));
        return;
    };
    for relay in relays.iter_mut() {
        let socket = sockets.get::<tcp::Socket>(relay.handle);
        let read = !relay.host_eof && relay.to_guest.is_empty() && socket.may_send();
        let write = !relay.to_host.is_empty();
        if (read, write) != relay.interest
            && poller
                .modify_with_mode(
                    relay.host.source(),
                    Event::new(relay.key, read, write),
                    PollMode::Level,
                )
                .is_ok()
        {
            relay.interest = (read, write);
        }
    }
    events.clear();
    match poller.wait(events, Some(timeout)) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::Interrupted => {}
        Err(_) => std::thread::sleep(Duration::from_millis(5)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netgw::GUEST_MAC;
    use std::sync::Mutex;

    /// A guest TCP stack (10.0.3.15) whose port 22 echoes, wired to the relay.
    #[test]
    fn relays_a_connection_to_the_guest_port_22() {
        let to_guest = Arc::new(Mutex::new(VecDeque::<Vec<u8>>::new()));
        let forward = {
            let to_guest = to_guest.clone();
            SshForward::start("127.0.0.1:0".parse().unwrap(), move |frame| {
                to_guest.lock().unwrap().push_back(frame.to_vec());
            })
            .unwrap()
        };
        let addr = forward.local_addr();
        let stop = Arc::new(AtomicBool::new(false));
        let guest = {
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut link = Link {
                    rx: VecDeque::new(),
                    tx: Vec::new(),
                };
                let mut iface = Interface::new(
                    Config::new(HardwareAddress::Ethernet(EthernetAddress(GUEST_MAC))),
                    &mut link,
                    now(),
                );
                iface.update_ip_addrs(|addrs| {
                    let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(GUEST_IP), 24));
                });
                let mut sockets = SocketSet::new(Vec::new());
                let mut socket = tcp::Socket::new(
                    tcp::SocketBuffer::new(vec![0; 4096]),
                    tcp::SocketBuffer::new(vec![0; 4096]),
                );
                socket.listen(GUEST_SSH).unwrap();
                let handle = sockets.add(socket);
                let mut buf = [0u8; 512];
                while !stop.load(Ordering::Relaxed) {
                    link.rx.extend(to_guest.lock().unwrap().drain(..));
                    iface.poll(now(), &mut link, &mut sockets);
                    let socket = sockets.get_mut::<tcp::Socket>(handle);
                    if socket.can_recv() && socket.can_send() {
                        let n = socket.recv_slice(&mut buf).unwrap();
                        socket.send_slice(&buf[..n]).unwrap();
                    }
                    iface.poll(now(), &mut link, &mut sockets);
                    for frame in link.tx.drain(..) {
                        forward.deliver(frame);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            })
        };
        let mut host = TcpStream::connect(addr).unwrap();
        host.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        host.write_all(b"SSH-2.0-test\r\n").unwrap();
        let mut echoed = [0u8; 14];
        host.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"SSH-2.0-test\r\n");
        stop.store(true, Ordering::Relaxed);
        guest.join().unwrap();
    }
}
