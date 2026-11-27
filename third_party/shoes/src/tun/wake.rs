use std::{
    io,
    net::{Ipv4Addr, UdpSocket},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use polling::{Event, Events, PollMode, Poller};

const WAKE_KEY: usize = 0;
const DEVICE_KEY: usize = 1;

/// A connected pair of non-blocking loopback UDP sockets. The host gateway and
/// the stack exchange IP packets over one, and `Wake` signals over another.
/// `buffer` sizes the send and receive buffers (the kernel may cap it).
pub fn loopback_udp_pair(buffer: Option<usize>) -> io::Result<(UdpSocket, UdpSocket)> {
    let first = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let second = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    first.connect(second.local_addr()?)?;
    second.connect(first.local_addr()?)?;
    for socket in [&first, &second] {
        socket.set_nonblocking(true)?;
        if let Some(size) = buffer {
            let socket = socket2::SockRef::from(socket);
            let _ = socket.set_recv_buffer_size(size);
            let _ = socket.set_send_buffer_size(size);
        }
        #[cfg(windows)]
        disable_connection_reset(socket)?;
    }
    Ok((first, second))
}

/// Windows reports an ICMP port-unreachable from a closed peer as a recv error
/// (WSAECONNRESET) on UDP sockets; the pair treats a closed peer as silence.
#[cfg(windows)]
fn disable_connection_reset(socket: &UdpSocket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{SIO_UDP_CONNRESET, WSAIoctl};
    let off: u32 = 0;
    let mut returned = 0;
    let status = unsafe {
        WSAIoctl(
            socket.as_raw_socket() as usize,
            SIO_UDP_CONNRESET,
            (&off as *const u32).cast(),
            std::mem::size_of::<u32>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        )
    };
    if status == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

#[derive(Clone)]
pub(super) struct Wake(Arc<WakeInner>);

struct WakeInner {
    writer: UdpSocket,
    pending: AtomicBool,
}

pub(super) struct WakeReceiver {
    // Dropped first: the poller must not outlive the sockets it watches.
    poller: Poller,
    events: Events,
    device_watched: bool,
    reader: UdpSocket,
    wake: Wake,
}

impl Wake {
    pub fn new() -> io::Result<(Self, WakeReceiver)> {
        let (reader, writer) = loopback_udp_pair(None)?;
        let wake = Self(Arc::new(WakeInner {
            writer,
            pending: AtomicBool::new(false),
        }));
        let poller = Poller::new()?;
        // SAFETY: `reader` lives in the same struct, after the poller.
        unsafe {
            poller.add_with_mode(
                &reader,
                Event::readable(WAKE_KEY),
                PollMode::Level,
            )?;
        }
        let receiver = WakeReceiver {
            poller,
            events: Events::new(),
            device_watched: false,
            reader,
            wake: wake.clone(),
        };
        Ok((wake, receiver))
    }

    pub fn notify(&self) {
        if self.0.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        loop {
            match self.0.writer.send(&[1]) {
                Ok(_) => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                // A full socket is already readable. Producers may outlive the receiver.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    return;
                }
                Err(error) => {
                    log::warn!("Failed to wake TUN stack: {error}");
                    return;
                }
            }
        }
    }
}

impl WakeReceiver {
    /// Acknowledges notifications before the caller consumes shared work, never before waiting.
    pub fn drain(&mut self) -> io::Result<()> {
        loop {
            match self.reader.recv(&mut [0; 128]) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        self.wake.0.pending.swap(false, Ordering::AcqRel);
        Ok(())
    }

    /// Waits until `device` has a packet or a notification arrives. `device`
    /// must be the same socket on every call and outlive this receiver.
    pub fn wait(&mut self, device: &UdpSocket, delay: Option<Duration>) -> io::Result<bool> {
        if !self.device_watched {
            // SAFETY: the stack thread owns `device` for as long as it owns `self`.
            unsafe {
                self.poller.add_with_mode(
                    device,
                    Event::readable(DEVICE_KEY),
                    PollMode::Level,
                )?;
            }
            self.device_watched = true;
        }
        self.events.clear();
        Ok(self.poller.wait(&mut self.events, delay)? > 0)
    }

    /// Waits only for wake notifications while the relay gate is suspended,
    /// avoiding a level-triggered busy loop if any stray datagram reaches `device`.
    pub fn wait_suspended(&mut self, device: &UdpSocket, delay: Option<Duration>) -> io::Result<bool> {
        if self.device_watched {
            self.poller.delete(device)?;
            self.device_watched = false;
        }
        self.events.clear();
        Ok(self.poller.wait(&mut self.events, delay)? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    fn device() -> (UdpSocket, UdpSocket) {
        loopback_udp_pair(None).unwrap()
    }

    #[test]
    fn notifications_coalesce_and_survive_before_wait() {
        let (_peer, tun) = device();
        let (wake, mut receiver) = Wake::new().unwrap();
        for _ in 0..10_000 {
            wake.notify();
        }
        assert!(receiver.wait(&tun, Some(Duration::from_secs(1))).unwrap());

        // The receiver stays quiet once the notification is consumed.
        receiver.drain().unwrap();
        assert!(!receiver.wait(&tun, Some(Duration::ZERO)).unwrap());
        wake.notify();
        assert!(receiver.wait(&tun, Some(Duration::from_secs(1))).unwrap());
        receiver.drain().unwrap();
        assert!(!receiver.wait(&tun, Some(Duration::ZERO)).unwrap());
    }

    #[test]
    fn notification_interrupts_an_indefinite_wait() {
        let (peer, tun) = device();
        let (wake, mut receiver) = Wake::new().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let result = receiver.wait(&tun, None);
            done_tx.send(result).unwrap();
        });
        ready_rx.recv().unwrap();
        wake.notify();
        let result = done_rx.recv_timeout(Duration::from_secs(1));
        if result.is_err() {
            let _ = peer.send(&[1]);
        }
        waiter.join().unwrap();
        assert!(result.unwrap().unwrap());
    }

    #[test]
    fn device_traffic_wakes_the_wait() {
        let (peer, tun) = device();
        let (_wake, mut receiver) = Wake::new().unwrap();
        assert!(!receiver.wait(&tun, Some(Duration::ZERO)).unwrap());
        peer.send(&[1]).unwrap();
        assert!(receiver.wait(&tun, Some(Duration::from_secs(1))).unwrap());
    }

    #[test]
    fn late_producer_does_not_fail() {
        let (wake, receiver) = Wake::new().unwrap();
        drop(receiver);
        wake.notify();
        wake.notify();
    }

    fn recv_byte(socket: &UdpSocket) -> usize {
        loop {
            match socket.recv(&mut [0]) {
                Ok(received) => return received,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => thread::yield_now(),
                Err(error) => panic!("{error}"),
            }
        }
    }

    #[test]
    fn notification_during_acknowledgment_is_observed_as_shared_work() {
        let (wake, mut receiver) = Wake::new().unwrap();
        let (tx, rx) = mpsc::channel();
        wake.notify();
        assert_eq!(recv_byte(&receiver.reader), 1);
        tx.send(42).unwrap();
        wake.notify();
        receiver.drain().unwrap();
        assert_eq!(rx.try_recv().unwrap(), 42);
        wake.notify();
        assert_eq!(recv_byte(&receiver.reader), 1);
    }
}
