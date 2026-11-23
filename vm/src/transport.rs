//! Datagram pairs and readiness waiting, identical on every host.
//!
//! The gateway reaches the proxy stack through a connected pair of loopback
//! UDP sockets, and worker threads are woken through another. A UDP pair needs
//! no platform branches (unlike a Unix socketpair, which Windows lacks), and
//! `polling` waits on sockets with epoll, kqueue or wepoll.

use std::io;
use std::net::UdpSocket;
use std::time::Duration;

pub use polling::Event;
use polling::{AsRawSource, Events, PollMode, Poller};

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

/// Level-triggered readiness over sockets that stay open while watched.
pub struct Readiness {
    poller: Poller,
    events: Events,
}

impl Readiness {
    pub fn new() -> io::Result<Self> {
        Ok(Self { poller: Poller::new()?, events: Events::new() })
    }

    /// Watch `socket` under `key` for readability and/or writability.
    pub fn watch(&self, key: usize, socket: impl AsRawSource, read: bool, write: bool) -> io::Result<()> {
        let event = Event::new(key, read, write);
        // SAFETY: callers keep each socket open until this `Readiness` is dropped.
        unsafe { self.poller.add_with_mode(socket, event, PollMode::Level) }
    }

    /// Wait for readiness; `None` waits indefinitely. An interrupted wait
    /// reports no events.
    pub fn wait(&mut self, timeout: Option<Duration>) -> io::Result<&Events> {
        self.events.clear();
        match self.poller.wait(&mut self.events, timeout) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
        Ok(&self.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_reports_a_readable_socket_until_drained() {
        let (a, b) = packet_pair().unwrap();
        let mut ready = Readiness::new().unwrap();
        ready.watch(3, &b, true, false).unwrap();
        assert_eq!(ready.wait(Some(Duration::from_millis(10))).unwrap().iter().count(), 0);
        a.send(&[1, 2, 3]).unwrap();
        for _ in 0..2 {
            let events: Vec<_> = ready.wait(Some(Duration::from_secs(2))).unwrap().iter().collect();
            assert!(events.iter().any(|event| event.key == 3 && event.readable));
        }
        b.recv(&mut [0u8; 8]).unwrap();
        assert_eq!(ready.wait(Some(Duration::from_millis(10))).unwrap().iter().count(), 0);
    }
}
