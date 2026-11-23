//! The packet relay at runtime: the shoes stacks on the packet socket pairs,
//! held traffic, and the state shared by the gateway's threads.

use super::chain::{Upstream, shoes_config};
use super::dns::{Resolver, bound_dns, dns_ipv6_nodata, sanitize_dns_query};
use super::frame::{ipv4_udp, to_guest_frame, write_frame};
use super::policy::{Lane, hold_packet, outbound_probe_tcp};
use super::EgressMode;
use crate::util::LockExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use shoes::embed::RelayGate;

use crate::sockets::UnixStream;
use crate::transport::{Readiness, packet_pair};
use crate::{Error, Result};

/// A running proxy stack (shoes) on our end of the packet socket pair.
pub(super) struct Stack {
    gate: RelayGate,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stack {
    fn start(device: UdpSocket, upstream: &Upstream, error: Arc<Mutex<Option<String>>>) -> Result<Self> {
        let stack = Self::with_config_dns(shoes_config(upstream)?, error.clone(), bound_dns(upstream)?, device)?;
        // Configuration and socket errors are reported by the stack thread.
        // Surface immediate failures before considering a switch successful.
        std::thread::sleep(Duration::from_millis(200));
        let failure = error.locked().clone();
        if let Some(message) = failure {
            drop(stack);
            return Err(Error::Message(message));
        }
        Ok(stack)
    }

    #[cfg(test)]
    fn with_config(config: String, error: Arc<Mutex<Option<String>>>, device: UdpSocket) -> Result<Self> {
        Self::with_config_dns(config, error, None, device)
    }

    fn with_config_dns(
        config: String,
        error: Arc<Mutex<Option<String>>>,
        dns: Option<shoes::embed::BoundDns>,
        device: UdpSocket,
    ) -> Result<Self> {
        let (shutdown, shutdown_rx) = tokio::sync::oneshot::channel();
        let gate = RelayGate::default();
        let stack_gate = gate.clone();
        let thread = std::thread::Builder::new()
            .name("vm-net-stack".into())
            .spawn(move || {
                let outcome = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())
                    .and_then(|runtime| {
                        runtime.block_on(async {
                            if let Some(dns) = dns {
                                shoes::embed::run_tun_controlled_with_bound_dns(&config, shutdown_rx, stack_gate, dns, device).await
                            } else {
                                shoes::embed::run_tun_controlled(&config, shutdown_rx, stack_gate, device).await
                            }
                        }).map_err(|e| e.to_string())
                    });
                if let Err(message) = outcome {
                    *error.locked() = Some(message);
                }
            })?;
        Ok(Self {
            gate,
            shutdown: Some(shutdown),
            thread: Some(thread),
        })
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(super) struct PacketPath {
    pub(super) host: UdpSocket,
    pub(super) stack: UdpSocket,
}

impl PacketPath {
    pub(super) fn new() -> Result<Self> {
        let (host, stack) = packet_pair()?;
        Ok(Self { host, stack })
    }

    fn clear(&self) {
        let mut scratch = [0u8; 2048];
        for socket in [&self.host, &self.stack] {
            while socket.recv(&mut scratch).is_ok() {}
        }
    }
}

// Cap each queue independently so a burst of one protocol cannot use all slots.
pub(super) const HELD_PACKETS: usize = 256;
pub(super) const HELD_DNS: usize = 64;
const DNS_INFLIGHT: usize = 32;
pub(super) const MAX_DNS_QUERY: usize = 4096;

pub(super) struct DnsRequest {
    pub(super) src: Ipv4Addr,
    pub(super) dst: Ipv4Addr,
    pub(super) port: u16,
    pub(super) ip_len: usize,
    pub(super) query: Vec<u8>,
}

impl DnsRequest {
    fn same_query(&self, other: &Self) -> bool {
        self.src == other.src && self.dst == other.dst && self.port == other.port
            && self.query == other.query
    }
}

pub(super) struct GuestEgress {
    pub(super) mode: EgressMode,
    pub(super) dns_gate: RelayGate,
    pub(super) probe_flows: HashSet<(Ipv4Addr, u16)>,
    pub(super) ordinary_probe_syns: HashMap<(Ipv4Addr, u16), [u8; 4]>,
    pub(super) held_packets: VecDeque<Vec<u8>>,
    pub(super) held_dns: VecDeque<DnsRequest>,
    pub(super) dns_inflight: usize,
}

impl GuestEgress {
    pub(super) fn invalidate_lanes(&mut self) {
        self.dns_gate.suspend(|| {});
        self.probe_flows.clear();
    }

    pub(super) fn discard_held(&mut self) {
        self.held_packets.clear();
        self.held_dns.clear();
    }

    fn record_ordinary_syn(&mut self, packet: &[u8]) {
        if let Some((flow, tcp)) = outbound_probe_tcp(packet)
            && tcp[13] & 0x12 == 0x02
        {
            self.ordinary_probe_syns.insert(flow, tcp[4..8].try_into().unwrap());
        }
    }
}

pub(super) struct Shared {
    pub(super) dns: Vec<Ipv4Addr>,
    pub(super) guest: Mutex<Option<Arc<UnixStream>>>,
    pub(super) egress: Mutex<GuestEgress>,
    pub(super) held_ready: Condvar,
    pub(super) stop: AtomicBool,
    pub(super) error: Arc<Mutex<Option<String>>>,
    pub(super) resolver: Mutex<Option<Arc<Resolver>>>,
    pub(super) paths: [PacketPath; 2],
    pub(super) rx_bytes: AtomicU64,
    pub(super) tx_bytes: AtomicU64,
}

pub(super) struct GatewayStacks {
    pub(super) upstream: Option<Upstream>,
    pub(super) active: [Option<Stack>; 2],
    pub(super) ordinary_opened: bool,
}

impl Shared {
    pub(super) fn write_guest(&self, frame: &[u8]) -> bool {
        let guest = self.guest.locked();
        let Some(stream) = guest.as_ref() else {
            return false;
        };
        if write_frame(&mut &**stream, frame).is_ok() {
            true
        } else {
            // A partial frame would make later writes unreadable.
            let _ = stream.shutdown(std::net::Shutdown::Both);
            false
        }
    }

    pub(super) fn disconnect_guest(&self) {
        let mut egress = self.egress.locked();
        *self.guest.locked() = None;
        egress.invalidate_lanes();
        egress.discard_held();
        egress.ordinary_probe_syns.clear();
        if egress.mode.lane().is_some() {
            egress.dns_gate.resume();
        }
    }

    pub(super) fn send_ip(&self, packet: Vec<u8>) {
        let mut egress = self.egress.locked();
        let mode = egress.mode;
        if mode.lane() == Some(Lane::Verification)
            && let Some((flow, tcp)) = outbound_probe_tcp(&packet)
        {
            // Only a new SYN can enter verification. A retransmission of a
            // known ordinary SYN must stay with its ordinary connection.
            let allowed = if tcp[13] & 0x12 == 0x02 {
                !mode.holds_ordinary()
                    || egress.ordinary_probe_syns.get(&flow).map(|seq| seq.as_slice()) != Some(&tcp[4..8])
            } else {
                egress.probe_flows.contains(&flow)
            };
            if allowed {
                if self.paths[Lane::Verification as usize].host.send(&packet).is_ok() {
                    egress.probe_flows.insert(flow);
                }
                return;
            }
        }
        if (mode.holds_ordinary() || (mode == EgressMode::Open && !egress.held_packets.is_empty()))
            && hold_packet(&packet)
        {
            egress.record_ordinary_syn(&packet);
            if egress.held_packets.len() < HELD_PACKETS {
                egress.held_packets.push_back(packet);
            }
        } else if mode.lane() == Some(Lane::Ordinary) && mode.allows_packet(&packet, false) {
            egress.record_ordinary_syn(&packet);
            let _ = self.send_ordinary_ip(&packet);
        }
    }

    fn send_ordinary_ip(&self, packet: &[u8]) -> std::io::Result<()> {
        self.paths[Lane::Ordinary as usize].host.send(packet)?;
        self.tx_bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);
        Ok(())
    }

    fn receive_ip(&self, lane: Lane, buffer: &mut [u8]) {
        let egress = self.egress.locked();
        let Ok(len) = self.paths[lane as usize].host.recv(buffer) else {
            return;
        };
        if egress.mode.lane() != Some(lane) {
            return;
        }
        let packet = &buffer[..len];
        if egress.mode.allows_packet(packet, true)
            && self.write_guest(&to_guest_frame(packet))
            && lane == Lane::Ordinary
        {
            self.rx_bytes.fetch_add(len as u64, Ordering::Relaxed);
        }
    }

    pub(super) fn run_down(&self) {
        let _ = Readiness::scope(|ready| {
            let lanes = [Lane::Ordinary, Lane::Verification];
            for lane in lanes {
                ready.watch(lane as usize, &self.paths[lane as usize].host, true, false)?;
            }
            let mut packet = vec![0u8; 65_536];
            while !self.stop.load(Ordering::Relaxed) {
                let Ok(events) = ready.wait(Some(Duration::from_millis(500))) else {
                    // A persistent poller error can return immediately on every wait.
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                };
                for lane in lanes {
                    if events.iter().any(|event| event.key == lane as usize && event.readable) {
                        self.receive_ip(lane, &mut packet);
                    }
                }
            }
            Ok(())
        });
    }

    pub(super) fn run_held(self: &Arc<Self>) {
        let mut egress = self.egress.locked();
        while !self.stop.load(Ordering::Relaxed) {
            if egress.mode == EgressMode::Open {
                let mut progressed = false;
                if let Some(packet) = egress.held_packets.front() {
                    let retry = self.send_ordinary_ip(packet).is_err_and(|error| {
                        matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted)
                            || error.raw_os_error() == Some(libc::ENOBUFS)
                    });
                    if !retry {
                        egress.held_packets.pop_front();
                        progressed = true;
                    }
                }
                if egress.dns_inflight < DNS_INFLIGHT
                    && let Some(request) = egress.held_dns.pop_front()
                {
                    self.start_dns(&mut egress, request);
                    progressed = true;
                }
                if progressed {
                    // Let mode changes, disconnects and new frames run between sends.
                    drop(egress);
                    egress = self.egress.locked();
                    continue;
                }
                if !egress.held_packets.is_empty() {
                    // A full Unix datagram socket need not report its peer's
                    // backpressure through POLLOUT on both supported platforms.
                    (egress, _) = self.held_ready.wait_timeout(egress, Duration::from_millis(5))
                        .unwrap_or_else(|p| p.into_inner());
                    continue;
                }
            }
            egress = self.held_ready.wait(egress).unwrap_or_else(|p| p.into_inner());
        }
    }

    pub(super) fn resolve(self: &Arc<Self>, mut request: DnsRequest) {
        if request.query.len() > MAX_DNS_QUERY || !sanitize_dns_query(&mut request.query) {
            return;
        }
        let mut egress = self.egress.locked();
        let mode = egress.mode;
        let has_held_dns = !egress.held_dns.is_empty();
        // A retransmitted ordinary query stays held even if its name is on
        // the verification allowlist.
        if mode == EgressMode::Rechecking
            && egress.held_dns.iter().any(|held| held.same_query(&request))
        {
            return;
        }
        if !(mode == EgressMode::Open && has_held_dns) && mode.allows_dns(&mut request.query) {
            self.start_dns(&mut egress, request);
        } else if (mode.holds_ordinary() || (mode == EgressMode::Open && has_held_dns))
            && egress.held_dns.len() < HELD_DNS
        {
            egress.held_dns.push_back(request);
        }
    }

    fn start_dns(self: &Arc<Self>, egress: &mut GuestEgress, request: DnsRequest) {
        let DnsRequest { src, dst, port, ip_len, query } = request;
        let Some(ticket) = egress.dns_gate.ticket() else { return };
        let counted = egress.mode.lane() == Some(Lane::Ordinary);
        if let Some(answer) = dns_ipv6_nodata(&query) {
            let packet = ipv4_udp(dst, src, 53, port, &answer);
            if self.write_guest(&to_guest_frame(&packet)) && counted {
                self.tx_bytes.fetch_add(ip_len as u64, Ordering::Relaxed);
                self.rx_bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);
            }
            return;
        }
        if egress.dns_inflight >= DNS_INFLIGHT {
            return;
        }
        let Some(resolver) = self.resolver.locked().clone() else {
            return;
        };
        let shared = self.clone();
        let task = std::thread::Builder::new()
            .name("vm-net-dns".into())
            .spawn(move || {
                let answer = resolver.lookup(&ticket, &query);
                let mut egress = shared.egress.locked();
                if let Some(answer) = answer && ticket.is_current() {
                    let packet = ipv4_udp(dst, src, 53, port, &answer);
                    if shared.write_guest(&to_guest_frame(&packet)) && counted {
                        shared.rx_bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);
                    }
                }
                egress.dns_inflight -= 1;
                if egress.mode == EgressMode::Open && !egress.held_dns.is_empty() {
                    shared.held_ready.notify_one();
                }
            });
        if task.is_ok() {
            egress.dns_inflight += 1;
            if counted {
                self.tx_bytes.fetch_add(ip_len as u64, Ordering::Relaxed);
            }
        }
    }
}

impl GatewayStacks {
    pub(super) fn suspend(&self, shared: &Shared) {
        for (slot, path) in self.active.iter().zip(&shared.paths) {
            if let Some(stack) = slot {
                stack.gate.suspend(|| path.clear());
            }
        }
    }

    pub(super) fn close(&mut self) {
        self.active = [None, None];
        self.ordinary_opened = false;
    }

    pub(super) fn activate(&mut self, shared: &Shared, mode: EgressMode) -> Result<()> {
        let mode = if self.upstream.is_some() { mode } else { EgressMode::Closed };
        if mode == EgressMode::Closed {
            self.close();
        }
        // Each verification policy starts with fresh TCP tuples.
        self.active[Lane::Verification as usize] = None;
        if mode == EgressMode::Install && self.ordinary_opened {
            self.active[Lane::Ordinary as usize] = None;
            self.ordinary_opened = false;
        }
        if let (Some(lane), Some(upstream)) = (mode.lane(), &self.upstream) {
            let slot = &mut self.active[lane as usize];
            if slot.is_none() {
                *shared.error.locked() = None;
                let started = Stack::start(
                    shared.paths[lane as usize].stack.try_clone()?,
                    upstream,
                    shared.error.clone(),
                );
                *slot = Some(started?);
            }
        }
        let stack = mode.lane().map(|lane| {
            self.active[lane as usize].as_ref()
                .ok_or_else(|| Error::Message("No network stack for the selected egress mode".into()))
        }).transpose()?;
        self.ordinary_opened |= mode == EgressMode::Open;
        let mut egress = shared.egress.locked();
        egress.mode = mode;
        if let Some(stack) = stack {
            stack.gate.resume();
            egress.dns_gate.resume();
            if let (Some(ticket), Some(resolver)) = (egress.dns_gate.ticket(), shared.resolver.locked().clone())
                && resolver.warm_on_resume
            {
                let _ = resolver.warm(ticket);
            }
        }
        if mode == EgressMode::Open {
            shared.held_ready.notify_one();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netgw::testutil::*;
    use crate::netgw::GUEST_IP;
    use crate::netgw::chain::shoes_config_blocking;

    /// shoes' TUN stack over a datagram socket pair: a SYN from the "VM"
    /// becomes a real TCP connection from the host, and the SYN-ACK comes
    /// back as a packet. (Private destinations are normally blocked; this
    /// test allows them so it can use a local server.)
    #[test]
    fn stack_turns_packets_into_real_connections() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let (ours, theirs) = packet_pair().unwrap();
        let config = shoes_config_blocking(&Upstream::Direct, &[]).unwrap();
        let error = Arc::new(Mutex::new(None));
        let _stack = Stack::with_config(config, error.clone(), theirs).unwrap();
        std::thread::sleep(Duration::from_millis(300));

        ours.send(&tcp_syn(GUEST_IP, 40000, Ipv4Addr::LOCALHOST, port))
            .unwrap();
        server.set_nonblocking(true).unwrap();
        let started = std::time::Instant::now();
        let accepted = loop {
            if let Ok((stream, _)) = server.accept() {
                break stream;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "no connection; stack error: {:?}",
                error.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(accepted);

        ours.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut packet = vec![0u8; 2048];
        let len = ours.recv(&mut packet).expect("a reply packet");
        let reply = &packet[..len];
        assert_eq!(reply[0] >> 4, 4, "IPv4");
        assert_eq!(reply[9], 6, "TCP");
        assert_eq!(&reply[16..20], &GUEST_IP.octets(), "addressed to the VM");
        assert_eq!(reply[20 + 13] & 0x12, 0x12, "SYN-ACK");
    }

    #[test]
    fn stack_blocks_private_destinations_itself() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        server.set_nonblocking(true).unwrap();
        let (ours, theirs) = packet_pair().unwrap();
        let config = shoes_config(&Upstream::Direct).unwrap();
        let _stack = Stack::with_config(config, Arc::new(Mutex::new(None)), theirs).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        ours.send(&tcp_syn(GUEST_IP, 40001, Ipv4Addr::LOCALHOST, port))
            .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            server.accept().is_err(),
            "a blocked destination was connected"
        );
    }
}
