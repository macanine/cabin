//! Frame builders shared by the gateway's tests.

use super::frame::{ETH_IPV4, checksum, ipv4_udp};
use super::{GATEWAY_MAC, GUEST_IP, GUEST_MAC};
use std::net::Ipv4Addr;

pub(super) fn ipv4_frame(protocol: u8, dst: Ipv4Addr, transport: &[u8]) -> Vec<u8> {
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

pub(super) fn frame(dst: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = dst.to_vec();
    f.extend_from_slice(&GUEST_MAC);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

pub(super) fn dhcp(kind: u8) -> Vec<u8> {
    let mut bootp = vec![0u8; 240];
    bootp[0] = 1;
    bootp[4..8].copy_from_slice(&[1, 2, 3, 4]);
    bootp[28..34].copy_from_slice(&GUEST_MAC);
    bootp[236..240].copy_from_slice(&[99, 130, 83, 99]);
    bootp.extend_from_slice(&[53, 1, kind, 255]);
    ipv4_udp(Ipv4Addr::UNSPECIFIED, Ipv4Addr::BROADCAST, 68, 67, &bootp)
}

pub(super) fn tcp_syn(src: Ipv4Addr, sport: u16, dst: Ipv4Addr, dport: u16) -> Vec<u8> {
    tcp_segment((src, sport), (dst, dport), 1000, 0, 0x02, &[])
}

pub(super) fn tcp_segment(
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
