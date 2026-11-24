//! The VM's only way to the internet: a virtual gateway on the Mac.
//!
//! QEMU connects the VM's second NIC to a Unix socket (`-netdev stream`,
//! frames prefixed with a 4-byte big-endian length). This gateway answers ARP
//! and DHCP for the VM itself, drops everything that is not IPv4 to the
//! internet, and hands the remaining IP packets to shoes' TUN stack over a
//! socket pair. shoes terminates TCP/UDP in user space and sends each flow
//! through the configured chain (a subscription node, or direct). Nothing in
//! the VM is configured for a proxy, and the VM cannot reach the Mac or the
//! LAN: private destinations are dropped here and blocked again in shoes.
//!
//! When shoes is not running (or its node fails) packets are simply not
//! answered: the VM has no internet (fail closed).
//!
//! This is the VM's only NIC. The gateway address also serves the Mac's SSH
//! port forward (`net_forward`) and NTP: the VM keeps time from the Mac's
//! clock, which macOS synchronises, whatever the upstream can carry.

mod chain;
mod dns;
mod frame;
mod policy;
mod relay;

pub use chain::{Upstream, shoes_config};
pub use frame::{Action, BLOCKED, handle_frame, ntp_reply, to_guest_frame};
pub use policy::EgressMode;

use crate::util::LockExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use shoes::embed::RelayGate;

use crate::sockets::UnixListener;
use crate::{Error, Result};

use dns::Resolver;
use frame::read_frame_until;
use relay::{DnsRequest, GatewayStacks, GuestEgress, PacketPath, Shared};

pub const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 3, 1);
pub const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 3, 15);
pub const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
pub const GATEWAY_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x0a, 0x03, 0x01];
/// MAC of the VM's internet NIC (QEMU `-device …,mac=`).
pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x0a, 0x03, 0x0f];
pub const MTU: usize = 1500;
/// Used when no DNS servers are configured.
pub const DEFAULT_DNS: [Ipv4Addr; 2] = [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)];

/// Cumulative IP bytes from the guest's point of view, for the life of this gateway.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GatewayTraffic {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// The gateway: listens on `socket` for QEMU and runs the proxy stack.
pub struct Gateway {
    _socket_path: RemoveOnDrop,
    shared: Arc<Shared>,
    /// The Mac's SSH port, relayed to the guest.
    forward: Option<Arc<crate::net_forward::SshForward>>,
    stacks: Mutex<GatewayStacks>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Gateway {
    /// Listen on `socket` (QEMU connects to it) and start routing through
    /// `upstream`. Packets are answered by the stack only while it runs.
    /// `ssh` is the Mac-side address relayed to the guest's port 22.
    pub fn start(
        socket: &Path,
        dns: Vec<Ipv4Addr>,
        upstream: &Upstream,
        ssh: std::net::SocketAddr,
    ) -> Result<Self> {
        Self::start_with_egress(socket, dns, Some(upstream), ssh, EgressMode::Open)
    }

    /// Start with the internet path closed. With no upstream, only local
    /// management traffic is possible until a route is installed.
    pub fn start_closed(
        socket: &Path,
        dns: Vec<Ipv4Addr>,
        upstream: Option<&Upstream>,
        ssh: std::net::SocketAddr,
    ) -> Result<Self> {
        Self::start_with_egress(socket, dns, upstream, ssh, EgressMode::Closed)
    }

    fn start_with_egress(
        socket: &Path,
        dns: Vec<Ipv4Addr>,
        upstream: Option<&Upstream>,
        ssh: std::net::SocketAddr,
        egress: EgressMode,
    ) -> Result<Self> {
        crate::sockets::prepare_listener(socket)?;
        if let Some(parent) = socket.parent() {
            crate::util::ensure_dir(parent)?;
        }
        // On early failure, close the listener before removing its path.
        let socket_path;
        let listener = UnixListener::bind(socket).map_err(|error| {
            Error::Message(format!(
                "Could not open the VM's network socket {}: {error}",
                socket.display()
            ))
        })?;
        socket_path = RemoveOnDrop(socket.to_path_buf());
        listener.set_nonblocking(true)?;
        let resolver = upstream.map(Resolver::start).transpose()?.map(Arc::new);
        let dns_gate = RelayGate::default();
        dns_gate.suspend(|| {});
        let down_wake = crate::transport::wake_pair()?;
        let shared = Arc::new(Shared {
            down_wake,
            dns: if dns.is_empty() {
                DEFAULT_DNS.to_vec()
            } else {
                dns
            },
            guest: Mutex::new(None),
            egress: Mutex::new(GuestEgress {
                mode: EgressMode::Closed,
                dns_gate,
                probe_flows: HashSet::new(),
                ordinary_probe_syns: HashMap::new(),
                held_packets: VecDeque::new(),
                held_dns: VecDeque::new(),
                dns_inflight: 0,
            }),
            held_ready: Condvar::new(),
            stop: AtomicBool::new(false),
            error: Arc::new(Mutex::new(None)),
            resolver: Mutex::new(resolver),
            paths: [PacketPath::new()?, PacketPath::new()?],
            rx_bytes: AtomicU64::new(0),
            tx_bytes: AtomicU64::new(0),
        });
        let mut stacks = GatewayStacks {
            upstream: upstream.cloned(),
            active: [None, None],
            ordinary_opened: false,
        };
        stacks.activate(&shared, egress)?;
        let forward = {
            let shared = shared.clone();
            Arc::new(crate::net_forward::SshForward::start(ssh, move |frame| {
                shared.write_guest(frame);
            })?)
        };

        // Own the workers before starting any of them. A later startup
        // failure then stops and joins the workers already started.
        let mut gateway = Self {
            _socket_path: socket_path,
            shared,
            forward: Some(forward.clone()),
            stacks: Mutex::new(stacks),
            threads: Vec::new(),
        };
        // Move the listener into the first worker so it closes before the
        // gateway removes its socket path, even if spawning fails.
        // QEMU can reconnect after an app restart.
        {
            let shared = gateway.shared.clone();
            gateway.threads.push(std::thread::Builder::new().name("vm-net-up".into()).spawn(
                move || {
                    while !shared.stop.load(Ordering::Relaxed) {
                        let stream = match listener.accept() {
                            Ok((stream, _)) => stream,
                            Err(_) => {
                                std::thread::sleep(Duration::from_millis(100));
                                continue;
                            }
                        };
                        // A blocking stream without timeouts could prevent shutdown.
                        if stream.set_nonblocking(false)
                            .and_then(|()| stream.set_read_timeout(Some(Duration::from_millis(250))))
                            .and_then(|()| stream.set_write_timeout(Some(Duration::from_millis(200))))
                            .is_err()
                        {
                            continue;
                        }
                        let stream = Arc::new(stream);
                        {
                            let mut guest = shared.guest.locked();
                            if shared.stop.load(Ordering::Relaxed) {
                                break;
                            }
                            *guest = Some(stream.clone());
                        }
                        let mut frame = Vec::new();
                        while !shared.stop.load(Ordering::Relaxed) {
                            match read_frame_until(&mut &*stream, &mut frame, &shared.stop) {
                                Ok(()) => {}
                                Err(_) => break,
                            }
                            match handle_frame(&frame, &shared.dns) {
                                Action::ToGuest(reply) => {
                                    shared.write_guest(&reply);
                                }
                                Action::ToStack(packet) => shared.send_ip(packet),
                                Action::Forward(frame) => forward.deliver(frame),
                                Action::Dns {
                                    src,
                                    dst,
                                    port,
                                    ip_len,
                                    query,
                                } => {
                                    shared.resolve(DnsRequest { src, dst, port, ip_len, query });
                                }
                                Action::Drop => {}
                            }
                        }
                        shared.disconnect_guest();
                    }
                },
            )?);
        }
        // Stack → VM.
        {
            let shared = gateway.shared.clone();
            gateway.threads.push(
                std::thread::Builder::new()
                    .name("vm-net-down".into())
                    .spawn(move || shared.run_down())?,
            );
        }
        {
            let held = gateway.shared.clone();
            gateway.threads.push(std::thread::Builder::new().name("vm-net-held".into())
                .spawn(move || held.run_held())?);
        }
        Ok(gateway)
    }

    /// Switch the upstream (another node, or direct). Flows in progress are
    /// cut; the VM's NIC stays up.
    pub fn set_upstream(&self, upstream: &Upstream) -> Result<()> {
        let mut stacks = self.stacks.locked();
        // Validate the replacement before taking down the old upstream.
        let resolver = Arc::new(Resolver::start(upstream)?);
        let mode = {
            let mut egress = self.shared.egress.locked();
            stacks.suspend(&self.shared);
            egress.invalidate_lanes();
            egress.discard_held();
            egress.ordinary_probe_syns.clear();
            std::mem::replace(&mut egress.mode, EgressMode::Closed)
        };
        stacks.close();
        *self.shared.error.locked() = None;
        let previous_upstream = stacks.upstream.replace(upstream.clone());
        let previous_resolver = self.shared.resolver.locked().replace(resolver);
        if let Err(error) = stacks.activate(&self.shared, mode) {
            stacks.close();
            stacks.upstream = previous_upstream;
            *self.shared.resolver.locked() = previous_resolver;
            *self.shared.error.locked() = None;
            if let Err(rollback) = stacks.activate(&self.shared, mode) {
                stacks.close();
                let message = format!("{error}; could not restore the previous network path: {rollback}");
                *self.shared.error.locked() = Some(message.clone());
                return Err(Error::Message(message));
            }
            return Err(error);
        }
        Ok(())
    }

    /// Tear down the internet path while keeping the VM and SSH relay connected.
    pub fn clear_upstream(&self) {
        let mut stacks = self.stacks.locked();
        {
            let mut egress = self.shared.egress.locked();
            stacks.suspend(&self.shared);
            egress.invalidate_lanes();
            egress.discard_held();
            egress.ordinary_probe_syns.clear();
            egress.mode = EgressMode::Closed;
        }
        stacks.close();
        stacks.upstream = None;
        self.shared.resolver.locked().take();
        *self.shared.error.locked() = None;
    }

    /// Apply the guest internet policy. Hold modes preserve ordinary TCP
    /// relays and queue outbound packets until Open. Closed cuts all flows.
    pub fn set_egress_mode(&self, mode: EgressMode) {
        let mut stacks = self.stacks.locked();
        {
            let mut egress = self.shared.egress.locked();
            if egress.mode == mode {
                return;
            }
            stacks.suspend(&self.shared);
            egress.invalidate_lanes();
            if !mode.holds_ordinary() && mode != EgressMode::Open {
                egress.discard_held();
            }
            if mode == EgressMode::Closed {
                egress.ordinary_probe_syns.clear();
            }
            // Keep accepting ordinary packets while a Hold transition starts
            // the next stack or tears down the verification stack.
            egress.mode = if mode.holds_ordinary()
                || (mode == EgressMode::Open && egress.mode.holds_ordinary())
            {
                EgressMode::Suspended
            } else {
                EgressMode::Closed
            };
        }
        if let Err(error) = stacks.activate(&self.shared, mode) {
            stacks.close();
            let mut egress = self.shared.egress.locked();
            egress.mode = EgressMode::Closed;
            egress.discard_held();
            egress.ordinary_probe_syns.clear();
            *self.shared.error.locked() = Some(error.to_string());
        }
    }

    /// Guest internet IPv4 bytes handed to the gateway and delivered back to
    /// the guest, excluding management, verification and host proxy traffic.
    pub fn traffic(&self) -> GatewayTraffic {
        GatewayTraffic {
            rx_bytes: self.shared.rx_bytes.load(Ordering::Relaxed),
            tx_bytes: self.shared.tx_bytes.load(Ordering::Relaxed),
        }
    }

    /// The stack's last error (it stopped), if any.
    pub fn error(&self) -> Option<String> {
        self.shared
            .error
            .locked()
            .clone()
    }

    /// Where the Mac side reaches the guest's SSH (may differ from the
    /// requested address when that port was taken).
    pub fn ssh_addr(&self) -> Option<std::net::SocketAddr> {
        self.forward.as_ref().map(|forward| forward.local_addr())
    }

    /// Loopback HTTP entry point for host-side subscription downloads. It
    /// follows the same upstream as the guest gateway.
    pub fn proxy_url(&self) -> Option<String> {
        self.shared
            .resolver
            .locked()
            .as_ref()
            .map(|resolver| format!("http://127.0.0.1:{}", resolver.port))
    }

    /// Whether QEMU is connected right now.
    pub fn connected(&self) -> bool {
        self.shared
            .guest
            .locked()
            .is_some()
    }
}

/// Removes the gateway socket path after the listener has closed.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        {
            // The held worker checks stop under this same mutex before waiting.
            let _egress = self.shared.egress.locked();
            self.shared.stop.store(true, Ordering::Relaxed);
            self.shared.held_ready.notify_one();
        }
        let _ = self.shared.down_wake.0.send(&[0]);
        self.set_egress_mode(EgressMode::Closed);
        *self
            .shared
            .resolver
            .locked() = None;
        if let Some(guest) = self
            .shared
            .guest
            .locked()
            .take()
        {
            let _ = guest.shutdown(std::net::Shutdown::Both);
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        self.forward = None;
    }
}

#[cfg(test)]
mod testutil;

#[cfg(test)]
mod tests;
