//! Per-connection HTTP proxy gate for QEMU guestfwd.
//!
//! Resolve hostnames over the configured upstream proxy, reject non-public
//! destinations, and connect to the resolved address. This prevents a guest
//! from using an otherwise unrestricted HTTP proxy to reach the host or LAN.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::{Command, Stdio};
use std::time::Duration;
use url::{Host, Url};

const HEADER_LIMIT: usize = 65536;

fn public_ip(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || a >= 224
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 168)
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113))
}

fn is_host_address(ip: Ipv4Addr) -> io::Result<bool> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut item = list;
    let mut found = false;
    while !item.is_null() {
        let addr = unsafe { (*item).ifa_addr };
        if !addr.is_null() && unsafe { (*addr).sa_family as i32 } == libc::AF_INET {
            let socket = unsafe { &*(addr as *const libc::sockaddr_in) };
            if Ipv4Addr::from(u32::from_be(socket.sin_addr.s_addr)) == ip {
                found = true;
                break;
            }
        }
        item = unsafe { (*item).ifa_next };
    }
    unsafe {
        libc::freeifaddrs(list);
    }
    Ok(found)
}

fn checked_ip(ip: Ipv4Addr) -> io::Result<Ipv4Addr> {
    if !public_ip(ip) || is_host_address(ip)? {
        return Err(io::Error::other("private or host destination"));
    }
    Ok(ip)
}

fn dns_query(host: &str) -> io::Result<Vec<u8>> {
    let mut query = vec![0x76, 0x4d, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 || !label.is_ascii() {
            return Err(io::Error::other("invalid hostname"));
        }
        query.push(label.len() as u8);
        query.extend_from_slice(label.as_bytes());
    }
    query.extend_from_slice(&[0, 0, 1, 0, 1]);
    Ok(query)
}
fn skip_name(packet: &[u8], mut pos: usize) -> io::Result<usize> {
    loop {
        let len = *packet
            .get(pos)
            .ok_or_else(|| io::Error::other("invalid DNS response"))?;
        pos += 1;
        if len == 0 {
            return Ok(pos);
        }
        if len & 0xc0 == 0xc0 {
            return if pos < packet.len() {
                Ok(pos + 1)
            } else {
                Err(io::Error::other("invalid DNS pointer"))
            };
        }
        if len & 0xc0 != 0 || pos + len as usize > packet.len() {
            return Err(io::Error::other("invalid DNS name"));
        }
        pos += len as usize;
    }
}
fn word(packet: &[u8], pos: usize) -> io::Result<u16> {
    let bytes = packet
        .get(pos..pos + 2)
        .ok_or_else(|| io::Error::other("short DNS response"))?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}
fn parse_dns(packet: &[u8]) -> io::Result<Ipv4Addr> {
    if packet.len() < 12
        || packet[..2] != [0x76, 0x4d]
        || packet[2] & 0x80 == 0
        || packet[3] & 0x0f != 0
    {
        return Err(io::Error::other("DNS query failed"));
    }
    let questions = word(packet, 4)?;
    let answers = word(packet, 6)?;
    let mut pos = 12;
    for _ in 0..questions {
        pos = skip_name(packet, pos)?
            .checked_add(4)
            .ok_or_else(|| io::Error::other("invalid DNS"))?;
    }
    for _ in 0..answers {
        pos = skip_name(packet, pos)?;
        let kind = word(packet, pos)?;
        let class = word(packet, pos + 2)?;
        let len = word(packet, pos + 8)? as usize;
        pos += 10;
        let data = packet
            .get(pos..pos + len)
            .ok_or_else(|| io::Error::other("invalid DNS record"))?;
        if kind == 1 && class == 1 && len == 4 {
            let ip = Ipv4Addr::new(data[0], data[1], data[2], data[3]);
            if public_ip(ip) {
                return Ok(ip);
            }
        }
        pos += len;
    }
    Err(io::Error::other("no public IPv4 address"))
}
fn resolve(host: &str, upstream: SocketAddr) -> io::Result<Ipv4Addr> {
    match Host::parse(host).map_err(|_| io::Error::other("invalid host"))? {
        Host::Ipv4(ip) => checked_ip(ip),
        Host::Ipv6(_) => Err(io::Error::other("IPv6 is unavailable")),
        Host::Domain(domain) => {
            if domain.eq_ignore_ascii_case("localhost")
                || domain.ends_with(".localhost")
                || domain.ends_with(".local")
            {
                return Err(io::Error::other("local destination"));
            }
            let query = dns_query(&domain)?;
            let mut child = Command::new("/usr/bin/curl")
                .args([
                    "-q",
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "10",
                    "--noproxy",
                    "",
                    "--proxy",
                    &format!("http://{upstream}"),
                    "--header",
                    "Content-Type: application/dns-message",
                    "--header",
                    "Accept: application/dns-message",
                    "--data-binary",
                    "@-",
                    "https://1.1.1.1/dns-query",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()?;
            child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("DNS stdin unavailable"))?
                .write_all(&query)?;
            let output = child.wait_with_output()?;
            if !output.status.success() || output.stdout.len() > 65535 {
                return Err(io::Error::other("DNS over proxy failed"));
            }
            checked_ip(parse_dns(&output.stdout)?)
        }
    }
}
fn read_header(input: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut byte = [0u8; 1];
    while data.len() < HEADER_LIMIT {
        input.read_exact(&mut byte)?;
        data.push(byte[0]);
        if data.ends_with(b"\r\n\r\n") {
            return Ok(data);
        }
    }
    Err(io::Error::other("HTTP header too large"))
}
fn relay(upstream: SocketAddr) -> io::Result<()> {
    let mut input = io::stdin();
    let mut output = io::stdout();
    let header = read_header(&mut input)?;
    let text = std::str::from_utf8(&header).map_err(|_| io::Error::other("invalid HTTP header"))?;
    let (first, headers) = text
        .split_once("\r\n")
        .ok_or_else(|| io::Error::other("invalid request"))?;
    let mut parts = first.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| io::Error::other("missing method"))?;
    let target = parts
        .next()
        .ok_or_else(|| io::Error::other("missing target"))?;
    let version = parts
        .next()
        .ok_or_else(|| io::Error::other("missing version"))?;
    if parts.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return Err(io::Error::other("invalid request"));
    }
    let connect = method.eq_ignore_ascii_case("CONNECT");
    let url = if connect {
        Url::parse(&format!("http://{target}/"))
    } else {
        Url::parse(target)
    }
    .map_err(|_| io::Error::other("invalid target"))?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || (connect && (url.path() != "/" || url.query().is_some()))
    {
        return Err(io::Error::other("unsupported target"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| io::Error::other("missing host"))?;
    let port = if connect {
        target
            .rsplit_once(':')
            .and_then(|(_, value)| value.parse::<u16>().ok())
            .filter(|port| *port != 0)
            .ok_or_else(|| io::Error::other("missing port"))?
    } else {
        url.port_or_known_default()
            .ok_or_else(|| io::Error::other("missing port"))?
    };
    let ip = resolve(host, upstream)?;
    let mut remote = TcpStream::connect_timeout(&upstream, Duration::from_secs(5))?;
    remote.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        remote,
        "CONNECT {ip}:{port} HTTP/1.1\r\nHost: {ip}:{port}\r\n\r\n"
    )?;
    let response = read_header(&mut remote)?;
    let status = std::str::from_utf8(&response)
        .ok()
        .and_then(|s| s.lines().next())
        .and_then(|s| s.split_whitespace().nth(1));
    if !status.is_some_and(|s| s.starts_with('2')) {
        return Err(io::Error::other("proxy connection failed"));
    }
    remote.set_read_timeout(None)?;
    if connect {
        output.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")?;
        output.flush()?;
    } else {
        let mut path = url.path().to_owned();
        if let Some(query) = url.query() {
            path.push('?');
            path.push_str(query);
        }
        write!(remote, "{method} {path} {version}\r\n")?;
        for line in headers.split("\r\n") {
            if line.is_empty() {
                break;
            }
            let name = line.split(':').next().unwrap_or("");
            if !name.eq_ignore_ascii_case("proxy-authorization")
                && !name.eq_ignore_ascii_case("proxy-connection")
            {
                write!(remote, "{line}\r\n")?;
            }
        }
        remote.write_all(b"\r\n")?;
    }
    let mut writer = remote.try_clone()?;
    std::thread::spawn(move || {
        let _ = io::copy(&mut input, &mut writer);
        let _ = writer.shutdown(std::net::Shutdown::Write);
    });
    let mut buffer = [0u8; 16384];
    while let Ok(count) = remote.read(&mut buffer) {
        if count == 0
            || output
                .write_all(&buffer[..count])
                .and_then(|_| output.flush())
                .is_err()
        {
            break;
        }
    }
    let _ = remote.shutdown(std::net::Shutdown::Both);
    Ok(())
}

pub fn run() -> i32 {
    let upstream =
        std::env::var("VMCTL_RELAY_UPSTREAM").unwrap_or_else(|_| "127.0.0.1:18080".into());
    let result = upstream
        .parse::<SocketAddr>()
        .map_err(|_| io::Error::other("invalid upstream"))
        .and_then(relay);
    if result.is_err() {
        let _ = io::stdout().write_all(
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return 1;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_private_and_special_addresses() {
        for address in [
            "127.0.0.1",
            "10.0.2.2",
            "169.254.169.254",
            "192.168.1.1",
            "100.64.1.1",
            "224.0.0.1",
        ] {
            assert!(!public_ip(address.parse().unwrap()), "{address}");
        }
        assert!(public_ip("1.1.1.1".parse().unwrap()));
    }
    #[test]
    fn parses_dns_and_never_accepts_private_answers() {
        let mut packet = dns_query("example.com").unwrap();
        packet[2] = 0x81;
        packet[3] = 0x80;
        packet[7] = 1;
        packet.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 127, 0, 0, 1]);
        assert!(parse_dns(&packet).is_err());
        let n = packet.len();
        packet[n - 4..].copy_from_slice(&[1, 1, 1, 1]);
        assert_eq!(parse_dns(&packet).unwrap(), Ipv4Addr::new(1, 1, 1, 1));
    }
}
