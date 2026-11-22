use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(unix)]
use std::mem::ManuallyDrop;

#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
#[cfg(target_family = "unix")]
use std::path::Path;

use socket2::{Domain, Protocol, SockAddr, Socket, Type};

/// Validate an interface name without silently accepting an empty or truncated name.
pub(crate) fn validate_interface(interface: &str) -> std::io::Result<()> {
    if interface.is_empty() || interface.as_bytes().contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid bind_interface name",
        ));
    }
    #[cfg(target_os = "macos")]
    interface_index(interface)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn interface_index(interface: &str) -> std::io::Result<libc::c_uint> {
    let name = std::ffi::CString::new(interface).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "invalid bind_interface name",
        )
    })?;
    let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
    if index == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("network interface {interface:?} does not exist"),
        ));
    }
    Ok(index)
}

#[cfg(target_os = "macos")]
fn bind_apple_interface(
    fd: std::os::fd::RawFd,
    interface: &str,
    is_ipv6: bool,
) -> std::io::Result<()> {
    validate_interface(interface)?;
    let index = interface_index(interface)?;
    let (level, option) = if is_ipv6 {
        (libc::IPPROTO_IPV6, libc::IPV6_BOUND_IF)
    } else {
        (libc::IPPROTO_IP, libc::IP_BOUND_IF)
    };
    let result = unsafe {
        libc::setsockopt(
            fd,
            level,
            option,
            (&index as *const libc::c_uint).cast(),
            std::mem::size_of_val(&index) as libc::socklen_t,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn bind_socket_interface(socket: &Socket, interface: &str, is_ipv6: bool) -> std::io::Result<()> {
    validate_interface(interface)?;
    #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
    {
        let _ = is_ipv6;
        socket.bind_device(Some(interface.as_bytes()))
    }
    #[cfg(target_os = "macos")]
    {
        bind_apple_interface(socket.as_raw_fd(), interface, is_ipv6)
    }
    #[cfg(not(any(
        target_os = "android",
        target_os = "fuchsia",
        target_os = "linux",
        target_os = "macos"
    )))]
    {
        let _ = (socket, is_ipv6);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "bind_interface is unsupported on this platform",
        ))
    }
}

#[cfg(all(test, target_os = "macos"))]
mod interface_tests {
    use super::*;

    fn bound_index(fd: std::os::fd::RawFd, is_ipv6: bool) -> libc::c_uint {
        let (level, option) = if is_ipv6 {
            (libc::IPPROTO_IPV6, libc::IPV6_BOUND_IF)
        } else {
            (libc::IPPROTO_IP, libc::IP_BOUND_IF)
        };
        let mut index: libc::c_uint = 0;
        let mut len = std::mem::size_of_val(&index) as libc::socklen_t;
        assert_eq!(
            unsafe {
                libc::getsockopt(
                    fd,
                    level,
                    option,
                    (&mut index as *mut libc::c_uint).cast(),
                    &mut len,
                )
            },
            0,
            "{}",
            std::io::Error::last_os_error()
        );
        index
    }

    #[tokio::test]
    async fn binds_tcp_and_udp_in_both_address_families() {
        let expected = interface_index("lo0").unwrap();
        for is_ipv6 in [false, true] {
            let tcp = new_tcp_socket(Some("lo0".into()), is_ipv6).unwrap();
            let udp = new_udp_socket(is_ipv6, Some("lo0".into())).unwrap();
            assert_eq!(bound_index(tcp.as_raw_fd(), is_ipv6), expected);
            assert_eq!(bound_index(udp.as_raw_fd(), is_ipv6), expected);
            let addr = if is_ipv6 { "[::1]:0" } else { "127.0.0.1:0" };
            let listener = new_tcp_listener(addr.parse().unwrap(), 10, Some("lo0".into())).unwrap();
            assert_eq!(bound_index(listener.as_raw_fd(), is_ipv6), expected);
        }
    }

    #[tokio::test]
    async fn invalid_interfaces_return_errors_for_every_socket_family() {
        for name in ["", "lo0\0bad", "shoes-no-such-interface"] {
            assert!(validate_interface(name).is_err());
            for is_ipv6 in [false, true] {
                assert!(new_tcp_socket(Some(name.into()), is_ipv6).is_err());
                assert!(new_udp_socket(is_ipv6, Some(name.into())).is_err());
            }
        }
    }
}

#[cfg(all(test, unix, feature = "ffi"))]
mod protection_tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn outbound_protection_fails_closed_without_affecting_listeners() {
        struct Restore(Arc<dyn crate::tun::SocketProtector>);
        impl Drop for Restore {
            fn drop(&mut self) {
                crate::tun::set_global_socket_protector(self.0.clone());
            }
        }
        let _restore = Restore(crate::tun::get_global_socket_protector());
        let owner = std::thread::current().id();
        crate::tun::set_global_socket_protector(Arc::new(crate::tun::FnSocketProtector::new(
            move |_| {
                if std::thread::current().id() == owner {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "protection denied",
                    ))
                } else {
                    Ok(())
                }
            },
        )));
        assert!(new_tcp_socket(None, false).is_err());
        assert!(new_udp_socket(false, None).is_err());
        assert!(
            new_outbound_socket2_udp_socket(false, None, Some("0.0.0.0:0".parse().unwrap()))
                .is_err()
        );
        assert!(new_tcp_listener("0.0.0.0:0".parse().unwrap(), 10, None).is_ok());
        assert!(
            new_socket2_udp_socket(false, None, Some("0.0.0.0:0".parse().unwrap()), false).is_ok()
        );
    }
}

pub fn new_udp_socket(
    is_ipv6: bool,
    bind_interface: Option<String>,
) -> std::io::Result<tokio::net::UdpSocket> {
    let socket = new_outbound_socket2_udp_socket(
        is_ipv6,
        bind_interface,
        Some(get_unspecified_socket_addr(is_ipv6)),
    )?;

    into_tokio_udp_socket(socket)
}

pub fn new_outbound_socket2_udp_socket(
    is_ipv6: bool,
    bind_interface: Option<String>,
    bind_address: Option<SocketAddr>,
) -> std::io::Result<Socket> {
    let socket = new_socket2_udp_socket(is_ipv6, bind_interface, bind_address, false)?;
    #[cfg(any(target_os = "android", target_os = "ios", all(unix, feature = "ffi")))]
    crate::tun::protect_socket(socket.as_raw_fd())?;
    Ok(socket)
}

fn get_unspecified_socket_addr(is_ipv6: bool) -> SocketAddr {
    if !is_ipv6 {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0)), 0)
    } else {
        "[::]:0".parse().unwrap()
    }
}

pub fn new_socket2_udp_socket(
    is_ipv6: bool,
    bind_interface: Option<String>,
    bind_address: Option<SocketAddr>,
    reuse_port: bool,
) -> std::io::Result<socket2::Socket> {
    new_socket2_udp_socket_with_buffer_size(is_ipv6, bind_interface, bind_address, reuse_port, None)
}

pub fn new_socket2_udp_socket_with_buffer_size(
    is_ipv6: bool,
    bind_interface: Option<String>,
    bind_address: Option<SocketAddr>,
    reuse_port: bool,
    buffer_size: Option<usize>,
) -> std::io::Result<socket2::Socket> {
    let domain = if is_ipv6 { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;

    socket.set_nonblocking(true)?;

    // Set socket buffer sizes if specified.
    // This helps prevent packet drops during bursts for high-throughput connections.
    if let Some(size) = buffer_size {
        // Ignore errors - kernel may cap the value
        let _ = socket.set_recv_buffer_size(size);
        let _ = socket.set_send_buffer_size(size);
    }

    if reuse_port {
        #[cfg(all(unix, not(any(target_os = "solaris", target_os = "illumos"))))]
        socket.set_reuse_port(true)?;

        #[cfg(any(not(unix), target_os = "solaris", target_os = "illumos"))]
        panic!("Cannot support reuse sockets");
    }

    if let Some(interface) = bind_interface {
        bind_socket_interface(&socket, &interface, is_ipv6)?;
    }

    if let Some(bind_address) = bind_address {
        socket.bind(&SockAddr::from(bind_address))?;
    }

    Ok(socket)
}

fn into_tokio_udp_socket(socket: socket2::Socket) -> std::io::Result<tokio::net::UdpSocket> {
    #[cfg(unix)]
    {
        let raw_fd = socket.into_raw_fd();
        let std_udp_socket = unsafe { std::net::UdpSocket::from_raw_fd(raw_fd) };
        tokio::net::UdpSocket::from_std(std_udp_socket)
    }
    #[cfg(windows)]
    {
        let std_udp_socket: std::net::UdpSocket = socket.into();
        tokio::net::UdpSocket::from_std(std_udp_socket)
    }
}

pub fn new_tcp_socket(
    bind_interface: Option<String>,
    is_ipv6: bool,
) -> std::io::Result<tokio::net::TcpSocket> {
    let tcp_socket = if is_ipv6 {
        tokio::net::TcpSocket::new_v6()?
    } else {
        tokio::net::TcpSocket::new_v4()?
    };
    #[cfg(any(target_os = "android", target_os = "ios", all(unix, feature = "ffi")))]
    crate::tun::protect_socket(tcp_socket.as_raw_fd())?;

    if let Some(interface) = bind_interface {
        validate_interface(&interface)?;
        #[cfg(any(target_os = "android", target_os = "fuchsia", target_os = "linux"))]
        tcp_socket.bind_device(Some(interface.as_bytes()))?;
        #[cfg(target_os = "macos")]
        bind_apple_interface(tcp_socket.as_raw_fd(), &interface, is_ipv6)?;
        #[cfg(not(any(
            target_os = "android",
            target_os = "fuchsia",
            target_os = "linux",
            target_os = "macos"
        )))]
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "bind_interface is unsupported on this platform",
        ));
    }

    Ok(tcp_socket)
}

pub fn set_tcp_keepalive(
    tcp_stream: &tokio::net::TcpStream,
    idle_time: std::time::Duration,
    send_interval: std::time::Duration,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let raw_fd = tcp_stream.as_raw_fd();
        let socket2_socket = ManuallyDrop::new(unsafe { Socket::from_raw_fd(raw_fd) });
        if idle_time.is_zero() && send_interval.is_zero() {
            socket2_socket.set_keepalive(false)?;
        } else {
            let keepalive = socket2::TcpKeepalive::new()
                .with_time(idle_time)
                .with_interval(send_interval);
            socket2_socket.set_keepalive(true)?;
            socket2_socket.set_tcp_keepalive(&keepalive)?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let _ = (tcp_stream, idle_time, send_interval);
        Ok(())
    }
}

// TODO: change backlog to Option<u32> and make configuration, backlog -1 uses somaxconn on linux
// https://github.com/rust-lang/rust/blob/3534594029ed1495290e013647a1f53da561f7f1/library/std/src/os/unix/net/listener.rs#L93
pub fn new_tcp_listener(
    bind_address: SocketAddr,
    backlog: u32,
    bind_interface: Option<String>,
) -> std::io::Result<tokio::net::TcpListener> {
    let domain = if bind_address.is_ipv6() {
        Domain::IPV6
    } else {
        Domain::IPV4
    };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;

    socket.set_nonblocking(true)?;
    socket.set_reuse_address(true)?;

    if let Some(interface) = bind_interface {
        bind_socket_interface(&socket, &interface, bind_address.is_ipv6())?;
    }

    socket.bind(&SockAddr::from(bind_address))?;

    let backlog = backlog.try_into().unwrap_or(4096);
    socket.listen(backlog)?;

    let std_listener: std::net::TcpListener = socket.into();
    tokio::net::TcpListener::from_std(std_listener)
}

#[cfg(target_family = "unix")]
pub fn new_unix_listener<P: AsRef<Path>>(
    path: P,
    backlog: u32,
) -> std::io::Result<tokio::net::UnixListener> {
    let path = path.as_ref();

    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.set_nonblocking(true)?;

    let addr = SockAddr::unix(path)?;
    socket.bind(&addr)?;

    let backlog = backlog.try_into().unwrap_or(4096);
    socket.listen(backlog)?;

    let std_listener: std::os::unix::net::UnixListener = socket.into();
    tokio::net::UnixListener::from_std(std_listener)
}
