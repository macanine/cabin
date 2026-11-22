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
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr};

use crate::Result;
use crate::compat::{POLLIN, POLLOUT, PollItem, Pollable, WakeSocket, poll, poll_in};
use crate::netgw::{GATEWAY_IP, GATEWAY_MAC, GUEST_IP, MTU};

const GUEST_SSH: u16 = 22;
const BUFFER: usize = 256 * 1024;
const CHUNK: usize = 16 * 1024;

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
    handle: SocketHandle,
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
    frames: Sender<Vec<u8>>,
    wake: WakeSocket,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SshForward {
    /// Listen on `listen` and relay to the guest. `send` delivers Ethernet
    /// frames to the guest.
    pub fn start(listen: SocketAddr, send: impl Fn(&[u8]) + Send + 'static) -> Result<Self> {
        let listener = TcpListener::bind(listen)?;
        let local = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let (wake, wake_rx) = crate::compat::wake_pair()?;
        wake.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        let (frames, inbox) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("vm-ssh-forward".into())
                .spawn(move || run(listener, inbox, wake_rx, stop, send))?
        };
        Ok(Self {
            local,
            frames,
            wake,
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
        if self.frames.send(frame).is_ok() {
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
    wake: WakeSocket,
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
    let mut chunk = vec![0u8; CHUNK];

    while !stop.load(Ordering::Relaxed) {
        // Drain wake-ups and queued frames from the guest.
        let mut byte = [0u8; 64];
        while wake.recv(&mut byte).is_ok() {}
        while let Ok(frame) = inbox.try_recv() {
            link.rx.push_back(frame);
        }

        // New Mac-side connections.
        while let Ok((host, _)) = listener.accept() {
            if host.set_nonblocking(true).is_err() {
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
            relays.push(Relay {
                handle: sockets.add(socket),
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
            send(&frame);
        }

        // Retire finished relays.
        relays.retain(|relay| {
            let socket = sockets.get::<tcp::Socket>(relay.handle);
            let done = relay.failed
                || (!socket.is_open() && relay.to_host.is_empty())
                || (relay.host_shut && relay.host_eof && !socket.is_active());
            if done {
                let _ = relay.host.shutdown(Shutdown::Both);
                sockets.remove(relay.handle);
            }
            !done
        });

        wait(
            &listener,
            &wake,
            &relays,
            &sockets,
            iface.poll_delay(now(), &sockets),
        );
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
    listener: &TcpListener,
    wake: &WakeSocket,
    relays: &[Relay],
    sockets: &SocketSet<'_>,
    delay: Option<smoltcp::time::Duration>,
) {
    let mut fds = vec![poll_in(listener), poll_in(wake)];
    for relay in relays {
        let socket = sockets.get::<tcp::Socket>(relay.handle);
        let mut events = 0;
        if !relay.host_eof && relay.to_guest.is_empty() && socket.may_send() {
            events |= POLLIN;
        }
        if !relay.to_host.is_empty() {
            events |= POLLOUT;
        }
        fds.push(PollItem {
            sock: relay.host.poll_sock(),
            events,
            revents: 0,
        });
    }
    let cap = Duration::from_millis(250);
    let timeout = delay
        .map(|d| Duration::from_micros(d.total_micros()).min(cap))
        .unwrap_or(cap);
    let _ = poll(&mut fds, timeout.as_millis() as i32);
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
