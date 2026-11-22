//! Bounded, read-only observations used by the desktop network gate.
//!
//! Every target is fixed in this module. The probe never reads URLs, hosts,
//! ports or commands from its caller, and it ignores inherited proxy settings.

use std::io::Read;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::model::{EnvironmentState, GuestNetworkProbe};
use crate::util::now_millis;

#[path = "../../vm/src/network_probe_targets.rs"]
mod targets;
use targets::{ALTERNATE_HTTPS_URL, ALTERNATE_PROBE_V4, HTTPS_URL, PUBLIC_PROBE_V4};

const DNS_NAME: &str = "one.one.one.one";
const PUBLIC_V6: Ipv6Addr = Ipv6Addr::new(0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111);
const METADATA_V4: Ipv4Addr = Ipv4Addr::new(169, 254, 169, 254);
const PRIVATE_V4: Ipv4Addr = Ipv4Addr::new(192, 168, 50, 1);
const GATEWAY_V4: Ipv4Addr = Ipv4Addr::new(10, 0, 3, 1);

fn elapsed_ms(started: Instant) -> u32 {
    started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

fn resolve_with_timeout(timeout: Duration) -> (bool, Option<u32>) {
    let (reply, wait) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = reply.send(
            (DNS_NAME, 443)
                .to_socket_addrs()
                .map(|mut addresses| addresses.any(|address| address.is_ipv4())),
        );
    });
    let started = Instant::now();
    let ok = wait
        .recv_timeout(timeout)
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false);
    (ok, Some(elapsed_ms(started)))
}

fn connect_with_timeout(address: SocketAddr, timeout: Duration) -> (bool, Option<u32>) {
    let started = Instant::now();
    let ok = TcpStream::connect_timeout(&address, timeout).is_ok();
    (ok, Some(elapsed_ms(started)))
}

struct TransportObservation {
    connect_ok: bool,
    connect_ms: Option<u32>,
    https_ms: u32,
    exit: Result<(String, String), String>,
}

fn observe_transport(url: &str, ip: Ipv4Addr) -> TransportObservation {
    let timeout = Duration::from_secs(3);
    let address = SocketAddr::from((ip, 443));
    let connect = std::thread::spawn(move || connect_with_timeout(address, timeout));
    // Keep the URL hostname for TLS verification, and use exactly the IPv4
    // destination that the gateway admits during a closed check.
    let agent = ureq::AgentBuilder::new()
        .resolver(move |_: &str| Ok(vec![address]))
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(timeout)
        .timeout_connect(timeout)
        .build();
    let started = Instant::now();
    let exit = (|| {
        let response = agent.get(url).call().map_err(|error| error.to_string())?;
        if response.status() != 200 {
            return Err(format!("{url} returned HTTP {}", response.status()));
        }
        let mut bytes = Vec::new();
        response.into_reader().take(32 * 1024).read_to_end(&mut bytes).map_err(|error| error.to_string())?;
        let (ip, location) = parse_trace(&String::from_utf8_lossy(&bytes));
        ip.zip(location).ok_or_else(|| format!("{url} did not report a public IPv4 address and country"))
    })();
    let https_ms = elapsed_ms(started);
    let (connect_ok, connect_ms) = connect.join().unwrap_or_default();
    TransportObservation {
        connect_ok: connect_ok || exit.is_ok(),
        connect_ms: connect_ms.filter(|_| connect_ok),
        https_ms,
        exit,
    }
}

fn probe_transport() -> TransportObservation {
    let primary = observe_transport(HTTPS_URL, PUBLIC_PROBE_V4);
    let Err(primary_error) = &primary.exit else { return primary };
    let mut alternate = observe_transport(ALTERNATE_HTTPS_URL, ALTERNATE_PROBE_V4);
    if let Err(alternate_error) = &alternate.exit {
        alternate.exit = Err(format!("{primary_error}. Alternate probe: {alternate_error}"));
    }
    alternate
}

fn parse_trace(text: &str) -> (Option<String>, Option<String>) {
    let value = |key: &str| {
        text.lines()
            .find_map(|line| line.split_once('=').filter(|(name, _)| *name == key))
            .map(|(_, value)| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    (
        value("ip").and_then(|value| public_v4(&value)),
        value("loc"),
    )
}

fn public_v4(value: &str) -> Option<String> {
    let address = value.parse::<Ipv4Addr>().ok()?;
    let [first, second, ..] = address.octets();
    let private = first == 0
        || first == 10
        || (first == 100 && (64..128).contains(&second))
        || first == 127
        || (first == 169 && second == 254)
        || (first == 172 && (16..32).contains(&second))
        || (first == 192 && second == 168)
        || (first == 198 && matches!(second, 18 | 19))
        || first >= 224;
    (!private).then(|| address.to_string())
}

fn dns_query() -> Vec<u8> {
    let mut query = vec![0u8; 12 + 17 + 4];
    query[0..2].copy_from_slice(&0x5a17u16.to_be_bytes());
    query[2] = 1;
    query[4..6].copy_from_slice(&1u16.to_be_bytes());
    let mut index = 12;
    for label in ["blocked", "invalid"] {
        query[index] = label.len() as u8;
        index += 1;
        query[index..index + label.len()].copy_from_slice(label.as_bytes());
        index += label.len();
    }
    query[index] = 0;
    query[index + 1..index + 3].copy_from_slice(&1u16.to_be_bytes());
    query[index + 3..index + 5].copy_from_slice(&1u16.to_be_bytes());
    query
}

fn udp_blocked(timeout: Duration) -> bool {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return false;
    };
    if socket.set_read_timeout(Some(timeout)).is_err()
        || socket.send_to(&dns_query(), (PUBLIC_PROBE_V4, 53)).is_err()
    {
        return false;
    }
    let mut reply = [0u8; 512];
    socket.recv_from(&mut reply).is_err()
}

fn blocked_ipv4(address: Ipv4Addr) -> bool {
    TcpStream::connect_timeout(&(address, 80).into(), Duration::from_millis(900)).is_err()
}

fn blocked_public_port() -> bool {
    TcpStream::connect_timeout(&(PUBLIC_PROBE_V4, 80).into(), Duration::from_millis(900)).is_err()
}

fn blocked_ipv6() -> bool {
    TcpStream::connect_timeout(
        &SocketAddr::new(PUBLIC_V6.into(), 443),
        Duration::from_millis(900),
    )
    .is_err()
}

/// Run every fixed observation. Slow boundary checks run concurrently with
/// the permitted path so the complete operation stays within a bounded time.
pub fn probe(environment: EnvironmentState, check_closed_boundary: bool) -> GuestNetworkProbe {
    let boundary_timeout = Duration::from_millis(900);
    let ipv6 = std::thread::spawn(blocked_ipv6);
    let ipv4 = std::thread::spawn(move || {
        blocked_ipv4(METADATA_V4) && blocked_ipv4(PRIVATE_V4) && blocked_ipv4(GATEWAY_V4)
    });
    let public_port = check_closed_boundary.then(|| std::thread::spawn(blocked_public_port));
    let udp = std::thread::spawn(move || udp_blocked(boundary_timeout));
    let handles = [ipv6, ipv4, udp];
    let dns = std::thread::spawn(|| resolve_with_timeout(Duration::from_secs(3)));
    let transport = probe_transport();
    let (public_ip, trace_location, error) = match transport.exit {
        Ok((ip, location)) => (Some(ip), Some(location), None),
        Err(error) => (None, None, Some(error)),
    };

    let mut boundary = handles
        .into_iter()
        .map(|handle| handle.join().unwrap_or(false));
    let ipv6_blocked = boundary.next().unwrap_or(false);
    let private_blocked = boundary.next().unwrap_or(false);
    let unlisted_blocked = public_port.map(|probe| probe.join().unwrap_or(false));
    let (dns_ok, dns_ms) = dns.join().unwrap_or_default();
    let udp_blocked = boundary.next().unwrap_or(false);
    let hostname = std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok());
    let username = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok());
    let timezone = environment
        .system_timezone
        .or(Some(environment.committed.timezone))
        .filter(|value| !value.is_empty());
    let language = environment
        .system_language
        .or(Some(environment.committed.language))
        .filter(|value| !value.is_empty());

    GuestNetworkProbe {
        checked_at: now_millis(),
        hostname,
        username,
        timezone,
        language,
        dns_ok,
        dns_ms,
        connect_ok: transport.connect_ok,
        connect_ms: transport.connect_ms,
        https_ok: public_ip.is_some(),
        https_ms: Some(transport.https_ms),
        public_ip,
        trace_location,
        ipv6_blocked,
        private_blocked,
        unlisted_blocked,
        udp_blocked,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_values_are_parsed_without_trusting_other_lines() {
        assert_eq!(
            parse_trace("fl=1\nip=203.0.113.8\nloc=JP\n"),
            (Some("203.0.113.8".into()), Some("JP".into()))
        );
        assert_eq!(parse_trace("ip=\nloc=\n"), (None, None));
        assert_eq!(
            parse_trace("ip=10.0.0.1\nloc=US\n"),
            (None, Some("US".into()))
        );
        assert_eq!(
            parse_trace("ip=2001:db8::1\nloc=US\n"),
            (None, Some("US".into()))
        );
    }

    #[test]
    fn boundary_query_has_one_fixed_question() {
        let query = dns_query();
        assert_eq!(u16::from_be_bytes([query[4], query[5]]), 1);
        assert!(query.windows(7).any(|part| part == b"blocked"));
        assert!(query.windows(7).any(|part| part == b"invalid"));
    }
}
