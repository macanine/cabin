use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Instant;

use super::*;
use super::frame::{ETH_IPV4, ipv4_udp, read_frame, write_frame};
use super::dns::dns_ipv6_nodata;
use super::policy::{Lane, tcp_packet};
use super::relay::{HELD_DNS, HELD_PACKETS};
use super::testutil::*;
use crate::network_probe_targets::PUBLIC_PROBE_V4;
use crate::sockets::UnixStream;

#[test]
fn a_taken_ssh_port_moves_the_relay_to_a_free_one() {
    let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    let dir = std::env::temp_dir().join(format!("netgw-taken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let gateway = Gateway::start(
        &dir.join("net.sock"),
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
fn gateway_stops_with_a_partial_frame_and_can_restart() {
    let dir = crate::sockets::test_dir();
    let socket = dir.join("net.sock");
    let ssh = (Ipv4Addr::LOCALHOST, 0).into();
    let gateway = Gateway::start_closed(&socket, Vec::new(), None, ssh).unwrap();
    let mut guest = UnixStream::connect(&socket).unwrap();
    // Announce a frame but leave its payload incomplete and the connection open.
    guest.write_all(&[0, 0, 0, 64, 0]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !gateway.connected() {
        assert!(Instant::now() < deadline, "gateway did not accept the guest");
        std::thread::sleep(Duration::from_millis(5));
    }

    let (stopped, done) = std::sync::mpsc::channel();
    let shutdown = std::thread::spawn(move || {
        drop(gateway);
        stopped.send(()).unwrap();
    });
    done.recv_timeout(Duration::from_secs(5))
        .expect("gateway did not stop while the guest was still connected");
    shutdown.join().unwrap();
    assert!(!socket.exists());
    drop(guest);

    let restarted = Gateway::start_closed(&socket, Vec::new(), None, ssh).unwrap();
    drop(restarted);
    std::fs::remove_dir_all(dir).unwrap();
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
    fn start(link: &mut UnixStream, port: u16) -> Self {
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

    fn send(&self, link: &mut UnixStream, data: &[u8]) {
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

    fn receive(&mut self, link: &mut UnixStream, expected: &[u8]) {
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

fn exchange_dhcp(link: &mut UnixStream) {
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
    let gateway = Gateway::start(&dir.join("net.sock"), vec![], &Upstream::Chain(vec![hop]), (Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let mut link = UnixStream::connect(dir.join("net.sock")).unwrap();
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
fn hold_packet_queue_is_bounded_and_flushes_in_order() {
    let proxy = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    proxy.set_nonblocking(true).unwrap();
    let hop = serde_yaml::from_str(&format!(
        "{{address: '{}', protocol: {{type: http}}}}", proxy.local_addr().unwrap()
    )).unwrap();
    let dir = std::env::temp_dir().join(format!("netgw-hold-{}", std::process::id()));
    let socket = dir.join("net.sock");
    let gateway = Gateway::start_closed(
        &socket, vec![], Some(&Upstream::Chain(vec![hop])), (Ipv4Addr::LOCALHOST, 0).into(),
    ).unwrap();
    gateway.set_egress_mode(EgressMode::Suspended);
    let mut link = UnixStream::connect(&socket).unwrap();
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

    // Flush through the packet socket directly so all TCP/UDP stays local.
    gateway.set_egress_mode(EgressMode::Suspended);
    let path = &gateway.shared.paths[Lane::Ordinary as usize];
    {
        let mut egress = gateway.shared.egress.lock().unwrap();
        egress.mode = EgressMode::Open;
        gateway.shared.held_ready.notify_one();
    }
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
    recv(&syn);
    recv(&syn);
    for index in 0..HELD_PACKETS - 2 {
        recv(&udp(index));
    }
    assert!(path.stack.recv(&mut [0; MTU]).is_err());
    assert!(gateway.shared.egress.lock().unwrap().held_packets.is_empty());
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
        &socket, vec![], Some(&Upstream::Chain(vec![hop])), (Ipv4Addr::LOCALHOST, 0).into(),
    ).unwrap();
    let mut link = UnixStream::connect(&socket).unwrap();
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
                link = UnixStream::connect(&socket).unwrap();
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
            &dir.join("net.sock"),
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
        &socket,
        Vec::new(),
        &upstream,
        (Ipv4Addr::LOCALHOST, 0).into(),
    )
    .unwrap();
    let mut link = UnixStream::connect(&socket).unwrap();
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
        &socket,
        vec![Ipv4Addr::new(9, 9, 9, 9)],
        &Upstream::Direct,
        (Ipv4Addr::LOCALHOST, 0).into(),
    )
    .unwrap();
    let mut qemu = UnixStream::connect(&socket).unwrap();
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
