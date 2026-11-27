use std::cell::Cell;
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

// getaddrinfo can outlive a request and offers no portable cancellation.
// Count timed-out calls until the OS actually returns, including across detectors.
static LOOKUPS: LookupLimit = LookupLimit::new(16);

thread_local! {
    static REQUEST_DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

// ureq calls the resolver on the requesting thread, but omits the request deadline.
pub(super) fn with_timeout<T>(timeout: Duration, request: impl FnOnce() -> T) -> T {
    struct Restore(Option<Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            REQUEST_DEADLINE.set(self.0);
        }
    }

    let _restore = Restore(REQUEST_DEADLINE.replace(Instant::now().checked_add(timeout)));
    request()
}

pub(super) fn resolve(netloc: &str, ipv4_only: bool) -> io::Result<Vec<SocketAddr>> {
    let mut addresses = if let Ok(address) = netloc.parse::<SocketAddr>() {
        vec![address]
    } else {
        let deadline = REQUEST_DEADLINE.get().ok_or_else(timed_out)?;
        let netloc = netloc.to_owned();
        LOOKUPS.resolve(deadline, move || Ok(netloc.to_socket_addrs()?.collect()))?
    };
    if ipv4_only {
        addresses.retain(SocketAddr::is_ipv4);
        if addresses.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "No IPv4 address available",
            ));
        }
    }
    Ok(addresses)
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "DNS lookup deadline elapsed")
}

struct LookupLimit {
    active: AtomicUsize,
    max: usize,
}

impl LookupLimit {
    const fn new(max: usize) -> Self {
        Self {
            active: AtomicUsize::new(0),
            max,
        }
    }

    fn resolve<T: Send + 'static>(
        &'static self,
        deadline: Instant,
        lookup: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        struct Slot(&'static AtomicUsize);
        impl Drop for Slot {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::Relaxed);
            }
        }

        if deadline <= Instant::now() {
            return Err(timed_out());
        }
        let mut active = self.active.load(Ordering::Relaxed);
        loop {
            if active >= self.max {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "DNS lookup limit reached",
                ));
            }
            match self.active.compare_exchange_weak(
                active,
                active + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => active = current,
            }
        }
        let slot = Slot(&self.active);
        let (send, receive) = mpsc::sync_channel(1);
        std::thread::Builder::new().spawn(move || {
            let result = if deadline > Instant::now() {
                lookup()
            } else {
                Err(timed_out())
            };
            drop(slot);
            let _ = send.send(result);
        })?;
        let result = receive
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => timed_out(),
                mpsc::RecvTimeoutError::Disconnected => {
                    io::Error::other("DNS lookup worker exited")
                }
            })?;
        if deadline <= Instant::now() {
            return Err(timed_out());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timed_out_lookup_retains_its_slot_until_it_finishes() -> io::Result<()> {
        static LIMIT: LookupLimit = LookupLimit::new(1);
        let (release, blocked) = mpsc::channel();
        let result = LIMIT.resolve(Instant::now() + Duration::from_millis(250), move || {
            blocked.recv().map_err(io::Error::other)
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        let deadline = || Instant::now() + Duration::from_secs(1);
        assert_eq!(
            LIMIT.resolve(deadline(), || Ok(())).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        release.send(()).map_err(io::Error::other)?;
        let end = deadline();
        while LIMIT.active.load(Ordering::Relaxed) > 0 && Instant::now() < end {
            std::thread::yield_now();
        }
        LIMIT.resolve(deadline(), || Ok(()))
    }

    #[test]
    fn numeric_addresses_and_expired_hostname_requests_need_no_workers() -> io::Result<()> {
        assert_eq!(resolve("127.0.0.1:80", true)?.len(), 1);
        assert_eq!(
            resolve("[::1]:80", true).unwrap_err().kind(),
            io::ErrorKind::AddrNotAvailable
        );
        assert_eq!(
            with_timeout(Duration::ZERO, || resolve("localhost:80", false))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        Ok(())
    }
}
