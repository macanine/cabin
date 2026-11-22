use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

#[cfg(unix)]
use std::os::{
    fd::{AsRawFd, RawFd},
    unix::net::UnixStream,
};

#[cfg(unix)]
/// The packet-carrying device handle the stack waits on: a TUN fd on Unix.
pub(super) type TunDescriptor = RawFd;

#[cfg(windows)]
/// The packet-carrying device handle on Windows: one half of a loopback UDP
/// pair owned by the host gateway (Cabin addition, see VENDORED.txt).
pub(super) type TunDescriptor = std::sync::Arc<std::net::UdpSocket>;

/// One half of the wake-up channel (Unix socketpair or loopback UDP pair).
#[cfg(unix)]
type WakeSocket = UnixStream;
#[cfg(windows)]
type WakeSocket = std::net::UdpSocket;

#[derive(Clone)]
pub(super) struct Wake(Arc<WakeInner>);

struct WakeInner {
    writer: WakeSocket,
    pending: AtomicBool,
}

pub(super) struct WakeReceiver {
    reader: WakeSocket,
    wake: Wake,
}

impl Wake {
    pub fn new() -> io::Result<(Self, WakeReceiver)> {
        let (reader, writer) = wake_sockets()?;
        let wake = Self(Arc::new(WakeInner {
            writer,
            pending: AtomicBool::new(false),
        }));
        let receiver = WakeReceiver {
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
            match wake_write(&self.0.writer) {
                Ok(_) => return,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                // A full socket is already readable. Producers may outlive the receiver.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::BrokenPipe
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

/// One wake byte into the channel's writer half (write on Unix, send on Windows).
fn wake_write(writer: &WakeSocket) -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut writer = writer;
        writer.write(&[1]).map(|_| ())
    }
    #[cfg(windows)]
    {
        writer.send(&[1]).map(|_| ())
    }
}

/// Drain all pending wake bytes (Unix: stream reads; Windows: one datagram each).
fn wake_drain_all(reader: &mut WakeSocket) -> io::Result<bool> {
    let mut any = false;
    loop {
        let result = {
            #[cfg(unix)]
            {
                reader.read(&mut [0; 128])
            }
            #[cfg(windows)]
            {
                reader.recv(&mut [0; 128])
            }
        };
        match result {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(_) => any = true,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error),
        }
    }
    Ok(any)
}

impl WakeReceiver {
    /// Acknowledges notifications before the caller consumes shared work, never before waiting.
    pub fn drain(&mut self) -> io::Result<()> {
        wake_drain_all(&mut self.reader)?;
        self.wake.0.pending.swap(false, Ordering::AcqRel);
        Ok(())
    }

    #[cfg(unix)]
    pub fn wait(&self, tun: RawFd, delay: Option<Duration>) -> io::Result<bool> {
        let mut fds = [
            libc::pollfd {
                fd: tun,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let timeout = delay.map_or(-1, |delay| {
            delay
                .as_nanos()
                .div_ceil(1_000_000)
                .min(libc::c_int::MAX as u128) as libc::c_int
        });
        let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            if fd.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBADF));
            }
            if fd.revents & libc::POLLERR != 0 {
                return Err(io::Error::from_raw_os_error(libc::EIO));
            }
            if fd.revents & libc::POLLHUP != 0 && fd.revents & libc::POLLIN == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        Ok(result > 0)
    }

    /// Windows waits on the packet socket and the wake socket with WSAPoll.
    #[cfg(windows)]
    pub fn wait(&self, tun: &TunDescriptor, delay: Option<Duration>) -> io::Result<bool> {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{WSAPOLLFD, WSAPoll};
        const WIN_POLLIN: i16 = 0x0300; // POLLRDNORM | POLLRDBAND
        const WIN_POLLERR: i16 = 0x8;
        const WIN_POLLHUP: i16 = 0x10;
        const WIN_POLLNVAL: i16 = 0x20;
        let mut fds = [
            WSAPOLLFD {
                fd: tun.as_raw_socket() as usize,
                events: WIN_POLLIN,
                revents: 0,
            },
            WSAPOLLFD {
                fd: self.reader.as_raw_socket() as usize,
                events: WIN_POLLIN,
                revents: 0,
            },
        ];
        let timeout = delay.map_or(-1, |delay| {
            delay
                .as_nanos()
                .div_ceil(1_000_000)
                .min(i32::MAX as u128) as i32
        });
        let result = unsafe { WSAPoll(fds.as_mut_ptr(), fds.len() as u32, timeout) };
        if result < 0 {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        for fd in fds {
            if fd.revents & WIN_POLLNVAL != 0 {
                return Err(io::Error::from(io::ErrorKind::InvalidInput));
            }
            if fd.revents & WIN_POLLERR != 0 {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            if fd.revents & WIN_POLLHUP != 0 && fd.revents & WIN_POLLIN == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
        }
        Ok(result > 0)
    }
}

fn wake_sockets() -> io::Result<(WakeSocket, WakeSocket)> {
    #[cfg(unix)]
    {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok((reader, writer))
    }
    #[cfg(windows)]
    {
        use std::net::{Ipv4Addr, SocketAddr};
        let first = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        let second = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
        let first_addr: SocketAddr = first.local_addr()?;
        let second_addr: SocketAddr = second.local_addr()?;
        first.connect(second_addr)?;
        second.connect(first_addr)?;
        first.set_nonblocking(true)?;
        second.set_nonblocking(true)?;
        Ok((first, second))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    #[test]
    fn notifications_coalesce_and_survive_before_wait() {
        let (_peer, tun) = UnixStream::pair().unwrap();
        let (wake, mut receiver) = Wake::new().unwrap();
        for _ in 0..10_000 {
            wake.notify();
        }
        assert!(
            receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );

        // The receiver stays quiet once the notification is consumed.
        assert!(
            !receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );
        wake.notify();
        assert!(
            receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );
        let mut bytes = [0; 128];
        assert_eq!(receiver.reader.read(&mut bytes).unwrap(), 1);
        receiver.drain().unwrap();
        assert!(
            !receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );
        wake.notify();
        assert!(
            receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );
    }

    #[test]
    fn notification_interrupts_an_indefinite_wait() {
        let (mut peer, tun) = UnixStream::pair().unwrap();
        let (wake, receiver) = Wake::new().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            ready_tx.send(()).unwrap();
            let result = receiver.wait(tun.as_raw_fd(), None);
            done_tx.send(result).unwrap();
        });
        ready_rx.recv().unwrap();
        wake.notify();
        let result = done_rx.recv_timeout(Duration::from_secs(1));
        if result.is_err() {
            let _ = peer.write(&[1]);
        }
        waiter.join().unwrap();
        assert!(result.unwrap().unwrap());
    }

    #[test]
    fn full_wake_socket_is_already_a_notification() {
        let (_peer, tun) = UnixStream::pair().unwrap();
        let (wake, mut receiver) = Wake::new().unwrap();
        let bytes = [0; 4096];
        loop {
            match (&wake.0.writer).write(&bytes) {
                Ok(written) => assert!(written > 0),
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
                    break;
                }
            }
        }
        wake.notify();
        assert!(
            receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );
        receiver.drain().unwrap();
        assert!(
            !receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );
        wake.notify();
        assert!(
            receiver
                .wait(tun.as_raw_fd(), Some(Duration::ZERO))
                .unwrap()
        );
    }

    #[test]
    fn late_producer_does_not_raise_sigpipe() {
        let (wake, receiver) = Wake::new().unwrap();
        drop(receiver);
        wake.notify();
        wake.notify();
    }

    #[test]
    fn notification_during_acknowledgment_is_observed_as_shared_work() {
        let (wake, mut receiver) = Wake::new().unwrap();
        let (tx, rx) = mpsc::channel();
        wake.notify();
        assert_eq!(receiver.reader.read(&mut [0]).unwrap(), 1);
        tx.send(42).unwrap();
        wake.notify();
        receiver.drain().unwrap();
        assert_eq!(rx.try_recv().unwrap(), 42);
        wake.notify();
        assert_eq!(receiver.reader.read(&mut [0]).unwrap(), 1);
    }

    #[test]
    fn invalid_descriptor_is_not_reported_as_readiness() {
        let (_, receiver) = Wake::new().unwrap();
        assert_eq!(
            receiver
                .wait(libc::c_int::MAX, Some(Duration::ZERO))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EBADF)
        );
    }
}
