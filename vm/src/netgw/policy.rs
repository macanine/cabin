//! Egress modes and the packet classification each mode admits.

use super::{GUEST_IP, MTU};
use crate::network_probe_targets::{ALTERNATE_PROBE_V4, PUBLIC_PROBE_V4};
use std::net::Ipv4Addr;

use super::dns::dns_question;

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
pub(super) enum Lane {
    Ordinary = 0,
    Verification = 1,
}

impl EgressMode {
    pub fn checking() -> Self {
        Self::Checking
    }

    pub(super) fn lane(self) -> Option<Lane> {
        match self {
            Self::Open | Self::Install => Some(Lane::Ordinary),
            Self::Checking | Self::Rechecking => Some(Lane::Verification),
            Self::Closed | Self::Suspended => None,
        }
    }

    pub(super) fn holds_ordinary(self) -> bool {
        matches!(self, Self::Suspended | Self::Rechecking)
    }

    pub(super) fn allows_packet(self, packet: &[u8], to_guest: bool) -> bool {
        // Both sides of the packet socket must remain IPv4-only, even in Open.
        if packet.len() < 20 || packet[0] >> 4 != 4 {
            return false;
        }
        match self {
            Self::Open => true,
            Self::Install => tcp_packet(packet)
                .is_some_and(|(ip, tcp)| matches!(remote_endpoint(ip, tcp, to_guest).1, 80 | 443)),
            Self::Checking | Self::Rechecking => tcp_packet(packet)
                .is_some_and(|(ip, tcp)| verification_endpoint(remote_endpoint(ip, tcp, to_guest))),
            Self::Closed | Self::Suspended => false,
        }
    }

    pub(super) fn allows_dns(self, query: &mut Vec<u8>) -> bool {
        match self {
            Self::Open | Self::Install => dns_question(query).is_some(),
            Self::Checking | Self::Rechecking => restrict_verification_dns(query),
            Self::Closed | Self::Suspended => false,
        }
    }
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

pub(super) fn tcp_packet(packet: &[u8]) -> Option<(&[u8], &[u8])> {
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

pub(super) fn outbound_probe_tcp(packet: &[u8]) -> Option<((Ipv4Addr, u16), &[u8])> {
    let (ip, tcp) = tcp_packet(packet)?;
    let remote = remote_endpoint(ip, tcp, false);
    if ip[12..16] != GUEST_IP.octets() || !verification_endpoint(remote) {
        return None;
    }
    let local_port = u16::from_be_bytes([tcp[0], tcp[1]]);
    Some(((remote.0, local_port), tcp))
}

/// `handle_frame` has already checked the IPv4 lengths and destination.
pub(super) fn hold_packet(packet: &[u8]) -> bool {
    if packet.len() > MTU
        || packet[12..16] != GUEST_IP.octets()
        || u16::from_be_bytes([packet[6], packet[7]]) & 0x3fff != 0
    {
        return false;
    }
    let header = usize::from(packet[0] & 0x0f) * 4;
    let transport = &packet[header..];
    match packet[9] {
        6 if transport.len() >= 20 => {
            (20..=transport.len()).contains(&(usize::from(transport[12] >> 4) * 4))
        }
        17 if transport.len() >= 8 => {
            transport[2..4] != 53u16.to_be_bytes()
                && (8..=transport.len()).contains(&usize::from(u16::from_be_bytes([
                    transport[4],
                    transport[5],
                ])))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netgw::frame::{Action, ETH_IPV4, handle_frame};
    use crate::netgw::testutil::*;
    use crate::netgw::{DEFAULT_DNS, GATEWAY_MAC};

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
        assert_eq!(
            handle_frame(&frame(GATEWAY_MAC, ETH_IPV4, &ipv6), &DEFAULT_DNS),
            Action::Drop
        );
        assert_eq!(
            handle_frame(&frame(GATEWAY_MAC, 0x86dd, &ipv6), &DEFAULT_DNS),
            Action::Drop
        );
    }

    #[test]
    fn checking_mode_only_admits_the_fixed_probe_targets() {
        let mut tcp = vec![0u8; 20];
        tcp[2..4].copy_from_slice(&443u16.to_be_bytes());
        let allowed = ipv4_frame(6, PUBLIC_PROBE_V4, &tcp)[14..].to_vec();
        let alternate = ipv4_frame(6, ALTERNATE_PROBE_V4, &tcp)[14..].to_vec();
        let unlisted = ipv4_frame(6, Ipv4Addr::new(104, 16, 124, 96), &tcp)[14..].to_vec();
        assert_ne!(
            outbound_probe_tcp(&allowed).unwrap().0,
            outbound_probe_tcp(&alternate).unwrap().0
        );
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
}
