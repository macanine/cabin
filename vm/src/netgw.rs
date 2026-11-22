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

use crate::network_probe_targets::{ALTERNATE_PROBE_V4, PUBLIC_PROBE_V4};
use crate::util::LockExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use shoes::embed::{HttpRelay, RelayGate, RelayTicket};

use crate::compat::{POLLIN, PollItem, WakeSocket, poll, poll_in};
use crate::sockets::{DuplexStream, Endpoint};
use crate::{Error, Result};

pub const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 3, 1);
pub const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 3, 15);
pub const NETMASK: Ipv4Addr = Ipv4Addr::new(255, 255, 255, 0);
pub const GATEWAY_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x0a, 0x03, 0x01];
/// MAC of the VM's internet NIC (QEMU `-device …,mac=`).
pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x0a, 0x03, 0x0f];
pub const MTU: usize = 1500;
/// Used when no DNS servers are configured.
pub const DEFAULT_DNS: [Ipv4Addr; 2] = [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)];

/// The traffic states owned by the desktop app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressMode {
    /// Cut all guest internet flows.
    Closed,
    /// Pause ordinary TCP relays and hold new ordinary outbound packets.
    Suspended,
    /// Hold ordinary traffic while allowing the fixed verification targets.
    Rechecking,
    /// Allow DNS and package downloads during first boot.
    Install,
    /// Allow only the fixed guest verification targets on a separate stack.
    Checking,
    Open,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Lane {
    Ordinary = 0,
    Verification = 1,
}

impl EgressMode {
    pub fn checking() -> Self {
        Self::Checking
    }

    fn lane(self) -> Option<Lane> {
        match self {
            Self::Open | Self::Install => Some(Lane::Ordinary),
            Self::Checking | Self::Rechecking => Some(Lane::Verification),
            Self::Closed | Self::Suspended => None,
        }
    }

    fn holds_ordinary(self) -> bool {
        matches!(self, Self::Suspended | Self::Rechecking)
    }

    fn allows_packet(self, packet: &[u8], to_guest: bool) -> bool {
        // Both sides of the packet socket must remain IPv4-only, even in Open.
        if packet.len() < 20 || packet[0] >> 4 != 4 {
            return false;
        }
        match self {
            Self::Open => true,
            Self::Install => tcp_packet(packet)
                .is_some_and(|(ip, tcp)| matches!(remote_endpoint(ip, tcp, to_guest).1, 80 | 443)),
            Self::Checking | Self::Rechecking => tcp_packet(packet).is_some_and(|(ip, tcp)| {
                verification_endpoint(remote_endpoint(ip, tcp, to_guest))
            }),
            Self::Closed | Self::Suspended => false,
        }
    }

    fn allows_dns(self, query: &mut Vec<u8>) -> bool {
        match self {
            Self::Open | Self::Install => dns_question(query).is_some(),
            Self::Checking | Self::Rechecking => restrict_verification_dns(query),
            Self::Closed | Self::Suspended => false,
        }
    }
}

/// Offset after one uncompressed DNS question, and its type and class.
fn dns_question(query: &[u8]) -> Option<(usize, u16, u16)> {
    if query.len() < 17 || query[2] & 0xf8 != 0 || query[4..6] != [0, 1]
        || query[6..10] != [0, 0, 0, 0] {
        return None;
    }
    let mut pos = 12;
    let mut name_len = 0usize;
    loop {
        let len = *query.get(pos)? as usize;
        pos += 1;
        name_len += len + 1;
        if len > 63 || name_len > 255 { return None; }
        if len == 0 { break; }
        pos = pos.checked_add(len)?;
        if pos > query.len() { return None; }
    }
    let end = pos.checked_add(4)?;
    let fields = query.get(pos..end)?;
    Some((end, u16::from_be_bytes([fields[0], fields[1]]), u16::from_be_bytes([fields[2], fields[3]])))
}

/// Keep one Internet question and a bare EDNS record when requested. EDNS
/// options may contain client addresses or other data the resolver needn't see.
fn sanitize_dns_query(query: &mut Vec<u8>) -> bool {
    let Some((end, _, 1)) = dns_question(query) else { return false };
    let opt = if query[10..12] == [0, 1] {
        query.get(end..end + 11).filter(|record| {
            record[..3] == [0, 0, 41]
                && record[5..7] == [0, 0]
                && query.len() == end + 11 + usize::from(u16::from_be_bytes([record[9], record[10]]))
        }).map(|record| {
            let size = u16::from_be_bytes([record[3], record[4]]).clamp(512, 1232).to_be_bytes();
            [0, 0, 41, size[0], size[1], 0, 0, record[7] & 0x80, 0, 0, 0]
        })
    } else {
        None
    };
    query.truncate(end);
    query[2] &= 1;
    query[3] &= 0x10; // checking disabled
    query[10..12].fill(0);
    if let Some(opt) = opt {
        query[11] = 1;
        query.extend_from_slice(&opt);
    }
    true
}

/// Only accept a DNS response to the question we sent; retry transient server failures.
fn valid_doh_response(query: &[u8], response: &[u8]) -> bool {
    let Some((end, _, _)) = dns_question(query) else { return false; };
    response.len() >= end
        && response[..2] == query[..2]
        && response[2] & 0xf8 == 0x80
        && matches!(response[3] & 0x0f, 0 | 3)
        && response[4..6] == [0, 1]
        && response[12..end].eq_ignore_ascii_case(&query[12..end])
}

/// Answer IPv6 address queries locally; no IPv6 answer or upstream request.
fn dns_ipv6_nodata(query: &[u8]) -> Option<Vec<u8>> {
    let (end, kind, _) = dns_question(query)?;
    if kind != 28 { return None; }
    let mut answer = query[..end].to_vec();
    answer[2] = 0x80 | (query[2] & 1); // response, preserve recursion desired
    answer[3] = 0x80; // recursion available, NOERROR
    answer[6..12].fill(0);
    Some(answer)
}

fn verification_endpoint((ip, port): (Ipv4Addr, u16)) -> bool {
    port == 443 && (ip == PUBLIC_PROBE_V4 || ip == ALTERNATE_PROBE_V4)
}

fn restrict_verification_dns(query: &mut Vec<u8>) -> bool {
    const NAME: &[u8] = b"\x03one\x03one\x03one\x03one\x00";
    let name_end = 12 + NAME.len();
    let question_end = name_end + 4;
    if query.len() < question_end
        || query[2] & 0xf8 != 0
        || query[4..6] != [0, 1]
        || !query[12..name_end].eq_ignore_ascii_case(NAME)
        || !matches!(&query[name_end..question_end], [0, 1 | 28, 0, 1])
    {
        return false;
    }
    // Forward exactly the fixed question, even when the guest sends EDNS options.
    query.truncate(question_end);
    query[2] &= 1; // recursion desired
    query[3] = 0;
    query[6..12].fill(0);
    true
}

fn tcp_packet(packet: &[u8]) -> Option<(&[u8], &[u8])> {
    if packet.len() < 20 || packet[0] >> 4 != 4 || packet[9] != 6 {
        return None;
    }
    let header = usize::from(packet[0] & 0x0f) * 4;
    let total = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if header < 20
        || total < header + 20
        || total > packet.len()
        || u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff != 0
    {
        return None;
    }
    Some(packet.split_at(header))
}

fn remote_endpoint(ip: &[u8], tcp: &[u8], to_guest: bool) -> (Ipv4Addr, u16) {
    let (addr, port) = if to_guest { (12, 0) } else { (16, 2) };
    (
        Ipv4Addr::from(<[u8; 4]>::try_from(&ip[addr..addr + 4]).unwrap()),
        u16::from_be_bytes([tcp[port], tcp[port + 1]]),
    )
}

/// Destinations the VM may never reach (LAN, the Mac, link-local, …).
pub const BLOCKED: [&str; 10] = [
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "198.18.0.0/15",
    "224.0.0.0/4",
    "240.0.0.0/4",
];

const ETH_IPV4: u16 = 0x0800;
const ETH_ARP: u16 = 0x0806;

fn blocked(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 0
        || a == 10
        || (a == 100 && (64..128).contains(&b))
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..32).contains(&b))
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || a >= 224
}

// ------------------------------------------------------------- frame logic

/// What to do with a frame from the VM.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Send this Ethernet frame back to the VM.
    ToGuest(Vec<u8>),
    /// A frame for the gateway's own TCP endpoint (SSH relay, `net_forward`).
    Forward(Vec<u8>),
    /// Hand this IPv4 packet to the proxy stack.
    ToStack(Vec<u8>),
    /// Resolve a guest UDP DNS query through the selected upstream.
    Dns {
        src: Ipv4Addr,
        dst: Ipv4Addr,
        port: u16,
        ip_len: usize,
        query: Vec<u8>,
    },
    Drop,
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum = 0u32;
    for chunk in data.chunks(2) {
        let word = u16::from_be_bytes([chunk[0], *chunk.get(1).unwrap_or(&0)]);
        sum += word as u32;
    }
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn ethernet(dst: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(14 + payload.len());
    frame.extend_from_slice(&dst);
    frame.extend_from_slice(&GATEWAY_MAC);
    frame.extend_from_slice(&ethertype.to_be_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Wrap an IPv4 packet from the stack for the VM.
pub fn to_guest_frame(packet: &[u8]) -> Vec<u8> {
    ethernet(GUEST_MAC, ETH_IPV4, packet)
}

fn ipv4_udp(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let udp_len = 8 + payload.len();
    let total = 20 + udp_len;
    let mut packet = vec![0u8; total];
    packet[0] = 0x45;
    packet[2..4].copy_from_slice(&(total as u16).to_be_bytes());
    packet[8] = 64;
    packet[9] = 17;
    packet[12..16].copy_from_slice(&src.octets());
    packet[16..20].copy_from_slice(&dst.octets());
    let sum = checksum(&packet[..20]);
    packet[10..12].copy_from_slice(&sum.to_be_bytes());
    packet[20..22].copy_from_slice(&sport.to_be_bytes());
    packet[22..24].copy_from_slice(&dport.to_be_bytes());
    packet[24..26].copy_from_slice(&(udp_len as u16).to_be_bytes());
    // UDP checksum 0 = none (allowed for IPv4).
    packet[28..].copy_from_slice(payload);
    packet
}

fn arp_reply(request: &[u8]) -> Option<Vec<u8>> {
    // htype 1, ptype IPv4, hlen 6, plen 4, op 1 (request)
    if request.len() < 28 || request[0..8] != [0, 1, 8, 0, 6, 4, 0, 1] {
        return None;
    }
    let sender_mac: [u8; 6] = request[8..14].try_into().ok()?;
    let sender_ip = &request[14..18];
    let target_ip = Ipv4Addr::new(request[24], request[25], request[26], request[27]);
    if target_ip != GATEWAY_IP {
        return None;
    }
    let mut reply = Vec::with_capacity(28);
    reply.extend_from_slice(&[0, 1, 8, 0, 6, 4, 0, 2]);
    reply.extend_from_slice(&GATEWAY_MAC);
    reply.extend_from_slice(&GATEWAY_IP.octets());
    reply.extend_from_slice(&sender_mac);
    reply.extend_from_slice(sender_ip);
    Some(ethernet(sender_mac, ETH_ARP, &reply))
}

/// Seconds between the NTP era (1900) and the Unix epoch.
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

fn ntp_timestamp(time: std::time::SystemTime) -> [u8; 8] {
    let since = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = (since.as_secs() + NTP_UNIX_OFFSET) as u32;
    let fraction = ((since.subsec_nanos() as u64) << 32) / 1_000_000_000;
    let mut out = [0u8; 8];
    out[..4].copy_from_slice(&seconds.to_be_bytes());
    out[4..].copy_from_slice(&(fraction as u32).to_be_bytes());
    out
}

/// SNTP server reply (RFC 4330) carrying the Mac's clock.
pub fn ntp_reply(request: &[u8], now: std::time::SystemTime) -> Option<Vec<u8>> {
    // Client mode (3), any version 1–4.
    if request.len() < 48 || request[0] & 0x07 != 3 {
        return None;
    }
    let version = (request[0] >> 3) & 0x07;
    if !(1..=4).contains(&version) {
        return None;
    }
    let stamp = ntp_timestamp(now);
    let mut reply = vec![0u8; 48];
    reply[0] = (version << 3) | 4; // no leap warning, server mode
    reply[1] = 2; // stratum: one step below the Mac's own servers
    reply[2] = request[2]; // poll
    reply[3] = (-20i8) as u8; // precision ≈ 1 µs
    reply[8..12].copy_from_slice(&[0, 0, 0, 0x41]); // root dispersion ≈ 1 ms
    reply[12..16].copy_from_slice(&GATEWAY_IP.octets()); // reference id
    reply[16..24].copy_from_slice(&stamp); // reference
    reply[24..32].copy_from_slice(&request[40..48]); // origin = client's transmit
    reply[32..40].copy_from_slice(&stamp); // receive
    reply[40..48].copy_from_slice(&stamp); // transmit
    Some(reply)
}

fn dhcp_reply(request: &[u8], dns: &[Ipv4Addr]) -> Option<Vec<u8>> {
    // BOOTP fixed part is 236 bytes, then the magic cookie.
    if request.len() < 240 || request[0] != 1 || request[236..240] != [99, 130, 83, 99] {
        return None;
    }
    let mut kind = None;
    let mut i = 240;
    while i < request.len() && request[i] != 255 {
        if request[i] == 0 {
            i += 1;
            continue;
        }
        let len = *request.get(i + 1)? as usize;
        if request[i] == 53 {
            kind = request.get(i + 2).copied();
        }
        i += 2 + len;
    }
    let reply_kind = match kind? {
        1 => 2, // DISCOVER → OFFER
        3 => 5, // REQUEST → ACK
        _ => return None,
    };
    let mut reply = vec![0u8; 240];
    reply[0] = 2; // BOOTREPLY
    reply[1] = 1;
    reply[2] = 6;
    reply[4..8].copy_from_slice(&request[4..8]); // xid
    reply[10..12].copy_from_slice(&request[10..12]); // flags
    reply[16..20].copy_from_slice(&GUEST_IP.octets()); // yiaddr
    reply[20..24].copy_from_slice(&GATEWAY_IP.octets()); // siaddr
    reply[28..44].copy_from_slice(&request[28..44]); // chaddr
    reply[236..240].copy_from_slice(&[99, 130, 83, 99]);
    let mut option = |code: u8, value: &[u8]| {
        reply.push(code);
        reply.push(value.len() as u8);
        reply.extend_from_slice(value);
    };
    option(53, &[reply_kind]);
    option(54, &GATEWAY_IP.octets());
    option(51, &86400u32.to_be_bytes());
    option(1, &NETMASK.octets());
    option(3, &GATEWAY_IP.octets());
    let servers: Vec<u8> = dns.iter().flat_map(|ip| ip.octets()).collect();
    option(6, &servers);
    option(26, &(MTU as u16).to_be_bytes());
    option(42, &GATEWAY_IP.octets()); // NTP: the gateway itself
    reply.push(255);
    let packet = ipv4_udp(GATEWAY_IP, Ipv4Addr::BROADCAST, 67, 68, &reply);
    Some(ethernet([0xff; 6], ETH_IPV4, &packet))
}

/// Decide what happens to one Ethernet frame from the VM.
pub fn handle_frame(frame: &[u8], dns: &[Ipv4Addr]) -> Action {
    if frame.len() < 14 {
        return Action::Drop;
    }
    let payload = &frame[14..];
    match u16::from_be_bytes([frame[12], frame[13]]) {
        ETH_ARP => match arp_reply(payload) {
            Some(reply) => Action::ToGuest(reply),
            // The guest answering the relay's ARP request.
            None if payload.len() >= 28
                && payload[6..8] == [0, 2]
                && payload[24..28] == GATEWAY_IP.octets() =>
            {
                Action::Forward(frame.to_vec())
            }
            None => Action::Drop,
        },
        ETH_IPV4 => {
            if payload.len() < 20 || payload[0] >> 4 != 4 {
                return Action::Drop;
            }
            let header = ((payload[0] & 0x0f) as usize) * 4;
            let total = u16::from_be_bytes([payload[2], payload[3]]) as usize;
            if header < 20 || total < header || total > payload.len() {
                return Action::Drop;
            }
            let packet = &payload[..total];
            let protocol = packet[9];
            let dst = Ipv4Addr::new(packet[16], packet[17], packet[18], packet[19]);
            // DHCP to the gateway (broadcast before the VM has an address).
            if protocol == 17 && packet.len() >= header + 8 {
                let dport = u16::from_be_bytes([packet[header + 2], packet[header + 3]]);
                if dport == 67 {
                    return dhcp_reply(&packet[header + 8..], dns)
                        .map_or(Action::Drop, Action::ToGuest);
                }
                let src = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
                let udp_len = u16::from_be_bytes([packet[header + 4], packet[header + 5]]) as usize;
                if dport == 53
                    && src == GUEST_IP
                    && !blocked(dst)
                    && udp_len >= 20
                    && header + udp_len <= packet.len()
                    && u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff == 0
                {
                    return Action::Dns {
                        src,
                        dst,
                        port: u16::from_be_bytes([packet[header], packet[header + 1]]),
                        ip_len: total,
                        query: packet[header + 8..header + udp_len].to_vec(),
                    };
                }
            }
            if dst == GATEWAY_IP && protocol == 17 && packet.len() >= header + 8 {
                let dport = u16::from_be_bytes([packet[header + 2], packet[header + 3]]);
                if dport == 123 {
                    let src = Ipv4Addr::new(packet[12], packet[13], packet[14], packet[15]);
                    let sport = u16::from_be_bytes([packet[header], packet[header + 1]]);
                    return ntp_reply(&packet[header + 8..], std::time::SystemTime::now())
                        .map(|reply| to_guest_frame(&ipv4_udp(GATEWAY_IP, src, 123, sport, &reply)))
                        .map_or(Action::Drop, Action::ToGuest);
                }
            }
            if dst == GATEWAY_IP && protocol == 6 {
                return Action::Forward(frame.to_vec());
            }
            // The gateway offers nothing else; private ranges are off limits.
            if dst == GATEWAY_IP || dst == Ipv4Addr::BROADCAST || blocked(dst) {
                return Action::Drop;
            }
            Action::ToStack(packet.to_vec())
        }
        // IPv6 and everything else: the VM is IPv4-only to the internet.
        _ => Action::Drop,
    }
}

// --------------------------------------------------------------- upstream

/// Where the VM's internet traffic goes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Upstream {
    /// Straight out from the Mac (still isolated from LAN and the Mac).
    Direct,
    /// Through shoes client hops (subscription nodes), entry first; each hop
    /// is reached through the ones before it.
    Chain(Vec<serde_yaml::Value>),
    /// Direct traffic bound to a host interface.
    BoundDirect(String),
    /// A proxy chain whose first hop is bound to a host interface.
    BoundChain(Vec<serde_yaml::Value>, String),
}

impl Upstream {
    fn interface(&self) -> Option<&str> {
        match self {
            Self::BoundDirect(name) | Self::BoundChain(_, name) => Some(name),
            _ => None,
        }
    }
}

/// Interface-bound DNS bootstrap. Unix-only: shoes' interface binding has no
/// Windows counterpart, so adapter-scoped routes are rejected up front there.
#[cfg(unix)]
fn bound_dns(upstream: &Upstream) -> Result<Option<shoes::embed::BoundDns>> {
    upstream.interface().map(|name| {
        let servers = [([223, 5, 5, 5], 53), ([119, 29, 29, 29], 53), ([1, 1, 1, 1], 53)]
            .into_iter().map(std::net::SocketAddr::from).collect();
        shoes::embed::BoundDns::new(name.to_string(), servers).map_err(Error::Io)
    }).transpose()
}

#[cfg(not(unix))]
fn bound_dns(_upstream: &Upstream) -> Result<Option<shoes::embed::BoundDns>> {
    Ok(None)
}

/// The shoes `client_chain` for `upstream`: direct, one hop, or a sequence.
fn client_chain(upstream: &Upstream) -> Result<serde_yaml::Value> {
    #[cfg(unix)]
    if let Some(name) = upstream.interface() {
        crate::network_interfaces::require_available(name)?;
    }
    #[cfg(not(unix))]
    if upstream.interface().is_some() {
        return Err(Error::Message(
            "Selecting a network adapter is not supported on Windows".into(),
        ));
    }
    client_chain_value(upstream)
}

fn client_chain_value(upstream: &Upstream) -> Result<serde_yaml::Value> {
    use serde_yaml::{Mapping, Value};
    let string = |s: &str| Value::String(s.to_string());
    let (hops, interface) = match upstream {
        Upstream::Direct => (None, None),
        Upstream::BoundDirect(name) => (None, Some(name)),
        Upstream::Chain(hops) => (Some(hops), None),
        Upstream::BoundChain(hops, name) => (Some(hops), Some(name)),
    };
    let mut clean = if let Some(hops) = hops {
        hops.iter().map(|hop| {
            let mut hop = hop.as_mapping().cloned()
                .ok_or_else(|| Error::Message("invalid proxy node".into()))?;
            hop.remove(string("name"));
            hop.remove(string("bind_interface"));
            Ok(hop)
        }).collect::<Result<Vec<_>>>()?
    } else {
        let mut protocol = Mapping::new();
        protocol.insert(string("type"), string("direct"));
        let mut hop = Mapping::new();
        hop.insert(string("protocol"), Value::Mapping(protocol));
        vec![hop]
    };
    if clean.is_empty() {
        return Err(Error::Message("the route has no nodes".into()));
    }
    if let Some(name) = interface {
        clean[0].insert(string("bind_interface"), string(name));
    }
    let mut clean: Vec<_> = clean.into_iter().map(Value::Mapping).collect();
    if clean.len() == 1 { Ok(clean.remove(0)) } else { Ok(Value::Sequence(clean)) }
}

/// The shoes TUN config for `upstream` on packet descriptor `fd` (Unix).
#[cfg(unix)]
pub fn shoes_config(fd: i32, upstream: &Upstream) -> Result<String> {
    shoes_config_blocking(fd, upstream, &BLOCKED)
}

/// The shoes TUN config for `upstream` on Windows: the packet device is
/// passed separately, so the config carries no `device_fd`.
#[cfg(not(unix))]
pub fn shoes_config(upstream: &Upstream) -> Result<String> {
    shoes_config_blocking(upstream, &BLOCKED)
}

#[cfg(unix)]
fn shoes_config_blocking(fd: i32, upstream: &Upstream, blocked_masks: &[&str]) -> Result<String> {
    let mut tun = shoes_tun_mapping(upstream, blocked_masks)?;
    tun.insert(
        serde_yaml::Value::String("device_fd".into()),
        serde_yaml::Value::Number(fd.into()),
    );
    finish_tun_config(tun)
}

#[cfg(not(unix))]
fn shoes_config_blocking(upstream: &Upstream, blocked_masks: &[&str]) -> Result<String> {
    let mut tun = shoes_tun_mapping(upstream, blocked_masks)?;
    // The config schema tags TUN configs by the device_fd field; the packet
    // device itself is handed to the stack as a socket, so the value is a
    // placeholder.
    tun.insert(
        serde_yaml::Value::String("device_fd".into()),
        serde_yaml::Value::Number(serde_yaml::Number::from(0)),
    );
    finish_tun_config(tun)
}

fn shoes_tun_mapping(
    upstream: &Upstream,
    blocked_masks: &[&str],
) -> Result<serde_yaml::Mapping> {
    use serde_yaml::{Mapping, Value};
    let string = |s: &str| Value::String(s.to_string());
    let mut block = Mapping::new();
    block.insert(
        string("masks"),
        Value::Sequence(blocked_masks.iter().map(|m| string(m)).collect()),
    );
    block.insert(string("action"), string("block"));
    let chain = client_chain(upstream)?;
    let mut allow = Mapping::new();
    allow.insert(string("masks"), Value::Sequence(vec![string("0.0.0.0/0")]));
    allow.insert(string("action"), string("allow"));
    allow.insert(string("client_chain"), chain);
    let mut rules = Vec::new();
    if !blocked_masks.is_empty() {
        rules.push(Value::Mapping(block));
    }
    rules.push(Value::Mapping(allow));
    let mut tun = Mapping::new();
    tun.insert(string("packet_information"), Value::Bool(false));
    tun.insert(string("mtu"), Value::Number((MTU as u64).into()));
    tun.insert(string("tcp_enabled"), Value::Bool(true));
    tun.insert(string("udp_enabled"), Value::Bool(true));
    tun.insert(string("icmp_enabled"), Value::Bool(false));
    tun.insert(string("rules"), Value::Sequence(rules));
    Ok(tun)
}

fn finish_tun_config(tun: serde_yaml::Mapping) -> Result<String> {
    serde_yaml::to_string(&serde_yaml::Value::Sequence(vec![
        serde_yaml::Value::Mapping(tun),
    ]))
    .map_err(|error| Error::Message(error.to_string()))
}

// ---------------------------------------------------------------- runtime

/// Local shoes listener used solely to resolve DNS over HTTPS through the
/// same upstream as the packet stack. This also works with HTTP proxies,
/// which cannot carry UDP DNS.
struct Resolver {
    port: u16,
    preferred_doh: AtomicUsize,
    relay: HttpRelay,
    runtime: tokio::runtime::Handle,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Resolver {
    fn start(upstream: &Upstream) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let chain = serde_json::to_value(client_chain(upstream)?)
            .map_err(|e| Error::Message(e.to_string()))?;
        let config = serde_yaml::to_string(&serde_json::json!([{
            "address": format!("127.0.0.1:{port}"),
            "protocol": {"type": "http"},
            "rules": [{"masks": ["0.0.0.0/0"], "action": "allow", "client_chain": chain}]
        }]))
        .map_err(|e| Error::Message(e.to_string()))?;
        let dns = bound_dns(upstream)?;
        let (shutdown, mut rx) = tokio::sync::oneshot::channel();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("vm-dns-proxy".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => { let _ = ready.send(Err(error)); return; }
                };
                runtime.block_on(async move {
                    let setup = async {
                        let relay = if let Some(dns) = dns {
                            HttpRelay::new_with_bound_dns(&config, dns).await?
                        } else {
                            HttpRelay::new(&config).await?
                        };
                        Ok::<_, std::io::Error>((tokio::net::TcpListener::from_std(listener)?, relay))
                    }.await;
                    let (listener, host_relay) = match setup {
                        Ok(setup) => setup,
                        Err(error) => { let _ = ready.send(Err(error)); return; }
                    };
                    if ready.send(Ok((host_relay.clone(), tokio::runtime::Handle::current()))).is_err() {
                        return;
                    }
                    let mut tasks = tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            _ = &mut rx => break,
                            _ = tasks.join_next(), if !tasks.is_empty() => {},
                            conn = listener.accept() => {
                                match conn {
                                    Ok((stream, _)) => {
                                        let relay = host_relay.clone();
                                        tasks.spawn(async move { let _ = relay.relay(stream).await; });
                                    }
                                    Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
                                }
                            }
                        }
                    }
                    tasks.shutdown().await;
                });
            })?;
        let (relay, runtime) = initialized
            .recv()
            .map_err(|_| std::io::Error::other("DNS proxy failed to start"))??;
        Ok(Self {
            port,
            preferred_doh: AtomicUsize::new(if matches!(upstream, Upstream::BoundDirect(_)) { 1 } else { 0 }),
            relay,
            runtime,
            shutdown: Some(shutdown),
            thread: Some(thread),
        })
    }

    fn lookup(&self, ticket: &RelayTicket, query: &[u8]) -> Option<Vec<u8>> {
        if !(12..=MAX_DNS_QUERY).contains(&query.len()) || !ticket.is_current() {
            return None;
        }
        // Each attempt uses the same upstream and policy epoch. There is no
        // system-DNS or direct fallback if an upstream or TLS check fails.
        let endpoints = [
            "https://cloudflare-dns.com/dns-query",
            "https://dns.alidns.com/dns-query",
            "https://doh.pub/dns-query",
        ];
        let preferred = self.preferred_doh.load(Ordering::Relaxed) % endpoints.len();
        for offset in 0..endpoints.len() {
            let index = (preferred + offset) % endpoints.len();
            let endpoint = endpoints[index];
            if !ticket.is_current() { return None; }
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).ok()?;
            listener.set_nonblocking(true).ok()?;
            let proxy = ureq::Proxy::new(format!("http://{}", listener.local_addr().ok()?)).ok()?;
            let relay = self.relay.clone();
            let attempt_ticket = ticket.clone();
            let task = self.runtime.spawn(async move {
                attempt_ticket.run(async {
                    let listener = tokio::net::TcpListener::from_std(listener)?;
                    let (stream, _) = listener.accept().await?;
                    relay.relay(stream).await
                }).await;
            });
            let response = ureq::AgentBuilder::new()
                .resolver(crate::ipv4::resolve)
                .try_proxy_from_env(false)
                .proxy(proxy)
                .redirects(0)
                .timeout(Duration::from_secs(3))
                .build()
                .post(endpoint)
                .set("Content-Type", "application/dns-message")
                .set("Accept", "application/dns-message")
                .send_bytes(query);
            let mut bytes = Vec::new();
            let result = response.ok().filter(|response| response.status() == 200).and_then(|response| {
                response.into_reader().take(65536).read_to_end(&mut bytes).ok()
            });
            task.abort();
            if result.is_none() || !ticket.is_current() || !valid_doh_response(query, &bytes) {
                continue;
            }
            self.preferred_doh.store(index, Ordering::Relaxed);
            if bytes.len() > MTU - 28 {
                // Signal truncation so a DNS client can retry over TCP.
                bytes.truncate(12);
                bytes[2] |= 0x02;
                bytes[4..12].fill(0);
            }
            return Some(bytes);
        }
        None
    }
}

impl Drop for Resolver {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A running proxy stack (shoes) on our end of the packet socket pair.
struct Stack {
    gate: RelayGate,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stack {
    #[cfg(unix)]
    fn start(fd: i32, upstream: &Upstream, error: Arc<Mutex<Option<String>>>) -> Result<Self> {
        Self::start_with(shoes_config(fd, upstream)?, upstream, error, None)
    }

    /// Windows: the packet device is the stack's half of the loopback pair.
    #[cfg(not(unix))]
    fn start(device: WakeSocket, upstream: &Upstream, error: Arc<Mutex<Option<String>>>) -> Result<Self> {
        Self::start_with(shoes_config(upstream)?, upstream, error, Some(device))
    }

    fn start_with(
        config: String,
        upstream: &Upstream,
        error: Arc<Mutex<Option<String>>>,
        #[cfg(not(unix))] device: Option<WakeSocket>,
        #[cfg(unix)] _device: Option<WakeSocket>,
    ) -> Result<Self> {
        let stack = Self::with_config_dns(config, error.clone(), bound_dns(upstream)?, {
            #[cfg(not(unix))]
            {
                device
            }
            #[cfg(unix)]
            {
                None
            }
        })?;
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

    #[cfg(all(unix, test))]
    fn with_config(config: String, error: Arc<Mutex<Option<String>>>) -> Result<Self> {
        Self::with_config_dns(config, error, None, None)
    }

    fn with_config_dns(
        config: String,
        error: Arc<Mutex<Option<String>>>,
        dns: Option<shoes::embed::BoundDns>,
        #[cfg(not(unix))] device: Option<WakeSocket>,
        #[cfg(unix)] _device: Option<WakeSocket>,
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
                            #[cfg(unix)]
                            {
                                if let Some(dns) = dns {
                                    shoes::embed::run_tun_controlled_with_bound_dns(&config, shutdown_rx, stack_gate, dns).await
                                } else {
                                    shoes::embed::run_tun_controlled(&config, shutdown_rx, stack_gate).await
                                }
                            }
                            #[cfg(not(unix))]
                            {
                                let device = device.ok_or_else(|| {
                                    std::io::Error::other("the packet device is missing")
                                })?;
                                shoes::embed::run_tun_controlled_with_device(&config, shutdown_rx, stack_gate, device).await
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

/// Cumulative IP bytes from the guest's point of view, for the life of this gateway.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GatewayTraffic {
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

struct PacketPath {
    host: WakeSocket,
    stack: WakeSocket,
}

impl PacketPath {
    fn new() -> Result<Self> {
        let (host, stack) = crate::compat::packet_pair()?;
        Ok(Self { host, stack })
    }

    fn clear(&self) {
        for socket in [&self.host, &self.stack] {
            while socket.recv(&mut [0]).is_ok() {}
        }
    }
}

// Cap each queue independently so a burst of one protocol cannot use all slots.
const HELD_PACKETS: usize = 256;
const HELD_DNS: usize = 64;
const DNS_INFLIGHT: usize = 32;
const MAX_DNS_QUERY: usize = 4096;

struct DnsRequest {
    src: Ipv4Addr,
    dst: Ipv4Addr,
    port: u16,
    ip_len: usize,
    query: Vec<u8>,
}

impl DnsRequest {
    fn same_query(&self, other: &Self) -> bool {
        self.src == other.src && self.dst == other.dst && self.port == other.port
            && self.query == other.query
    }
}

struct GuestEgress {
    mode: EgressMode,
    dns_gate: RelayGate,
    probe_flows: HashSet<(Ipv4Addr, u16)>,
    ordinary_probe_syns: HashMap<(Ipv4Addr, u16), [u8; 4]>,
    held_packets: VecDeque<Vec<u8>>,
    held_dns: VecDeque<DnsRequest>,
    dns_inflight: usize,
}

impl GuestEgress {
    fn invalidate_lanes(&mut self) {
        self.dns_gate.suspend(|| {});
        self.probe_flows.clear();
    }

    fn discard_held(&mut self) {
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

fn outbound_probe_tcp(packet: &[u8]) -> Option<((Ipv4Addr, u16), &[u8])> {
    let (ip, tcp) = tcp_packet(packet)?;
    let remote = remote_endpoint(ip, tcp, false);
    if ip[12..16] != GUEST_IP.octets() || !verification_endpoint(remote) {
        return None;
    }
    let local_port = u16::from_be_bytes([tcp[0], tcp[1]]);
    Some(((remote.0, local_port), tcp))
}

/// `handle_frame` has already checked the IPv4 lengths and destination.
fn hold_packet(packet: &[u8]) -> bool {
    if packet.len() > MTU || packet[12..16] != GUEST_IP.octets()
        || u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff != 0
    {
        return false;
    }
    let header = usize::from(packet[0] & 0x0f) * 4;
    let transport = &packet[header..];
    match packet[9] {
        6 if transport.len() >= 20 => (20..=transport.len()).contains(&(usize::from(transport[12] >> 4) * 4)),
        17 if transport.len() >= 8 => {
            transport[2..4] != 53u16.to_be_bytes()
                && (8..=transport.len()).contains(&usize::from(u16::from_be_bytes([transport[4], transport[5]])))
        }
        _ => false,
    }
}

struct Shared {
    dns: Vec<Ipv4Addr>,
    guest: Mutex<Option<DuplexStream>>,
    egress: Mutex<GuestEgress>,
    held_ready: Condvar,
    stop: AtomicBool,
    error: Arc<Mutex<Option<String>>>,
    resolver: Mutex<Option<Arc<Resolver>>>,
    paths: [PacketPath; 2],
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
}

struct GatewayStacks {
    upstream: Option<Upstream>,
    active: [Option<Stack>; 2],
    ordinary_opened: bool,
}

/// The gateway: listens on `socket` for QEMU and runs the proxy stack.
pub struct Gateway {
    socket: Endpoint,
    shared: Arc<Shared>,
    /// The Mac's SSH port, relayed to the guest.
    forward: Option<Arc<crate::net_forward::SshForward>>,
    stacks: Mutex<GatewayStacks>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Shared {
    fn write_guest(&self, frame: &[u8]) -> bool {
        let mut guest = self.guest.locked();
        let Some(stream) = guest.as_mut() else {
            return false;
        };
        if write_frame(stream, frame).is_ok() {
            true
        } else {
            // A partial frame would make later writes unreadable.
            let _ = stream.shutdown_both();
            false
        }
    }

    fn disconnect_guest(&self) {
        let mut egress = self.egress.locked();
        *self.guest.locked() = None;
        egress.invalidate_lanes();
        egress.discard_held();
        egress.ordinary_probe_syns.clear();
        if egress.mode.lane().is_some() {
            egress.dns_gate.resume();
        }
    }

    fn send_ip(&self, packet: Vec<u8>) {
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

    fn run_down(&self) {
        let mut ready: Vec<PollItem> = self
            .paths
            .each_ref()
            .map(|path| poll_in(&path.host))
            .into_iter()
            .collect();
        let mut packet = vec![0u8; 65_536];
        while !self.stop.load(Ordering::Relaxed) {
            let count = poll(&mut ready, 500).unwrap_or(0);
            if count > 0 {
                for lane in [Lane::Ordinary, Lane::Verification] {
                    if ready[lane as usize].revents & POLLIN != 0 {
                        self.receive_ip(lane, &mut packet);
                    }
                }
            }
        }
    }

    fn run_held(self: &Arc<Self>) {
        let mut egress = self.egress.locked();
        while !self.stop.load(Ordering::Relaxed) {
            if egress.mode == EgressMode::Open {
                let mut progressed = false;
                if let Some(packet) = egress.held_packets.front() {
                    let retry = self.send_ordinary_ip(packet).is_err_and(|error| {
                        matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted)
                            || cfg!(unix) && error.raw_os_error() == Some(libc::ENOBUFS)
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

    fn resolve(self: &Arc<Self>, mut request: DnsRequest) {
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
    fn suspend(&self, shared: &Shared) {
        for (slot, path) in self.active.iter().zip(&shared.paths) {
            if let Some(stack) = slot {
                stack.gate.suspend(|| path.clear());
            }
        }
    }

    fn close(&mut self) {
        self.active = [None, None];
        self.ordinary_opened = false;
    }

    fn activate(&mut self, shared: &Shared, mode: EgressMode) -> Result<()> {
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
                #[cfg(unix)]
                let started = {
                    let fd = shared.paths[lane as usize].stack.as_raw_fd();
                    Stack::start(fd, upstream, shared.error.clone())
                };
                #[cfg(not(unix))]
                let started = Stack::start(
                    shared.paths[lane as usize].stack.try_clone()?,
                    upstream,
                    shared.error.clone(),
                );
                *slot = Some(started?);
            }
        }
        self.ordinary_opened |= mode == EgressMode::Open;
        let mut egress = shared.egress.locked();
        egress.mode = mode;
        if let Some(lane) = mode.lane() {
            self.active[lane as usize].as_ref().unwrap().gate.resume();
            egress.dns_gate.resume();
        }
        if mode == EgressMode::Open {
            shared.held_ready.notify_one();
        }
        Ok(())
    }
}

#[cfg(all(unix, test))]
fn datagram_pair() -> Result<(std::os::unix::net::UnixDatagram, std::os::unix::net::UnixDatagram)> {
    let (ours, theirs) = std::os::unix::net::UnixDatagram::pair()?;
    for socket in [&ours, &theirs] {
        // macOS defaults to tiny Unix datagram buffers.
        let size: libc::c_int = 4 * 1024 * 1024;
        for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
            unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    (&size as *const libc::c_int).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }
    }
    Ok((ours, theirs))
}

fn read_frame(stream: &mut DuplexStream, buffer: &mut Vec<u8>) -> std::io::Result<()> {
    let mut length = [0u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 65_536 {
        return Err(std::io::Error::other("oversized frame"));
    }
    buffer.resize(length, 0);
    stream.read_exact(buffer)
}

fn write_frame(stream: &mut DuplexStream, frame: &[u8]) -> std::io::Result<()> {
    let mut message = Vec::with_capacity(4 + frame.len());
    message.extend_from_slice(&(frame.len() as u32).to_be_bytes());
    message.extend_from_slice(frame);
    stream.write_all(&message)
}

impl Gateway {
    /// Listen on `socket` (QEMU connects to it) and start routing through
    /// `upstream`. Packets are answered by the stack only while it runs.
    /// `ssh` is the Mac-side address relayed to the guest's port 22.
    pub fn start(
        socket: &Endpoint,
        dns: Vec<Ipv4Addr>,
        upstream: &Upstream,
        ssh: std::net::SocketAddr,
    ) -> Result<Self> {
        Self::start_with_egress(socket, dns, Some(upstream), ssh, EgressMode::Open)
    }

    /// Start with the internet path closed. With no upstream, only local
    /// management traffic is possible until a route is installed.
    pub fn start_closed(
        socket: &Endpoint,
        dns: Vec<Ipv4Addr>,
        upstream: Option<&Upstream>,
        ssh: std::net::SocketAddr,
    ) -> Result<Self> {
        Self::start_with_egress(socket, dns, upstream, ssh, EgressMode::Closed)
    }

    fn start_with_egress(
        socket: &Endpoint,
        dns: Vec<Ipv4Addr>,
        upstream: Option<&Upstream>,
        ssh: std::net::SocketAddr,
        egress: EgressMode,
    ) -> Result<Self> {
        #[cfg(unix)]
        let socket_path = match socket {
            Endpoint::Unix(path) => path.clone(),
            Endpoint::Tcp(_) => {
                return Err(Error::Message(
                    "TCP gateway sockets are only used on Windows".into(),
                ));
            }
        };
        if crate::sockets::Endpoint::connect(socket, Duration::from_millis(250)).is_ok() {
            return Err(Error::Message(format!(
                "A network gateway is already listening at {}",
                socket.describe()
            )));
        }
        #[cfg(unix)]
        let listener = {
            let _ = std::fs::remove_file(&socket_path);
            if let Some(parent) = socket_path.parent() {
                crate::util::ensure_dir(parent)?;
            }
            std::os::unix::net::UnixListener::bind(&socket_path).map_err(|error| {
                Error::Message(format!(
                    "Could not open the VM's network socket {}: {error}",
                    socket_path.display()
                ))
            })?
        };
        #[cfg(not(unix))]
        let listener = match socket {
            Endpoint::Tcp(port) => TcpListener::bind((Ipv4Addr::LOCALHOST, *port)).map_err(
                |error| {
                    Error::Message(format!(
                        "Could not open the VM's network socket {}: {error}",
                        socket.describe()
                    ))
                },
            )?,
            Endpoint::Unix(_) => {
                return Err(Error::Message(
                    "Unix sockets are not available on this platform".into(),
                ));
            }
        };
        // Until the gateway is up, a failure removes the socket again, so
        // QEMU never finds a dead one.
        #[cfg(unix)]
        let mut cleanup = RemoveOnDrop(Some(socket_path.clone()));
        let resolver = upstream.map(Resolver::start).transpose()?.map(Arc::new);
        let dns_gate = RelayGate::default();
        dns_gate.suspend(|| {});
        let shared = Arc::new(Shared {
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
            // The configured port can be taken by another program; the
            // relay then takes any free loopback port and reports it.
            let send = move |frame: &[u8]| {
                shared.write_guest(frame);
            };
            let send = Arc::new(send);
            let first = {
                let send = send.clone();
                crate::net_forward::SshForward::start(ssh, move |frame| send(frame))
            };
            Arc::new(match first {
                Err(Error::Io(io))
                    if io.kind() == std::io::ErrorKind::AddrInUse && ssh.port() != 0 =>
                {
                    let send = send.clone();
                    crate::net_forward::SshForward::start((ssh.ip(), 0).into(), move |frame| {
                        send(frame)
                    })?
                }
                other => other?,
            })
        };

        let mut threads = Vec::new();
        // Stack → VM.
        {
            let shared = shared.clone();
            threads.push(
                std::thread::Builder::new()
                    .name("vm-net-down".into())
                    .spawn(move || shared.run_down())?,
            );
        }
        // VM → gateway / stack. One QEMU connection at a time; QEMU
        // reconnects after an app restart.
        {
            let shared = shared.clone();
            let forward = forward.clone();
            listener.set_nonblocking(true)?;
            threads.push(std::thread::Builder::new().name("vm-net-up".into()).spawn(
                move || {
                    while !shared.stop.load(Ordering::Relaxed) {
                        let mut stream: DuplexStream = match listener.accept() {
                            Ok((stream, _)) => stream.into(),
                            Err(_) => {
                                std::thread::sleep(Duration::from_millis(100));
                                continue;
                            }
                        };
                        let _ = stream.set_nonblocking(false);
                        // Reads must preserve frame boundaries even if the sender
                        // pauses after writing part of a length or payload.
                        let _ = stream.set_read_timeout(None);
                        let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                        *shared.guest.locked() =
                            stream.try_clone().ok();
                        let mut frame = Vec::new();
                        while !shared.stop.load(Ordering::Relaxed) {
                            match read_frame(&mut stream, &mut frame) {
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
        {
            let held = shared.clone();
            threads.push(std::thread::Builder::new().name("vm-net-held".into())
                .spawn(move || held.run_held())?);
        }
        #[cfg(unix)]
        {
            cleanup.0 = None;
        }
        Ok(Self {
            socket: socket.clone(),
            shared,
            forward: Some(forward),
            stacks: Mutex::new(stacks),
            threads,
        })
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

/// Removes a path when dropped, unless disarmed.
#[cfg(unix)]
struct RemoveOnDrop(Option<PathBuf>);

#[cfg(unix)]
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.set_egress_mode(EgressMode::Closed);
        self.shared.held_ready.notify_one();
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
            let _ = guest.shutdown_both();
        }
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        self.forward = None;
        #[cfg(unix)]
        if let crate::sockets::Endpoint::Unix(path) = &self.socket {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::net::TcpStream;
    use std::os::fd::OwnedFd;
    use std::time::Instant;

    use super::*;

    fn ipv4_frame(protocol: u8, dst: Ipv4Addr, transport: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x45, 0, 0, 0, 0, 0, 0x40, 0, 64, protocol, 0, 0];
        packet.extend_from_slice(&GUEST_IP.octets());
        packet.extend_from_slice(&dst.octets());
        packet.extend_from_slice(transport);
        let total = packet.len() as u16;
        packet[2..4].copy_from_slice(&total.to_be_bytes());
        let mut frame = Vec::new();
        frame.extend_from_slice(&GATEWAY_MAC);
        frame.extend_from_slice(&GUEST_MAC);
        frame.extend_from_slice(&ETH_IPV4.to_be_bytes());
        frame.extend_from_slice(&packet);
        frame
    }

    #[test]
    fn the_gateway_answers_ntp_with_the_macs_clock() {
        let mut request = vec![0u8; 48];
        request[0] = (4 << 3) | 3; // v4 client
        request[40..48].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let now = std::time::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let reply = ntp_reply(&request, now).unwrap();
        assert_eq!(
            reply[0],
            (4 << 3) | 4,
            "server mode, same version, no leap alarm"
        );
        assert_eq!(reply[1], 2);
        assert_eq!(
            &reply[24..32],
            &[1, 2, 3, 4, 5, 6, 7, 8],
            "origin echoes the client"
        );
        let seconds = u32::from_be_bytes(reply[40..44].try_into().unwrap()) as u64;
        assert_eq!(seconds, 1_700_000_000 + NTP_UNIX_OFFSET);
        assert!(ntp_reply(&request[..47], now).is_none());

        let mut udp = vec![0x30, 0x39, 0, 123, 0, 56, 0, 0];
        udp.extend_from_slice(&request);
        let Action::ToGuest(frame) = handle_frame(&ipv4_frame(17, GATEWAY_IP, &udp), &[]) else {
            panic!("NTP to the gateway is answered");
        };
        assert_eq!(&frame[0..6], &GUEST_MAC);
    }

    #[test]
    fn tcp_to_the_gateway_goes_to_the_ssh_relay() {
        let tcp = [0u8; 20];
        assert!(matches!(
            handle_frame(&ipv4_frame(6, GATEWAY_IP, &tcp), &[]),
            Action::Forward(_)
        ));
        // Other private destinations stay unreachable.
        assert!(matches!(
            handle_frame(&ipv4_frame(6, Ipv4Addr::new(10, 0, 3, 2), &tcp), &[]),
            Action::Drop
        ));
    }
    fn frame(dst: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut f = dst.to_vec();
        f.extend_from_slice(&GUEST_MAC);
        f.extend_from_slice(&ethertype.to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    fn dhcp(kind: u8) -> Vec<u8> {
        let mut bootp = vec![0u8; 240];
        bootp[0] = 1;
        bootp[4..8].copy_from_slice(&[1, 2, 3, 4]);
        bootp[28..34].copy_from_slice(&GUEST_MAC);
        bootp[236..240].copy_from_slice(&[99, 130, 83, 99]);
        bootp.extend_from_slice(&[53, 1, kind, 255]);
        ipv4_udp(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, 68, 67, &bootp)
    }

    fn tcp_to(dst: Ipv4Addr) -> Vec<u8> {
        let mut p = vec![0u8; 40];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&40u16.to_be_bytes());
        p[9] = 6;
        p[12..16].copy_from_slice(&GUEST_IP.octets());
        p[16..20].copy_from_slice(&dst.octets());
        p
    }

    #[test]
    fn answers_arp_for_the_gateway_only() {
        let mut arp = vec![0, 1, 8, 0, 6, 4, 0, 1];
        arp.extend_from_slice(&GUEST_MAC);
        arp.extend_from_slice(&GUEST_IP.octets());
        arp.extend_from_slice(&[0; 6]);
        arp.extend_from_slice(&GATEWAY_IP.octets());
        let Action::ToGuest(reply) = handle_frame(&frame([0xff; 6], ETH_ARP, &arp), &DEFAULT_DNS)
        else {
            panic!("no ARP reply");
        };
        assert_eq!(&reply[0..6], &GUEST_MAC);
        assert_eq!(&reply[14 + 8..14 + 14], &GATEWAY_MAC);
        let mut other = arp.clone();
        other[24..28].copy_from_slice(&[10, 0, 3, 99]);
        assert_eq!(
            handle_frame(&frame([0xff; 6], ETH_ARP, &other), &DEFAULT_DNS),
            Action::Drop
        );
    }

    #[test]
    fn dhcp_offers_the_guest_address_with_custom_dns() {
        let dns = [Ipv4Addr::new(9, 9, 9, 9)];
        for (kind, expect) in [(1u8, 2u8), (3, 5)] {
            let Action::ToGuest(reply) =
                handle_frame(&frame([0xff; 6], ETH_IPV4, &dhcp(kind)), &dns)
            else {
                panic!("no DHCP reply for {kind}");
            };
            let bootp = &reply[14 + 28..];
            assert_eq!(bootp[0], 2);
            assert_eq!(&bootp[16..20], &GUEST_IP.octets());
            let options = &bootp[240..];
            assert_eq!(&options[..3], &[53, 1, expect]);
            let dns_at = options
                .windows(2)
                .position(|w| w == [6, 4])
                .expect("DNS option");
            assert_eq!(&options[dns_at + 2..dns_at + 6], &[9, 9, 9, 9]);
            let ntp_at = options
                .windows(2)
                .position(|w| w == [42, 4])
                .expect("NTP option");
            assert_eq!(&options[ntp_at + 2..ntp_at + 6], &GATEWAY_IP.octets());
            assert_eq!(checksum(&reply[14..34]), 0, "valid IPv4 header checksum");
        }
    }

    #[test]
    fn internet_goes_to_the_stack_private_ranges_are_dropped() {
        let public = tcp_to(Ipv4Addr::new(93, 184, 216, 34));
        assert_eq!(
            handle_frame(&frame(GATEWAY_MAC, ETH_IPV4, &public), &DEFAULT_DNS),
            Action::ToStack(public.clone())
        );
        for private in [
            [192, 168, 1, 10],
            [10, 0, 2, 2],
            [172, 20, 0, 1],
            [127, 0, 0, 1],
            [169, 254, 1, 1],
        ] {
            let packet = tcp_to(Ipv4Addr::from(private));
            assert_eq!(
                handle_frame(&frame(GATEWAY_MAC, ETH_IPV4, &packet), &DEFAULT_DNS),
                Action::Drop,
                "{private:?}"
            );
        }
        // IPv6 never leaves.
        assert_eq!(
            handle_frame(&frame(GATEWAY_MAC, 0x86dd, &[0x60; 40]), &DEFAULT_DNS),
            Action::Drop
        );
    }

    #[test]
    fn ipv6_packets_are_dropped_in_both_directions_in_every_mode() {
        let ipv6 = [0x60; 40];
        for mode in [
            EgressMode::Closed,
            EgressMode::Suspended,
            EgressMode::Rechecking,
            EgressMode::Install,
            EgressMode::Checking,
            EgressMode::Open,
        ] {
            assert!(!mode.allows_packet(&ipv6, false));
            assert!(!mode.allows_packet(&ipv6, true));
        }
        assert_eq!(handle_frame(&frame(GATEWAY_MAC, ETH_IPV4, &ipv6), &DEFAULT_DNS), Action::Drop);
        assert_eq!(handle_frame(&frame(GATEWAY_MAC, 0x86dd, &ipv6), &DEFAULT_DNS), Action::Drop);
    }

    #[test]
    fn aaaa_queries_are_answered_without_forwarding() {
        let mut query = vec![0u8; 12];
        query[..2].copy_from_slice(&[0x12, 0x34]);
        query[2] = 1;
        query[5] = 1;
        query.extend_from_slice(b"\x03one\x03one\x03one\x03one\x00\x00\x1c\x00\x01");
        for mode in [EgressMode::Open, EgressMode::Install, EgressMode::Checking, EgressMode::Rechecking] {
            let mut accepted = query.clone();
            assert!(mode.allows_dns(&mut accepted));
            let answer = dns_ipv6_nodata(&accepted).unwrap();
            assert_eq!(&answer[..2], &[0x12, 0x34]);
            assert_eq!(&answer[2..4], &[0x81, 0x80]);
            assert_eq!(&answer[4..12], &[0, 1, 0, 0, 0, 0, 0, 0]);
            assert_eq!(&answer[12..], &query[12..]);
        }
        assert!(!EgressMode::Closed.allows_dns(&mut query));
        assert!(!EgressMode::Suspended.allows_dns(&mut query));
        let mut a = query.clone();
        let end = a.len();
        a[end - 3] = 1;
        assert!(dns_ipv6_nodata(&a).is_none());
        assert!(EgressMode::Open.allows_dns(&mut a));
        query[5] = 2;
        assert!(!EgressMode::Open.allows_dns(&mut query));
    }

    #[test]
    fn ordinary_dns_strips_edns_options_and_preserves_dnssec_request() {
        let mut query = vec![0u8; 12];
        query[5] = 1;
        query[11] = 1;
        query.extend_from_slice(b"\x07example\x03com\x00\x00\x01\x00\x01");
        let end = query.len();
        query.extend_from_slice(&[0, 0, 41, 16, 0, 0, 0, 128, 0, 0, 4, 0, 8, 0, 0]);
        assert!(sanitize_dns_query(&mut query));
        assert_eq!(&query[end..], &[0, 0, 41, 4, 208, 0, 0, 128, 0, 0, 0]);
        assert_eq!(query[11], 1);
        query.push(1); // A trailing record without a length is discarded.
        assert!(sanitize_dns_query(&mut query));
        assert_eq!(query.len(), end);
        assert_eq!(query[11], 0);
        query[end - 1] = 3; // CHAOS queries are not Internet DNS.
        assert!(!sanitize_dns_query(&mut query));
    }

    #[test]
    fn checking_mode_only_admits_the_fixed_probe_targets() {
        let mut tcp = vec![0u8; 20];
        tcp[2..4].copy_from_slice(&443u16.to_be_bytes());
        let allowed = ipv4_frame(6, PUBLIC_PROBE_V4, &tcp)[14..].to_vec();
        let alternate = ipv4_frame(6, ALTERNATE_PROBE_V4, &tcp)[14..].to_vec();
        let unlisted = ipv4_frame(6, Ipv4Addr::new(104, 16, 124, 96), &tcp)[14..].to_vec();
        assert_ne!(outbound_probe_tcp(&allowed).unwrap().0, outbound_probe_tcp(&alternate).unwrap().0);
        assert!(!EgressMode::Closed.allows_packet(&allowed, false));
        assert!(!EgressMode::Suspended.allows_packet(&allowed, false));
        let mut udp = allowed.clone();
        udp[9] = 17;
        let mut fragment = allowed.clone();
        fragment[7] = 1;
        tcp[2..4].copy_from_slice(&80u16.to_be_bytes());
        let denied = ipv4_frame(6, PUBLIC_PROBE_V4, &tcp)[14..].to_vec();
        let mut query = vec![0u8; 12];
        query[5] = 1;
        query.extend_from_slice(b"\x03one\x03one\x03one\x03one\x00\x00\x01\x00\x01");
        for mode in [EgressMode::checking(), EgressMode::Rechecking] {
            assert!(mode.allows_packet(&allowed, false));
            assert!(mode.allows_packet(&alternate, false));
            assert!(!mode.allows_packet(&unlisted, false));
            assert!(!mode.allows_packet(&udp, false));
            assert!(!mode.allows_packet(&fragment, false));
            assert!(!mode.allows_packet(&denied, false));

            let mut with_extra = query.clone();
            with_extra[11] = 1;
            with_extra.extend_from_slice(b"extra data");
            assert!(mode.allows_dns(&mut with_extra));
            assert_eq!(with_extra, query);
            let mut denied = query.clone();
            denied[5] = 2;
            assert!(!mode.allows_dns(&mut denied));
            denied[5] = 1;
            denied[13] = b'x';
            assert!(!mode.allows_dns(&mut denied));
        }
        assert!(!EgressMode::Closed.allows_dns(&mut query));
        assert!(!EgressMode::Suspended.allows_dns(&mut query));
    }

    #[test]
    fn install_mode_admits_dns_and_package_ports_only() {
        let mut tcp = vec![0u8; 20];
        for port in [80u16, 443] {
            tcp[2..4].copy_from_slice(&port.to_be_bytes());
            let allowed = ipv4_frame(6, Ipv4Addr::new(93, 184, 216, 34), &tcp)[14..].to_vec();
            assert!(
                EgressMode::Install.allows_packet(&allowed, false),
                "port {port}"
            );
        }
        tcp[2..4].copy_from_slice(&22u16.to_be_bytes());
        let denied = ipv4_frame(6, Ipv4Addr::new(93, 184, 216, 34), &tcp)[14..].to_vec();
        assert!(!EgressMode::Install.allows_packet(&denied, false));
        let udp = ipv4_frame(17, Ipv4Addr::new(93, 184, 216, 34), &tcp)[14..].to_vec();
        assert!(!EgressMode::Install.allows_packet(&udp, false));
        assert!(!EgressMode::Install.allows_packet(&[0; 9], false));

        let mut query = vec![0u8; 12];
        query[4..6].copy_from_slice(&1u16.to_be_bytes());
        query.extend_from_slice(b"\x06mirror\x07example\x03com\x00\x00\x01\x00\x01");
        let original = query.clone();
        assert!(EgressMode::Install.allows_dns(&mut query));
        assert_eq!(query, original);
    }

    #[test]
    fn doh_response_must_match_the_query_and_not_be_a_server_error() {
        let mut query = vec![0u8; 12];
        query[0..2].copy_from_slice(&[0x12, 0x34]);
        query[4..6].copy_from_slice(&1u16.to_be_bytes());
        query.extend_from_slice(b"\x03one\x03one\x03one\x03one\x00\x00\x01\x00\x01");
        let mut response = query.clone();
        response[2] = 0x81;
        response[3] = 0x80;
        assert!(valid_doh_response(&query, &response));
        response[3] = 0x82;
        assert!(!valid_doh_response(&query, &response));
        response[3] = 0x80;
        response[0] ^= 1;
        assert!(!valid_doh_response(&query, &response));
        response[0] ^= 1;
        response[13] ^= 1;
        assert!(!valid_doh_response(&query, &response));
        assert!(!valid_doh_response(&query, &response[..12]));
    }

    #[test]
    fn bound_upstreams_only_set_the_interface_on_the_first_hop() {
        let direct = client_chain_value(&Upstream::BoundDirect("en8".into())).unwrap();
        assert_eq!(direct["bind_interface"].as_str(), Some("en8"));
        let hop: serde_yaml::Value = serde_yaml::from_str("name: proxy\nbind_interface: stale\nprotocol: {type: socks}\n").unwrap();
        let chain = client_chain_value(&Upstream::BoundChain(vec![hop.clone(), hop], "en8".into())).unwrap();
        let hops = chain.as_sequence().unwrap();
        assert_eq!(hops[0]["bind_interface"].as_str(), Some("en8"));
        assert!(hops[1].get("bind_interface").is_none());
        assert!(client_chain(&Upstream::BoundDirect("cabin-nonexistent-interface".into())).is_err());
    }

    #[test]
    fn shoes_config_blocks_private_ranges_first() {
        let yaml = shoes_config(7, &Upstream::Direct).unwrap();
        let block = yaml.find("action: block").unwrap();
        let allow = yaml.find("action: allow").unwrap();
        assert!(block < allow, "{yaml}");
        assert!(
            yaml.contains("device_fd: 7") && yaml.contains("packet_information: false"),
            "{yaml}"
        );
        assert!(yaml.contains("192.168.0.0/16"));
    }

    #[test]
    fn a_taken_ssh_port_moves_the_relay_to_a_free_one() {
        let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        let dir = std::env::temp_dir().join(format!("netgw-taken-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let gateway = Gateway::start(
            &Endpoint::Unix(dir.join("net.sock")),
            Vec::new(),
            &Upstream::Direct,
            (Ipv4Addr::LOCALHOST, port).into(),
        )
        .expect("a taken SSH port is not fatal");
        let actual = gateway.ssh_addr().unwrap();
        assert_ne!(actual.port(), port);
        assert!(
            TcpStream::connect(actual).is_ok(),
            "the relay listens on the new port"
        );
        drop(gateway);
        assert!(!dir.join("net.sock").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_chain_goes_out_through_its_hops_in_order() {
        let hop = |name: &str, address: &str| {
            serde_yaml::from_str::<serde_yaml::Value>(&format!(
                "{{name: {name}, address: \"{address}\", protocol: {{type: socks}}}}"
            ))
            .unwrap()
        };
        let upstream = Upstream::Chain(vec![
            hop("entry", "198.51.100.1:1080"),
            hop("exit", "203.0.113.1:1080"),
        ]);
        let yaml = shoes_config(7, &upstream).unwrap();
        let entry = yaml.find("198.51.100.1:1080").unwrap();
        let exit = yaml.find("203.0.113.1:1080").unwrap();
        assert!(entry < exit, "{yaml}");
        assert!(!yaml.contains("name: entry"), "{yaml}");
        assert!(shoes_config(7, &Upstream::Chain(Vec::new())).is_err());
    }

    fn tcp_syn(src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16) -> Vec<u8> {
        tcp_segment((src, sport), (dst, dport), 1000, 0, 0x02, &[])
    }

    fn tcp_segment(
        (src, sport): (Ipv4Addr, u16),
        (dst, dport): (Ipv4Addr, u16),
        seq: u32,
        ack: u32,
        flags: u8,
        data: &[u8],
    ) -> Vec<u8> {
        let mut p = vec![0u8; 40 + data.len()];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&((40 + data.len()) as u16).to_be_bytes());
        p[6] = 0x40; // don't fragment
        p[8] = 64;
        p[9] = 6;
        p[12..16].copy_from_slice(&src.octets());
        p[16..20].copy_from_slice(&dst.octets());
        let sum = checksum(&p[..20]);
        p[10..12].copy_from_slice(&sum.to_be_bytes());
        let tcp = &mut p[20..];
        tcp[0..2].copy_from_slice(&sport.to_be_bytes());
        tcp[2..4].copy_from_slice(&dport.to_be_bytes());
        tcp[4..8].copy_from_slice(&seq.to_be_bytes());
        tcp[8..12].copy_from_slice(&ack.to_be_bytes());
        tcp[12] = 5 << 4;
        tcp[13] = flags;
        tcp[14..16].copy_from_slice(&65535u16.to_be_bytes());
        tcp[20..].copy_from_slice(data);
        let mut pseudo = Vec::new();
        pseudo.extend_from_slice(&src.octets());
        pseudo.extend_from_slice(&dst.octets());
        pseudo.extend_from_slice(&[0, 6]);
        pseudo.extend_from_slice(&(tcp.len() as u16).to_be_bytes());
        pseudo.extend_from_slice(tcp);
        let sum = checksum(&pseudo);
        p[36..38].copy_from_slice(&sum.to_be_bytes());
        p
    }

    /// shoes' TUN stack over a datagram socket pair: a SYN from the "VM"
    /// becomes a real TCP connection from the host, and the SYN-ACK comes
    /// back as a packet. (Private destinations are normally blocked; this
    /// test allows them so it can use a local server.)
    #[test]
    fn stack_turns_packets_into_real_connections() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        let (ours, theirs) = datagram_pair().unwrap();
        let theirs: OwnedFd = theirs.into();
        let config = shoes_config_blocking(theirs.as_raw_fd(), &Upstream::Direct, &[]).unwrap();
        let error = Arc::new(Mutex::new(None));
        let _stack = Stack::with_config(config, error.clone()).unwrap();
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
        let (ours, theirs) = datagram_pair().unwrap();
        let theirs: OwnedFd = theirs.into();
        let config = shoes_config(theirs.as_raw_fd(), &Upstream::Direct).unwrap();
        let _stack = Stack::with_config(config, Arc::new(Mutex::new(None))).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        ours.send(&tcp_syn(GUEST_IP, 40001, Ipv4Addr::LOCALHOST, port))
            .unwrap();
        std::thread::sleep(Duration::from_millis(1500));
        assert!(
            server.accept().is_err(),
            "a blocked destination was connected"
        );
    }

    fn accept_proxy(listener: &TcpListener) -> TcpStream {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "no outbound proxy connection");
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            header.push(byte[0]);
            assert!(header.len() < 1024);
        }
        assert!(
            header.starts_with(b"CONNECT 1.1.1.1:443 HTTP/1.1\r\n"),
            "{header:?}"
        );
        stream
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
            .unwrap();
        stream
    }

    struct GuestTcp {
        port: u16,
        seq: u32,
        ack: u32,
    }

    impl GuestTcp {
        fn start(link: &mut DuplexStream, port: u16) -> Self {
            write_frame(
                link,
                &frame(
                    GATEWAY_MAC,
                    ETH_IPV4,
                    &tcp_syn(GUEST_IP, port, PUBLIC_PROBE_V4, 443),
                ),
            )
            .unwrap();
            let mut reply = Vec::new();
            read_frame(link, &mut reply).unwrap();
            let (_, tcp) = tcp_packet(&reply[14..]).unwrap();
            assert_eq!(&tcp[2..4], &port.to_be_bytes());
            assert_eq!(tcp[13] & 0x12, 0x12);
            let ack = u32::from_be_bytes(tcp[4..8].try_into().unwrap()).wrapping_add(1);
            let guest = Self {
                port,
                seq: 1001,
                ack,
            };
            guest.send(link, &[]);
            guest
        }

        fn send(&self, link: &mut DuplexStream, data: &[u8]) {
            let packet = tcp_segment(
                (GUEST_IP, self.port),
                (PUBLIC_PROBE_V4, 443),
                self.seq,
                self.ack,
                0x18,
                data,
            );
            write_frame(link, &frame(GATEWAY_MAC, ETH_IPV4, &packet)).unwrap();
        }

        fn receive(&mut self, link: &mut DuplexStream, expected: &[u8]) {
            let mut reply = Vec::new();
            loop {
                read_frame(link, &mut reply).unwrap();
                let (_, tcp) = tcp_packet(&reply[14..]).unwrap();
                assert_eq!(&tcp[2..4], &self.port.to_be_bytes());
                let payload = &tcp[usize::from(tcp[12] >> 4) * 4..];
                if payload.is_empty() {
                    continue;
                }
                assert_eq!(payload, expected);
                self.ack = u32::from_be_bytes(tcp[4..8].try_into().unwrap())
                    .wrapping_add(payload.len() as u32);
                self.send(link, &[]);
                return;
            }
        }
    }

    fn expect_no_data(stream: &mut impl Read) {
        let error = stream.read(&mut [0]).expect_err("unexpected data or EOF");
        assert!(
            matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "{error}"
        );
    }

    fn exchange_dhcp(link: &mut DuplexStream) {
        write_frame(link, &frame([0xff; 6], ETH_IPV4, &dhcp(1))).unwrap();
        let mut reply = Vec::new();
        read_frame(link, &mut reply).unwrap();
        assert_eq!(&reply[..6], &[0xff; 6]);
    }

    #[test]
    fn held_dns_waits_for_open_while_rechecking_answers_the_fixed_question() {
        let upstream = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        upstream.set_nonblocking(true).unwrap();
        let hop = serde_yaml::from_str(&format!("{{address: '{}', protocol: {{type: http}}}}", upstream.local_addr().unwrap())).unwrap();
        let dir = std::env::temp_dir().join(format!("netgw-aaaa-{}", std::process::id()));
        let gateway = Gateway::start(&Endpoint::Unix(dir.join("net.sock")), vec![], &Upstream::Chain(vec![hop]), (Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let mut link = Endpoint::Unix(dir.join("net.sock").into()).connect(Duration::from_secs(2)).unwrap();
        link.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        exchange_dhcp(&mut link);
        let mut query = vec![0u8; 12];
        query[..2].copy_from_slice(&[0x12, 0x34]);
        query[2] = 1;
        query[5] = 1;
        query.extend_from_slice(b"\x07example\x03com\x00\x00\x1c\x00\x01");
        let packet = ipv4_udp(GUEST_IP, PUBLIC_PROBE_V4, 50000, 53, &query);
        write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &packet)).unwrap();
        let mut response = Vec::new();
        read_frame(&mut link, &mut response).unwrap();
        assert_eq!(&response[14 + 28..], dns_ipv6_nodata(&query).unwrap().as_slice());
        assert!(upstream.accept().is_err(), "AAAA must not create an upstream connection");
        let mut probe = query[..12].to_vec();
        probe.extend_from_slice(b"\x03one\x03one\x03one\x03one\x00\x00\x1c\x00\x01");
        let held_probe = ipv4_udp(GUEST_IP, PUBLIC_PROBE_V4, 50001, 53, &probe);
        gateway.set_egress_mode(EgressMode::Suspended);
        let before = gateway.traffic();
        link.set_read_timeout(Some(Duration::from_millis(250))).unwrap();
        write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &packet)).unwrap();
        write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &held_probe)).unwrap();
        exchange_dhcp(&mut link);
        expect_no_data(&mut link);

        gateway.set_egress_mode(EgressMode::Rechecking);
        write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &held_probe)).unwrap();
        let active_probe = ipv4_udp(GUEST_IP, PUBLIC_PROBE_V4, 50002, 53, &probe);
        write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &active_probe)).unwrap();
        read_frame(&mut link, &mut response).unwrap();
        assert_eq!(&response[14 + 22..14 + 24], &50002u16.to_be_bytes());
        assert_eq!(&response[14 + 28..], dns_ipv6_nodata(&probe).unwrap().as_slice());
        expect_no_data(&mut link);
        assert_eq!(gateway.traffic(), before);
        assert!(upstream.accept().is_err());

        gateway.set_egress_mode(EgressMode::Open);
        link.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        for question in [&query, &probe] {
            read_frame(&mut link, &mut response).unwrap();
            assert_eq!(&response[14 + 28..], dns_ipv6_nodata(question).unwrap().as_slice());
        }
        {
            let _egress = gateway.shared.egress.lock().unwrap();
            assert_eq!(
                gateway.traffic().tx_bytes,
                before.tx_bytes + (packet.len() + held_probe.len()) as u64,
            );
        }
        assert!(upstream.accept().is_err(), "AAAA must not create an upstream connection");
        drop(link);
        drop(gateway);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn hold_packet_queue_is_bounded_and_retries_a_full_socket() {
        let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        proxy.set_nonblocking(true).unwrap();
        let hop = serde_yaml::from_str(&format!(
            "{{address: '{}', protocol: {{type: http}}}}", proxy.local_addr().unwrap()
        )).unwrap();
        let dir = std::env::temp_dir().join(format!("netgw-hold-{}", std::process::id()));
        let socket = dir.join("net.sock");
        let gateway = Gateway::start_closed(
            &Endpoint::Unix(socket.clone()), vec![], Some(&Upstream::Chain(vec![hop])), (Ipv4Addr::LOCALHOST, 0).into(),
        ).unwrap();
        gateway.set_egress_mode(EgressMode::Suspended);
        let mut link = Endpoint::Unix(socket.clone()).connect(Duration::from_secs(2)).unwrap();
        link.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let syn = tcp_syn(GUEST_IP, 40001, PUBLIC_PROBE_V4, 443);
        write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &syn)).unwrap();
        exchange_dhcp(&mut link);
        gateway.set_egress_mode(EgressMode::Rechecking);
        // The retransmitted SYN must stay in the ordinary lane even though
        // it also matches the verification endpoint.
        write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &syn)).unwrap();
        let udp = |index: usize| ipv4_udp(GUEST_IP, PUBLIC_PROBE_V4, 40002, 443, &index.to_be_bytes());
        for index in 0..HELD_PACKETS {
            write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &udp(index))).unwrap();
        }
        exchange_dhcp(&mut link);
        assert!(proxy.accept().is_err());
        assert_eq!(gateway.traffic(), GatewayTraffic::default());
        assert_eq!(gateway.shared.egress.lock().unwrap().held_packets.len(), HELD_PACKETS);

        // Drain to the packet socket directly so all TCP/UDP stays local.
        gateway.set_egress_mode(EgressMode::Suspended);
        let path = &gateway.shared.paths[Lane::Ordinary as usize];
        let filler = [0; MTU];
        let mut fillers = 0;
        while path.host.send(&filler).is_ok() {
            fillers += 1;
            assert!(fillers < 10_000, "socket must eventually exert backpressure");
        }
        // Small packets may still fit after the last MTU-sized packet.
        while path.host.send(&filler[..syn.len()]).is_ok() {
            fillers += 1;
        }
        {
            let mut egress = gateway.shared.egress.lock().unwrap();
            egress.mode = EgressMode::Open;
            gateway.shared.held_ready.notify_one();
        }
        std::thread::sleep(Duration::from_millis(25));
        assert_eq!(gateway.traffic(), GatewayTraffic::default());
        assert_eq!(gateway.shared.egress.lock().unwrap().held_packets.len(), HELD_PACKETS);
        let deadline = Instant::now() + Duration::from_secs(5);
        let recv = |expected: &[u8]| {
            let mut data = [0; MTU];
            loop {
                match path.stack.recv(&mut data) {
                    Ok(n) => {
                        assert_eq!(&data[..n], expected);
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "held packet did not arrive");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        };
        // A Unix datagram write is all-or-nothing, including under pressure.
        for _ in 0..fillers {
            let n = path.stack.recv(&mut [0; MTU]).unwrap();
            assert!(n == MTU || n == syn.len());
        }
        recv(&syn);
        recv(&syn);
        for index in 0..HELD_PACKETS - 2 {
            recv(&udp(index));
        }
        assert!(path.stack.recv(&mut [0; MTU]).is_err());
        drop(link);
        drop(gateway);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn close_upstream_switch_and_guest_disconnect_discard_hold() {
        let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let hop = serde_yaml::from_str(&format!(
            "{{address: '{}', protocol: {{type: http}}}}", proxy.local_addr().unwrap()
        )).unwrap();
        let dir = std::env::temp_dir().join(format!("netgw-discard-{}", std::process::id()));
        let socket = dir.join("net.sock");
        let gateway = Gateway::start_closed(
            &Endpoint::Unix(socket.clone()), vec![], Some(&Upstream::Chain(vec![hop])), (Ipv4Addr::LOCALHOST, 0).into(),
        ).unwrap();
        let mut link = Endpoint::Unix(socket.clone()).connect(Duration::from_secs(2)).unwrap();
        link.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut query = vec![0u8; 12];
        query[5] = 1;
        query.extend_from_slice(b"\x07example\x03com\x00\x00\x1c\x00\x01");
        let dns = ipv4_udp(GUEST_IP, PUBLIC_PROBE_V4, 50000, 53, &query);
        let udp = ipv4_udp(GUEST_IP, PUBLIC_PROBE_V4, 50000, 443, b"held");
        for discard in ["close", "upstream", "disconnect"] {
            gateway.set_egress_mode(EgressMode::Suspended);
            write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &udp)).unwrap();
            for _ in 0..=HELD_DNS {
                write_frame(&mut link, &frame(GATEWAY_MAC, ETH_IPV4, &dns)).unwrap();
            }
            exchange_dhcp(&mut link);
            {
                let egress = gateway.shared.egress.lock().unwrap();
                assert_eq!(egress.held_packets.len(), 1);
                assert_eq!(egress.held_dns.len(), HELD_DNS);
            }
            match discard {
                "close" => gateway.set_egress_mode(EgressMode::Closed),
                "upstream" => gateway.set_upstream(&Upstream::Direct).unwrap(),
                "disconnect" => {
                    drop(link);
                    link = Endpoint::Unix(socket.clone()).connect(Duration::from_secs(2)).unwrap();
                    link.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    exchange_dhcp(&mut link);
                }
                _ => unreachable!(),
            }
            {
                let egress = gateway.shared.egress.lock().unwrap();
                assert!(egress.held_packets.is_empty(), "{discard}");
                assert!(egress.held_dns.is_empty(), "{discard}");
            }
            gateway.set_egress_mode(EgressMode::Open);
            exchange_dhcp(&mut link);
            assert_eq!(gateway.traffic(), GatewayTraffic::default(), "{discard}");
        }
        drop(link);
        drop(gateway);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn suspended_gateway_can_close_repeatedly_without_losing_a_wakeup() {
        let (finished, wait) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let dir = std::env::temp_dir().join(format!("netgw-wake-{}", std::process::id()));
            let gateway = Gateway::start(
                &Endpoint::Unix(dir.join("net.sock")),
                Vec::new(),
                &Upstream::Direct,
                (Ipv4Addr::LOCALHOST, 0).into(),
            )
            .unwrap();
            for _ in 0..12 {
                gateway.set_egress_mode(EgressMode::Suspended);
                gateway.set_egress_mode(EgressMode::Closed);
                gateway.set_egress_mode(EgressMode::Open);
            }
            drop(gateway);
            let _ = std::fs::remove_dir_all(dir);
            finished.send(()).unwrap();
        });
        wait.recv_timeout(Duration::from_secs(15))
            .expect("a suspended gateway must finish closing");
        thread.join().unwrap();
    }

    #[test]
    fn gateway_parks_tcp_in_both_directions_and_hard_close_kills_old_paths() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let hop = serde_yaml::from_str(&format!(
            "{{address: '{}', protocol: {{type: http}}}}",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let upstream = Upstream::Chain(vec![hop]);
        let dir = std::env::temp_dir().join(format!("netgw-modes-{}", std::process::id()));
        let socket = dir.join("net.sock");
        let gateway = Gateway::start(
            &Endpoint::Unix(socket.clone()),
            Vec::new(),
            &upstream,
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .unwrap();
        let mut link = Endpoint::Unix(socket.clone()).connect(Duration::from_secs(2)).unwrap();
        let normal_timeout = Some(Duration::from_secs(5));
        let quiet_timeout = Some(Duration::from_millis(150));
        link.set_read_timeout(normal_timeout).unwrap();
        exchange_dhcp(&mut link);
        assert_eq!(gateway.traffic(), GatewayTraffic::default());

        let mut guest = GuestTcp::start(&mut link, 40000);
        let mut remote = accept_proxy(&listener);
        guest.send(&mut link, b"before");
        let mut received = [0; 6];
        remote.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"before");
        guest.seq += 6;
        remote.write_all(b"ready").unwrap();
        guest.receive(&mut link, b"ready");
        exchange_dhcp(&mut link); // all previous guest frames have been processed
        gateway.set_egress_mode(EgressMode::Suspended);
        let before = gateway.traffic();
        assert_eq!(before.tx_bytes, 40 + 40 + (40 + 6) + 40);
        assert!(before.rx_bytes >= 40 + 45);

        guest.send(&mut link, b"held");
        remote.write_all(b"queued").unwrap();
        exchange_dhcp(&mut link);
        remote.set_read_timeout(quiet_timeout).unwrap();
        link.set_read_timeout(quiet_timeout).unwrap();
        expect_no_data(&mut remote);
        expect_no_data(&mut link);
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(gateway.traffic(), before);

        gateway.set_egress_mode(EgressMode::Rechecking);
        link.set_read_timeout(normal_timeout).unwrap();
        // Packets from the existing ordinary connection share the fixed probe
        // destination. Only a fresh connection can use the verification stack.
        guest.send(&mut link, b"held");
        let mut verify = GuestTcp::start(&mut link, 40002);
        let mut probe = accept_proxy(&listener);
        verify.send(&mut link, b"verify");
        probe.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"verify");
        verify.seq += 6;
        probe.write_all(b"probe response").unwrap();
        verify.receive(&mut link, b"probe response");
        exchange_dhcp(&mut link);
        expect_no_data(&mut remote);
        assert_eq!(gateway.traffic(), before);

        gateway.set_egress_mode(EgressMode::Open);
        assert_eq!(probe.read(&mut [0]).unwrap(), 0);
        remote.set_read_timeout(normal_timeout).unwrap();
        let mut held = [0; 4];
        remote.read_exact(&mut held).unwrap();
        assert_eq!(&held, b"held");
        guest.seq += 4;
        guest.receive(&mut link, b"queued");
        guest.send(&mut link, b"resume");
        remote.read_exact(&mut received).unwrap();
        assert_eq!(&received, b"resume");
        guest.seq += 6;
        gateway.set_egress_mode(EgressMode::Closed);
        let after = gateway.traffic();
        assert!(after.rx_bytes > before.rx_bytes && after.tx_bytes > before.tx_bytes);

        let mut host =
            TcpStream::connect(gateway.proxy_url().unwrap().trim_start_matches("http://")).unwrap();
        host.set_read_timeout(normal_timeout).unwrap();
        host.write_all(b"CONNECT 1.1.1.1:443 HTTP/1.1\r\nHost: 1.1.1.1:443\r\n\r\n")
            .unwrap();
        let _host_remote = accept_proxy(&listener);
        let mut response = [0; 8];
        host.read_exact(&mut response).unwrap();
        assert_eq!(&response, b"HTTP/1.1");
        assert_eq!(gateway.traffic(), after);

        gateway.set_egress_mode(EgressMode::Open);
        assert_eq!(
            remote.read(&mut [0]).unwrap(),
            0,
            "old upstream socket survived hard close"
        );
        assert_eq!(gateway.traffic(), after);
        guest.send(&mut link, b"old flow");
        let mut reply = Vec::new();
        let mut reset = false;
        for _ in 0..20 {
            read_frame(&mut link, &mut reply).unwrap();
            let (_, tcp) = tcp_packet(&reply[14..]).unwrap();
            assert_eq!(&tcp[2..4], &guest.port.to_be_bytes());
            if tcp[13] & 4 != 0 {
                reset = true;
                break;
            }
            // An ACK written to the QEMU socket before closure can still be
            // queued there. It must not contain revived upstream data.
            let header = usize::from(tcp[12] >> 4) * 4;
            assert!(tcp[header..].is_empty(), "old flow carried data after closure");
        }
        assert!(reset, "old flow must reset");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        let after_reset = {
            // The peer can receive the reset before the writer updates counters.
            let _egress = gateway.shared.egress.locked();
            gateway.traffic()
        };
        gateway.set_upstream(&upstream).unwrap();
        assert_eq!(gateway.traffic(), after_reset);
        let _fresh = GuestTcp::start(&mut link, 40003);
        let _fresh_remote = accept_proxy(&listener);
        assert!(gateway.traffic().tx_bytes > after.tx_bytes);
        assert!(gateway.error().is_none());
        drop(gateway);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gateway_speaks_qemu_stream_framing() {
        let dir = std::env::temp_dir().join(format!("netgw-{}", std::process::id()));
        let socket = dir.join("net.sock");
        let gateway = Gateway::start(
            &Endpoint::Unix(socket.clone()),
            vec![Ipv4Addr::new(9, 9, 9, 9)],
            &Upstream::Direct,
            (Ipv4Addr::LOCALHOST, 0).into(),
        )
        .unwrap();
        let mut qemu = Endpoint::Unix(socket.clone()).connect(Duration::from_secs(2)).unwrap();
        qemu.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write_frame(&mut qemu, &frame([0xff; 6], ETH_IPV4, &dhcp(1))).unwrap();
        let mut reply = Vec::new();
        read_frame(&mut qemu, &mut reply).expect("a DHCP offer");
        assert_eq!(&reply[0..6], &[0xff; 6]);
        assert_eq!(&reply[14 + 28 + 16..14 + 28 + 20], &GUEST_IP.octets());
        assert!(gateway.connected());
        drop(gateway);
        assert!(!socket.exists(), "socket removed on drop");
        let _ = std::fs::remove_dir_all(dir);
    }
}
