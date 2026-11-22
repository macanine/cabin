//! IPv4-only address resolution for Cabin's host-side network clients.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};

pub fn resolve(netloc: &str) -> io::Result<Vec<SocketAddr>> {
    let addresses: Vec<_> = netloc
        .to_socket_addrs()?
        .filter(SocketAddr::is_ipv4)
        .collect();
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "No IPv4 address available",
        ));
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ipv6_and_keeps_ipv4() {
        assert!(resolve("[::1]:443").is_err());
        assert!(resolve("[::ffff:127.0.0.1]:443").is_err());
        assert!(
            resolve("127.0.0.1:443")
                .unwrap()
                .iter()
                .all(SocketAddr::is_ipv4)
        );
    }
}
