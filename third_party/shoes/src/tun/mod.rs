//! TUN device support for shoes.
//!
//! This module provides VPN functionality by accepting IP packets from a TUN
//! device and routing TCP/UDP traffic through configured proxy chains.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐     ┌─────────────────┐     ┌─────────────────┐
//! │   TUN Device    │ ←→  │  shoes/smoltcp  │ ←→  │  Proxy Chain    │
//! │ (IP packets)    │     │ (our TCP stack) │     │ (VLESS, etc.)   │
//! └─────────────────┘     └─────────────────┘     └─────────────────┘
//! ```
//!
//! The smoltcp stack runs in a dedicated OS thread, waiting for packet
//! readiness and cross-thread wakeups. Cabin embeds it: the packet device is
//! always one half of a loopback UDP pair the host owns (one datagram per raw
//! IP packet), on every platform. Creating or wrapping real TUN devices is not
//! supported (see VENDORED.txt).

mod packet;
mod relay_gate;
mod tcp_conn;
mod tcp_stack_direct;
mod tun_server;
mod udp_handler;
mod udp_manager;
mod wake;

pub use relay_gate::{RelayGate, RelayTicket};
pub use tun_server::TunServerConfig;
pub use wake::loopback_udp_pair;

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;

use log::{debug, info, warn};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use crate::address::{Address, NetLocation};
use crate::client_proxy_selector::ClientProxySelector;
use crate::config::TunConfig;
use crate::config::selection::ConfigSelection;
use crate::resolver::Resolver;
use crate::tcp::tcp_client_handler_factory::create_tcp_client_proxy_selector;

use packet::PacketBuffer;
use tcp_stack_direct::{NewTcpConnection, PACKET_QUEUE_CAPACITY, TcpStackDirect};
use udp_manager::TunUdpManager;

/// Run the TUN server on `device` until `shutdown_rx` fires.
///
/// This function:
/// 1. Sets up our smoltcp-based TCP/IP stack on the packet socket
/// 2. The stack thread reads packets directly from it
/// 3. Handles TCP connections through the proxy chain
/// 4. Handles UDP packets through tokio (forwarded from stack thread)
async fn run_tun_server(
    config: TunServerConfig,
    device: UdpSocket,
    proxy_selector: Arc<ClientProxySelector>,
    resolver: Arc<dyn Resolver>,
    mut shutdown_rx: oneshot::Receiver<()>,
    gate: RelayGate,
) -> std::io::Result<()> {
    config.resource_limits.validate()?;
    info!(
        "Starting TUN server (direct mode): mtu={}, tcp={}, udp={}, icmp={}",
        config.mtu, config.tcp_enabled, config.udp_enabled, config.icmp_enabled
    );

    // Creates the direct TCP stack in a dedicated thread.
    let mut tcp_stack = TcpStackDirect::with_config(device, config.clone(), gate.clone())?;

    let mut gate_updates = gate.subscribe();

    // Get UDP receiver (stack thread filters UDP and sends here)
    let udp_from_stack_rx = tcp_stack.take_udp_rx().expect("udp_rx already taken");

    // Channel for sending UDP responses back (stack thread will write to TUN)
    let (udp_to_stack_tx, udp_to_stack_rx) = mpsc::channel::<PacketBuffer>(PACKET_QUEUE_CAPACITY);
    tcp_stack.set_udp_response_tx(udp_to_stack_rx);

    let (tcp_conn_tx, mut tcp_conn_rx) = mpsc::unbounded_channel::<NewTcpConnection>();
    tcp_stack.set_new_conn_tx(tcp_conn_tx);

    let mut tasks = JoinSet::new();
    if config.tcp_enabled {
        let proxy_selector = proxy_selector.clone();
        let resolver = resolver.clone();
        let gate = gate.clone();

        tasks.spawn(async move {
            info!("Starting TCP connection handler");
            let mut connections = JoinSet::new();
            loop {
                let new_conn = tokio::select! {
                    conn = tcp_conn_rx.recv() => match conn {
                        Some(conn) => conn,
                        None => break,
                    },
                    _ = connections.join_next(), if !connections.is_empty() => continue,
                };
                let proxy_selector = proxy_selector.clone();
                let resolver = resolver.clone();
                let gate = gate.clone();

                connections.spawn(async move {
                    let remote_addr = new_conn.remote_addr;
                    let target = socket_addr_to_net_location(remote_addr);

                    debug!("Handling TCP connection to {:?}", target);

                    if let Err(e) =
                        handle_tcp_connection(new_conn, target, proxy_selector, resolver, gate)
                            .await
                    {
                        debug!("TCP connection to {} failed: {}", remote_addr, e);
                    }
                });
            }

            debug!("TCP connection handler ended");
        });
    }

    if config.udp_enabled {
        let proxy_selector = proxy_selector.clone();
        let resolver = resolver.clone();
        let limits = config.resource_limits.clone();
        let wake = tcp_stack.wake_handle();

        tasks.spawn(async move {
            handle_udp_packets(
                udp_from_stack_rx,
                udp_to_stack_tx,
                proxy_selector,
                resolver,
                limits,
                wake,
                gate,
            )
            .await;
        });
    }

    let wake = tcp_stack.wake_handle();
    tasks.spawn(async move {
        while gate_updates.changed().await.is_ok() {
            wake.notify();
        }
    });

    info!("TUN server started successfully");

    // Wait for shutdown signal or stack thread exit
    let result = tokio::select! {
        _ = &mut shutdown_rx => Ok(()),
        _ = async {
            // Poll until stack stops running
            while tcp_stack.is_running() {
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            }
        } => Err(std::io::Error::other("TUN stack thread ended unexpectedly")),
    };

    tasks.shutdown().await;

    // tcp_stack is dropped here, which stops the stack thread
    info!("TUN server stopped");
    result
}

/// Convert a SocketAddr to a NetLocation.
fn socket_addr_to_net_location(addr: SocketAddr) -> NetLocation {
    let address = match addr.ip() {
        std::net::IpAddr::V4(v4) => Address::Ipv4(v4),
        std::net::IpAddr::V6(v6) => Address::Ipv6(v6),
    };
    NetLocation::new(address, addr.port())
}

/// Handle a TCP connection by forwarding it through the proxy chain.
async fn handle_tcp_connection(
    new_conn: NewTcpConnection,
    target: NetLocation,
    proxy_selector: Arc<ClientProxySelector>,
    resolver: Arc<dyn Resolver>,
    gate: RelayGate,
) -> std::io::Result<()> {
    use crate::client_proxy_selector::ConnectDecision;

    let NewTcpConnection {
        mut connection,
        remote_addr,
        epoch,
    } = new_conn;
    let control = connection.control();
    let _permit = crate::resources::try_stream().ok_or_else(crate::resources::exhausted)?;
    let setup = gate
        .run(|current| current == epoch, async {
            let decision = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                proxy_selector.judge(target.into(), &resolver),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "TUN routing timed out")
            })??;
            let ConnectDecision::Allow {
                chain_group,
                remote_location,
            } = decision
            else {
                return Ok::<_, std::io::Error>(None);
            };
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                chain_group.connect_tcp(remote_location, &resolver),
            )
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "TUN TCP setup timed out")
            })??;
            control.establish_upstream();
            Ok(Some(result.client_stream))
        })
        .await;
    if let Some(mut remote) = setup.transpose()?.flatten() {
        let result = gate
            .run(
                |current| current == epoch || control.is_established(),
                tokio::io::copy_bidirectional(&mut connection, &mut remote),
            )
            .await;
        debug!("TCP relay to {remote_addr} finished: {result:?}");
    }
    Ok(())
}

/// Handle UDP packets from the stack thread.
///
/// Uses the session-based TunUdpManager which:
/// - Keys sessions by local (app) address, not by destination
/// - Stores the return address in each session
/// - Routes responses using the stored address (no NAT table lookup)
async fn handle_udp_packets(
    from_stack_rx: mpsc::Receiver<PacketBuffer>,
    to_stack_tx: mpsc::Sender<PacketBuffer>,
    proxy_selector: Arc<ClientProxySelector>,
    resolver: Arc<dyn Resolver>,
    limits: crate::config::tun::TunResourceLimits,
    wake: wake::Wake,
    gate: RelayGate,
) {
    info!("Starting UDP handler (session-based)");

    let udp_handler = udp_handler::UdpHandler::new(from_stack_rx, to_stack_tx, wake);
    let (reader, writer) = udp_handler.split();

    let manager = TunUdpManager::new(reader, writer, proxy_selector, resolver, limits);

    if let Err(e) = manager.run(gate).await {
        warn!("UDP handler error: {}", e);
    }

    info!("UDP handler stopped");
}

/// Run the TUN config on the host-owned packet `device` until `shutdown_rx` fires.
pub(crate) async fn run_tun_from_config_controlled(
    config: TunConfig,
    device: UdpSocket,
    shutdown_rx: oneshot::Receiver<()>,
    resolver: Arc<dyn Resolver>,
    gate: RelayGate,
) -> std::io::Result<()> {
    let mut tun_server_config = TunServerConfig::new()
        .mtu(config.mtu)
        .tcp_enabled(config.tcp_enabled)
        .udp_enabled(config.udp_enabled)
        .icmp_enabled(config.icmp_enabled);
    tun_server_config.resource_limits = config.resource_limits;

    let rules = config.rules.map(ConfigSelection::unwrap_config).into_vec();
    let client_proxy_selector = Arc::new(create_tcp_client_proxy_selector(rules, resolver.clone()));

    run_tun_server(
        tun_server_config,
        device,
        client_proxy_selector,
        resolver,
        shutdown_rx,
        gate,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stop_signal_ends_the_server() {
        let (device, _peer) = wake::loopback_udp_pair(None).unwrap();
        let (tx, rx) = oneshot::channel();
        tx.send(()).unwrap();
        run_tun_server(
            TunServerConfig::new(),
            device,
            Arc::new(ClientProxySelector::new(Vec::new())),
            Arc::new(crate::resolver::NativeResolver::new()),
            rx,
            RelayGate::default(),
        )
        .await
        .unwrap();
    }
}
