//! SocketConnectorImpl - Implementation of SocketConnector trait.
//!
//! Handles TCP and QUIC transports with bind_interface support.
//! Created from the socket-related fields of any ClientConfig.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use async_trait::async_trait;
use log::{debug, error};
use tokio::io::ReadBuf;
use tokio::net::UdpSocket;

use crate::address::{NetLocation, ResolvedLocation};
use crate::async_stream::AsyncStream;
use crate::config::{ClientConfig, ClientQuicConfig, Transport};
use crate::quic_endpoint::QuicEndpoint;
use crate::quic_stream::QuicStream;
use crate::resolver::{Resolver, resolve_addresses, resolve_location};
use crate::rustls_config_util::create_client_config;
use crate::socket_util::{new_tcp_socket, new_udp_socket, set_tcp_keepalive};
use crate::thread_util::get_num_threads;

use super::socket_connector::SocketConnector;

const MAX_QUIC_ENDPOINTS: usize = 32;

/// Cache only successful initialization. The lock is held only while creating
/// endpoints, never during asynchronous connection setup or steady-state use.
#[derive(Debug)]
struct RetryableInit<T> {
    value: OnceLock<T>,
    lock: parking_lot::Mutex<()>,
}

impl<T> RetryableInit<T> {
    fn new() -> Self {
        Self {
            value: OnceLock::new(),
            lock: parking_lot::Mutex::new(()),
        }
    }

    fn get_or_try_init(&self, init: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<&T> {
        if let Some(value) = self.value.get() {
            return Ok(value);
        }
        let _guard = self.lock.lock();
        if let Some(value) = self.value.get() {
            return Ok(value);
        }
        let value = init()?;
        // Only the lock holder can initialize this OnceLock.
        let _ = self.value.set(value);
        Ok(self.value.get().expect("initialized under lock"))
    }
}

#[derive(Debug)]
enum TransportConfig {
    Tcp {
        no_delay: bool,
    },
    Quic {
        sni_hostname: Option<String>,
        endpoints: RetryableInit<Vec<Arc<QuicEndpoint>>>,
        client_config: quinn::ClientConfig,
        memory_bytes: usize,
        is_ipv6: bool,
        next_endpoint_index: AtomicU8,
    },
}

/// Implementation of SocketConnector for TCP and QUIC transports.
///
/// Created from the socket-related fields of any ClientConfig:
/// - `bind_interface`
/// - `transport`
/// - `tcp_settings`
/// - `quic_settings`
#[derive(Debug)]
pub struct SocketConnectorImpl {
    bind_interface: Option<String>,
    transport: TransportConfig,
}

impl SocketConnectorImpl {
    /// Create a SocketConnector from a ClientConfig's socket-related fields.
    ///
    /// # Arguments
    /// * `config` - The client config (socket fields are extracted)
    /// * `target_address` - The address this connector will connect to (used for QUIC SNI default).
    ///   Pass None for direct protocol (QUIC is not supported for direct).
    ///
    /// QUIC endpoints are initialized on connect; failed initialization can be retried.
    pub fn from_config(
        config: &ClientConfig,
        target_address: Option<&NetLocation>,
    ) -> Option<Self> {
        let bind_interface = config.bind_interface.clone().into_option();

        let default_sni_hostname =
            target_address.and_then(|addr| addr.address().hostname().map(ToString::to_string));

        // Direct protocol only supports TCP (no proxy server to connect via QUIC)
        let effective_transport = if config.protocol.is_direct() {
            &Transport::Tcp
        } else {
            &config.transport
        };

        let transport = match *effective_transport {
            Transport::Tcp | Transport::Udp => {
                let no_delay = config
                    .tcp_settings
                    .as_ref()
                    .map(|tc| tc.no_delay)
                    .unwrap_or(true);
                TransportConfig::Tcp { no_delay }
            }
            Transport::Quic => {
                // QUIC requires a target address for endpoint creation
                let target_address = target_address.expect(
                    "QUIC transport requires target_address (direct protocol should use TCP)",
                );

                let ClientQuicConfig {
                    verify,
                    server_fingerprints,
                    alpn_protocols,
                    sni_hostname,
                    key,
                    cert,
                } = config.quic_settings.clone().unwrap_or_default();

                let sni_hostname = if sni_hostname.is_unspecified() {
                    if let Some(ref hostname) = default_sni_hostname {
                        debug!(
                            "Using default sni hostname for QUIC client connection: {}",
                            hostname
                        );
                    }
                    default_sni_hostname.clone()
                } else {
                    sni_hostname.into_option()
                };

                let tls13_suite =
                    match rustls::crypto::aws_lc_rs::cipher_suite::TLS13_AES_128_GCM_SHA256 {
                        rustls::SupportedCipherSuite::Tls13(t) => t,
                        _ => {
                            panic!("Could not retrieve Tls13CipherSuite");
                        }
                    };

                let key_and_cert_bytes = key.zip(cert).map(|(key, cert)| {
                    let cert_bytes = cert.as_bytes().to_vec();
                    let key_bytes = key.as_bytes().to_vec();
                    (key_bytes, cert_bytes)
                });

                let rustls_client_config = create_client_config(
                    verify,
                    server_fingerprints.into_vec(),
                    alpn_protocols.into_vec(),
                    sni_hostname.is_some(),
                    key_and_cert_bytes,
                    false, // tls13_only - QUIC enforces TLS 1.3 anyway
                );

                let quic_client_config = quinn::crypto::rustls::QuicClientConfig::with_initial(
                    Arc::new(rustls_client_config),
                    tls13_suite.quic_suite().unwrap(),
                )
                .unwrap();

                let mut quinn_client_config =
                    quinn::ClientConfig::new(Arc::new(quic_client_config));

                let mut transport_config = quinn::TransportConfig::default();
                let memory_bytes =
                    crate::resources::configure_quic_transport(&mut transport_config);
                transport_config
                    .max_concurrent_bidi_streams(0_u32.into())
                    .max_concurrent_uni_streams(0_u8.into())
                    .keep_alive_interval(Some(std::time::Duration::from_secs(15)))
                    .max_idle_timeout(Some(std::time::Duration::from_secs(30).try_into().unwrap()));

                quinn_client_config.transport_config(Arc::new(transport_config));

                TransportConfig::Quic {
                    sni_hostname,
                    endpoints: RetryableInit::new(),
                    client_config: quinn_client_config,
                    memory_bytes,
                    is_ipv6: target_address.address().is_ipv6(),
                    next_endpoint_index: AtomicU8::new(0),
                }
            }
        };

        Some(Self {
            bind_interface,
            transport,
        })
    }

    /// Create a simple TCP SocketConnector for direct connections.
    ///
    /// Used when only TCP is needed (no QUIC).
    pub fn new_tcp(bind_interface: Option<String>, no_delay: bool) -> Self {
        Self {
            bind_interface,
            transport: TransportConfig::Tcp { no_delay },
        }
    }
}

#[async_trait]
impl SocketConnector for SocketConnectorImpl {
    async fn connect(
        &self,
        resolver: &Arc<dyn Resolver>,
        address: &ResolvedLocation,
    ) -> std::io::Result<Box<dyn AsyncStream>> {
        let quic_endpoints = if let TransportConfig::Quic {
            endpoints,
            client_config,
            memory_bytes,
            is_ipv6,
            ..
        } = &self.transport
        {
            Some(endpoints.get_or_try_init(|| {
                let count = std::cmp::min(get_num_threads(), MAX_QUIC_ENDPOINTS).max(1);
                let mut endpoints = Vec::with_capacity(count);
                for _ in 0..count {
                    let socket =
                        new_udp_socket(*is_ipv6, self.bind_interface.clone())?.into_std()?;
                    let mut endpoint = QuicEndpoint::new(None, socket, *memory_bytes)?;
                    endpoint.set_default_client_config(client_config.clone());
                    endpoints.push(Arc::new(endpoint));
                }
                Ok(endpoints)
            })?)
        } else {
            None
        };
        let target_addrs = match address.resolved_addr() {
            Some(r) if resolver.ipv4_only() && !r.is_ipv4() => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    "IPv6 is unavailable",
                ));
            }
            Some(r) => vec![r],
            None => resolve_addresses(resolver, address.location()).await?,
        };

        match &self.transport {
            TransportConfig::Tcp { no_delay } => {
                let mut last_err = None;
                for (i, target_addr) in target_addrs.iter().enumerate() {
                    let tcp_socket =
                        new_tcp_socket(self.bind_interface.clone(), target_addr.is_ipv6())?;
                    match tcp_socket.connect(*target_addr).await {
                        Ok(stream) => {
                            if i > 0 {
                                debug!(
                                    "TCP connect succeeded on address #{} ({}) after {} failures",
                                    i, target_addr, i
                                );
                            }
                            if let Err(e) = set_tcp_keepalive(
                                &stream,
                                std::time::Duration::from_secs(120),
                                std::time::Duration::from_secs(30),
                            ) {
                                error!("Failed to set TCP keepalive: {e}");
                            }
                            if *no_delay && let Err(e) = stream.set_nodelay(true) {
                                error!("Failed to set TCP no-delay: {e}");
                            }
                            return Ok(Box::new(stream));
                        }
                        Err(e) => {
                            debug!("TCP connect to {} failed: {}, trying next", target_addr, e);
                            last_err = Some(e);
                        }
                    }
                }
                Err(last_err
                    .unwrap_or_else(|| std::io::Error::other("no resolved addresses succeeded")))
            }
            TransportConfig::Quic {
                next_endpoint_index,
                sni_hostname,
                ..
            } => {
                let endpoints = quic_endpoints.expect("QUIC endpoints initialized above");
                let domain = match sni_hostname {
                    Some(s) => s.as_str(),
                    None => address.address().hostname().unwrap_or("example.com"),
                };

                let mut last_err = None;
                for (i, target_addr) in target_addrs.iter().enumerate() {
                    let endpoint = if endpoints.len() == 1 {
                        &endpoints[0]
                    } else {
                        let idx = next_endpoint_index.fetch_add(1, Ordering::Relaxed) as usize;
                        &endpoints[idx % endpoints.len()]
                    };

                    match endpoint.connect(*target_addr, domain) {
                        Ok(connecting) => match connecting.await {
                            Ok(conn) => match conn.open_bi().await {
                                Ok((send, recv)) => {
                                    if i > 0 {
                                        debug!(
                                            "QUIC connect succeeded on address #{} ({}) after {} failures",
                                            i, target_addr, i
                                        );
                                    }
                                    return Ok(Box::new(QuicStream::from(send, recv)));
                                }
                                Err(e) => {
                                    debug!("QUIC open_bi to {} failed: {}", target_addr, e);
                                    last_err = Some(std::io::Error::other(format!(
                                        "Failed to open QUIC stream: {e}"
                                    )));
                                }
                            },
                            Err(e) => {
                                debug!("QUIC connection to {} failed: {}", target_addr, e);
                                last_err = Some(std::io::Error::other(format!(
                                    "QUIC connection failed: {e}"
                                )));
                            }
                        },
                        Err(e) => {
                            debug!("QUIC connect to {} failed: {}", target_addr, e);
                            last_err = Some(std::io::Error::other(format!(
                                "Failed to connect to QUIC endpoint: {e}"
                            )));
                        }
                    }
                }
                Err(last_err
                    .unwrap_or_else(|| std::io::Error::other("no resolved addresses succeeded")))
            }
        }
    }

    async fn connect_udp_bidirectional(
        &self,
        resolver: &Arc<dyn Resolver>,
        mut target: ResolvedLocation,
    ) -> std::io::Result<Box<dyn crate::async_stream::AsyncMessageStream>> {
        debug!(
            "[SocketConnector] connect_udp_bidirectional called, target: {}",
            target.location()
        );

        let remote_addr = resolve_location(&mut target, resolver).await?;
        let client_socket = new_udp_socket(remote_addr.is_ipv6(), self.bind_interface.clone())?;

        // Don't use connect() - wrap in UnconnectedUdpSocket instead.
        // A connected UDP socket filters incoming packets by source address,
        // which breaks when bind_interface causes packets to arrive from
        // a different source than the target address.
        Ok(Box::new(UnconnectedUdpSocket::new(
            client_socket,
            remote_addr,
        )))
    }

    fn bind_interface(&self) -> Option<&str> {
        self.bind_interface.as_deref()
    }
}

/// A UDP socket wrapper that tracks the destination and uses send_to/recv_from.
/// Unlike a connected UDP socket, this accepts incoming packets from any source.
struct UnconnectedUdpSocket {
    socket: UdpSocket,
    destination: SocketAddr,
}

impl UnconnectedUdpSocket {
    fn new(socket: UdpSocket, destination: SocketAddr) -> Self {
        Self {
            socket,
            destination,
        }
    }
}

impl crate::async_stream::AsyncReadMessage for UnconnectedUdpSocket {
    fn read_message_eof_on_empty(&self) -> bool {
        false
    }
    fn poll_read_message(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        match this.socket.poll_recv_from(cx, buf) {
            Poll::Ready(Ok(addr)) => {
                log::debug!(
                    "[UnconnectedUdp] Received {} bytes from {} (target: {})",
                    buf.filled().len(),
                    addr,
                    this.destination
                );
                Poll::Ready(Ok(()))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl crate::async_stream::AsyncWriteMessage for UnconnectedUdpSocket {
    fn poll_write_message(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        this.socket
            .poll_send_to(cx, buf, this.destination)
            .map(|r| r.map(|_| ()))
    }
}

impl crate::async_stream::AsyncFlushMessage for UnconnectedUdpSocket {
    fn poll_flush_message(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl crate::async_stream::AsyncShutdownMessage for UnconnectedUdpSocket {
    fn poll_shutdown_message(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl crate::async_stream::AsyncPing for UnconnectedUdpSocket {
    fn supports_ping(&self) -> bool {
        false
    }

    fn poll_write_ping(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<bool>> {
        Poll::Ready(Ok(false))
    }
}

impl crate::async_stream::AsyncMessageStream for UnconnectedUdpSocket {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_tcp() {
        let connector = SocketConnectorImpl::new_tcp(Some("eth0".to_string()), true);
        assert!(matches!(
            connector.transport,
            TransportConfig::Tcp { no_delay: true }
        ));
        assert_eq!(connector.bind_interface, Some("eth0".to_string()));
    }

    #[test]
    fn quic_endpoint_initialization_retries_after_transient_failure_with_concurrent_callers() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let initializer = Arc::new(RetryableInit::new());
        let attempts = Arc::new(AtomicUsize::new(0));
        let first = initializer.get_or_try_init(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<usize, _>(std::io::Error::from_raw_os_error(libc::EMFILE))
        });
        assert_eq!(first.unwrap_err().raw_os_error(), Some(libc::EMFILE));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let initializer = initializer.clone();
                let attempts = attempts.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    *initializer
                        .get_or_try_init(|| {
                            attempts.fetch_add(1, Ordering::SeqCst);
                            Ok(42)
                        })
                        .unwrap()
                })
            })
            .collect();
        for thread in threads {
            assert_eq!(thread.join().unwrap(), 42);
        }
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "one failed initialization and one shared success"
        );
    }

    #[tokio::test]
    async fn quic_socket_binding_failure_reaches_caller() {
        use crate::config::ClientProxyConfig;
        use crate::option_util::NoneOrOne;
        use crate::resolver::NativeResolver;

        let target = NetLocation::from_str("127.0.0.1:443", None).unwrap();
        let config = ClientConfig {
            bind_interface: NoneOrOne::One("bad\0interface".into()),
            protocol: ClientProxyConfig::Socks {
                username: None,
                password: None,
            },
            transport: Transport::Quic,
            ..ClientConfig::default()
        };
        let connector = SocketConnectorImpl::from_config(&config, Some(&target)).unwrap();
        let resolver: Arc<dyn Resolver> = Arc::new(NativeResolver::new());
        for _ in 0..2 {
            let error = match connector.connect(&resolver, &target.clone().into()).await {
                Ok(_) => panic!("invalid binding unexpectedly connected"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("bind_interface"));
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn quic_connector_recovers_after_socket_creation_failure() {
        use crate::config::ClientProxyConfig;
        use crate::option_util::NoneOrOne;
        use crate::resolver::NativeResolver;
        let target = NetLocation::from_str("127.0.0.1:0", None).unwrap();
        let config = ClientConfig {
            bind_interface: NoneOrOne::One("invalid\0interface".into()),
            protocol: ClientProxyConfig::Socks {
                username: None,
                password: None,
            },
            transport: Transport::Quic,
            ..ClientConfig::default()
        };
        let mut connector = SocketConnectorImpl::from_config(&config, Some(&target)).unwrap();
        let resolver: Arc<dyn Resolver> = Arc::new(NativeResolver::new());
        assert!(
            connector
                .connect(&resolver, &target.clone().into())
                .await
                .is_err()
        );
        let TransportConfig::Quic { endpoints, .. } = &connector.transport else {
            panic!("expected QUIC")
        };
        assert!(endpoints.value.get().is_none());
        connector.bind_interface = Some(
            if cfg!(target_os = "macos") {
                "lo0"
            } else {
                "lo"
            }
            .into(),
        );
        // Port zero fails immediately after endpoint creation; no remote server is needed.
        assert!(connector.connect(&resolver, &target.into()).await.is_err());
        let TransportConfig::Quic { endpoints, .. } = &connector.transport else {
            panic!("expected QUIC")
        };
        assert!(
            endpoints.value.get().is_some(),
            "later connection must create and retain endpoints"
        );
    }

    #[test]
    fn test_from_config_direct_protocol() {
        let config = ClientConfig::default(); // default is direct protocol
        let connector = SocketConnectorImpl::from_config(&config, None);
        assert!(connector.is_some());
        assert!(matches!(
            connector.unwrap().transport,
            TransportConfig::Tcp { .. }
        ));
    }
}
