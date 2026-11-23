//! Ethernet, ARP, IPv4/UDP, DHCP and NTP framing for the VM's NIC, and the
//! length-prefixed QEMU stream framing.

use super::{GATEWAY_IP, GATEWAY_MAC, GUEST_IP, GUEST_MAC, MTU, NETMASK};
use std::io::{Read, Write};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};

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

pub(super) const ETH_IPV4: u16 = 0x0800;
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

pub(super) fn checksum(data: &[u8]) -> u16 {
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

pub(super) fn ipv4_udp(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
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

#[cfg(test)]
pub(super) fn read_frame(stream: &mut impl Read, buffer: &mut Vec<u8>) -> std::io::Result<()> {
    read_frame_until(stream, buffer, &AtomicBool::new(false))
}

/// Read one length-prefixed frame. The stream has a short read timeout so
/// a stop request is seen without relying on another thread's shutdown()
/// waking a blocked read (not dependable for AF_UNIX on Windows); bytes
/// already read are kept, so a pause mid-frame never loses framing.
pub(super) fn read_frame_until(stream: &mut impl Read, buffer: &mut Vec<u8>, stop: &AtomicBool) -> std::io::Result<()> {
    let mut length = [0u8; 4];
    fill(stream, &mut length, stop)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > 65_536 {
        return Err(std::io::Error::other("oversized frame"));
    }
    buffer.resize(length, 0);
    fill(stream, buffer, stop)
}

fn fill(stream: &mut impl Read, buffer: &mut [u8], stop: &AtomicBool) -> std::io::Result<()> {
    let mut filled = 0;
    while filled < buffer.len() {
        match stream.read(&mut buffer[filled..]) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => filled += read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted
                ) =>
            {
                if stop.load(Ordering::Relaxed) {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(super) fn write_frame(stream: &mut impl Write, frame: &[u8]) -> std::io::Result<()> {
    let mut message = Vec::with_capacity(4 + frame.len());
    message.extend_from_slice(&(frame.len() as u32).to_be_bytes());
    message.extend_from_slice(frame);
    stream.write_all(&message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netgw::testutil::*;
    use crate::netgw::DEFAULT_DNS;
    use std::time::Duration;

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
}
