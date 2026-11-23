//! Socket pairs and readiness polling: the one place the host transport differs
//! between Unix descriptors and Windows sockets.

#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::io;

#[cfg(unix)]
pub type WakeSocket = std::os::unix::net::UnixDatagram;
/// A connected datagram socket from [`wake_pair`] / [`packet_pair`].
#[cfg(windows)]
pub type WakeSocket = std::net::UdpSocket;

/// Connected datagram pair for cross-thread wake-ups (never blocks the sender).
pub fn wake_pair() -> io::Result<(WakeSocket, WakeSocket)> {
    #[cfg(unix)]
    {
        let (a, b) = std::os::unix::net::UnixDatagram::pair()?;
        a.set_nonblocking(true)?;
        b.set_nonblocking(true)?;
        Ok((a, b))
    }
    #[cfg(windows)]
    {
        pair_loopback_udp()
    }
}

/// Connected datagram pair carrying whole IP packets between the gateway and
/// the proxy stack. Unix uses a socketpair; Windows two loopback UDP sockets.
pub fn packet_pair() -> io::Result<(WakeSocket, WakeSocket)> {
    #[cfg(unix)]
    {
        let (a, b) = std::os::unix::net::UnixDatagram::pair()?;
        for socket in [&a, &b] {
            // macOS defaults to tiny Unix datagram buffers.
            let size: libc::c_int = 4 * 1024 * 1024;
            for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
                unsafe {
                    libc::setsockopt(
                        socket.as_raw_fd(),
                        libc::SOL_SOCKET,
                        option,
                        (&size as *const libc::c_int).cast(),
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                    );
                }
            }
        }
        a.set_nonblocking(true)?;
        b.set_nonblocking(true)?;
        Ok((a, b))
    }
    #[cfg(windows)]
    {
        pair_loopback_udp()
    }
}

#[cfg(windows)]
fn pair_loopback_udp() -> io::Result<(std::net::UdpSocket, std::net::UdpSocket)> {
    use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
    let first = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let second = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?;
    let first_addr: SocketAddr = first.local_addr()?;
    let second_addr: SocketAddr = second.local_addr()?;
    first.connect(second_addr)?;
    second.connect(first_addr)?;
    first.set_nonblocking(true)?;
    second.set_nonblocking(true)?;
    Ok((first, second))
}

// ---- readiness polling ----------------------------------------------------

/// Handle used by [`poll`]: a raw fd on Unix, a SOCKET on Windows.
#[cfg(unix)]
pub type PollSock = std::os::fd::RawFd;
#[cfg(windows)]
pub type PollSock = usize;

#[cfg(unix)]
pub const POLLIN: i16 = libc::POLLIN as i16;
#[cfg(unix)]
pub const POLLOUT: i16 = libc::POLLOUT as i16;
#[cfg(windows)]
pub const POLLIN: i16 = 0x0300; // POLLRDNORM | POLLRDBAND
#[cfg(windows)]
pub const POLLOUT: i16 = 0x0010; // POLLWRNORM

/// One polled socket. `events` requests readiness; `revents` reports it.
pub struct PollItem {
    pub sock: PollSock,
    pub events: i16,
    pub revents: i16,
}

/// Anything with an OS poll handle: fds on Unix, sockets on Windows.
pub trait Pollable {
    fn poll_sock(&self) -> PollSock;
}

#[cfg(unix)]
impl<T: std::os::fd::AsRawFd + ?Sized> Pollable for T {
    fn poll_sock(&self) -> PollSock {
        self.as_raw_fd()
    }
}

#[cfg(windows)]
impl<T: std::os::windows::io::AsRawSocket + ?Sized> Pollable for T {
    fn poll_sock(&self) -> PollSock {
        self.as_raw_socket() as usize
    }
}

/// Wait up to `timeout_ms` (-1: forever) for readiness. Returns the number of
/// ready entries; their `revents` carry the result (0 when not ready).
pub fn poll(items: &mut [PollItem], timeout_ms: i32) -> io::Result<usize> {
    #[cfg(unix)]
    {
        let mut fds: Vec<libc::pollfd> = items
            .iter()
            .map(|item| libc::pollfd {
                fd: item.sock,
                events: item.events,
                revents: item.revents,
            })
            .collect();
        let count = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if count < 0 {
            let error = io::Error::last_os_error();
            for (item, fd) in items.iter_mut().zip(fds.iter()) {
                item.revents = fd.revents;
            }
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(0);
            }
            return Err(error);
        }
        for (item, fd) in items.iter_mut().zip(fds.iter()) {
            item.revents = fd.revents;
        }
        Ok(count.max(0) as usize)
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{WSAPOLLFD, WSAPoll};
        let mut fds: Vec<WSAPOLLFD> = items
            .iter()
            .map(|item| WSAPOLLFD {
                fd: item.sock,
                events: item.events,
                revents: 0,
            })
            .collect();
        let count = unsafe { WSAPoll(fds.as_mut_ptr(), fds.len() as u32, timeout_ms) };
        if count < 0 {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        for (item, fd) in items.iter_mut().zip(fds.iter()) {
            item.revents = fd.revents;
        }
        Ok(count.max(0) as usize)
    }
}

/// A [`PollItem`] requesting POLLIN for a pollable socket.
pub fn poll_in(sock: &impl Pollable) -> PollItem {
    PollItem {
        sock: sock.poll_sock(),
        events: POLLIN,
        revents: 0,
    }
}

/// Kept until the transport rewrite moves `net_relay` to `network_interfaces`.
pub fn is_host_address_windows(ip: std::net::Ipv4Addr) -> bool {
    crate::network_interfaces::is_host_address(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_pair_transports_datagrams() {
        let (a, b) = wake_pair().unwrap();
        a.send(&[7u8; 5]).unwrap();
        let mut buffer = [0u8; 8];
        let read = b.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..read], &[7u8; 5]);
    }
}
