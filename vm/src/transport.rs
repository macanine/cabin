//! Datagram pairs and readiness waiting, identical on every host.
//!
//! The gateway reaches the proxy stack through a connected pair of loopback
//! UDP sockets, and worker threads are woken through another. A UDP pair needs
//! no platform branches (unlike a Unix socketpair, which Windows lacks), and
//! `polling` waits on sockets with epoll, kqueue or wepoll.

use std::io;
use std::net::UdpSocket;
use std::time::Duration;

#[cfg(unix)]
use std::os::fd::BorrowedFd as BorrowedSource;
#[cfg(windows)]
use std::os::windows::io::BorrowedSocket as BorrowedSource;

use polling::{AsSource, Event, Events, PollMode, Poller};

/// Receive/send buffer for the gateway's packet path. Loopback UDP drops
/// datagrams when a buffer fills, and macOS defaults are small.
const PACKET_BUFFER: usize = 4 * 1024 * 1024;

/// Connected non-blocking pair for cross-thread wake-ups.
pub fn wake_pair() -> io::Result<(UdpSocket, UdpSocket)> {
    shoes::embed::loopback_udp_pair(None)
}

/// Connected non-blocking pair carrying whole IP packets between the gateway
/// and the proxy stack.
pub fn packet_pair() -> io::Result<(UdpSocket, UdpSocket)> {
    shoes::embed::loopback_udp_pair(Some(PACKET_BUFFER))
}

/// Level-triggered readiness scoped to borrowed sockets.
pub(crate) struct Readiness<'socket> {
    poller: Poller,
    events: Events,
    sources: Vec<BorrowedSource<'socket>>,
}

impl<'socket> Readiness<'socket> {
    pub(crate) fn scope(run: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<()> {
        // The callback only borrows the poller, so it cannot leak registrations.
        let mut readiness = Self {
            poller: Poller::new()?,
            events: Events::new(),
            sources: Vec::new(),
        };
        run(&mut readiness)
    }

    /// Watch `socket` under `key` for readability and/or writability.
    pub(crate) fn watch(
        &mut self,
        key: usize,
        socket: &'socket impl AsSource,
        read: bool,
        write: bool,
    ) -> io::Result<()> {
        let source = socket.source();
        // A backend may fail after inserting the source. Track it before adding.
        self.sources.push(source);
        // SAFETY: the borrowed source stays open and Drop unregisters it.
        unsafe {
            self.poller
                .add_with_mode(&source, Event::new(key, read, write), PollMode::Level)
        }
    }

    /// Wait for readiness; `None` waits indefinitely. An interrupted wait
    /// reports no events.
    pub(crate) fn wait(&mut self, timeout: Option<Duration>) -> io::Result<&Events> {
        self.events.clear();
        match self.poller.wait(&mut self.events, timeout) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        Ok(&self.events)
    }
}

impl Drop for Readiness<'_> {
    fn drop(&mut self) {
        for source in self.sources.drain(..) {
            let _ = self.poller.delete(source);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_reports_a_readable_socket_until_drained() {
        let (a, b) = packet_pair().unwrap();
        Readiness::scope(|ready| {
            ready.watch(3, &b, true, false)?;
            assert_eq!(
                ready.wait(Some(Duration::from_millis(10)))?.iter().count(),
                0
            );
            a.send(&[1, 2, 3])?;
            for _ in 0..2 {
                let events = ready.wait(Some(Duration::from_secs(2)))?;
                assert!(events.iter().any(|event| event.key == 3 && event.readable));
            }
            b.recv(&mut [0u8; 8])?;
            assert_eq!(
                ready.wait(Some(Duration::from_millis(10)))?.iter().count(),
                0
            );
            Ok(())
        })
        .unwrap();
    }
}
