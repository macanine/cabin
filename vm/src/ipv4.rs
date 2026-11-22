//! IPv4-only name resolution for host-side VM downloads and checks.

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
