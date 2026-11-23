//! AF_UNIX sockets on every host.
//!
//! QEMU's control channels (QMP, guest agent, serial console) and the gateway
//! `-netdev stream` are Unix domain sockets at the instance paths in
//! `paths.rs`. QEMU 7.1 added AF_UNIX on Windows, so the same paths work
//! there; `uds_windows` mirrors the `std::os::unix::net` API for the host side.

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
