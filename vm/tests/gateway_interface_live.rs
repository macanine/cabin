//! Opt-in macOS host gateway test; it does not start or contact a VM.
//! CABIN_TEST_INTERFACE=en0 cargo test --manifest-path vm/Cargo.toml --test gateway_interface_live -- --ignored --nocapture
#![cfg(target_os = "macos")]

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vm_core::netgw::{EgressMode, Gateway, Upstream};

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        // macOS limits Unix socket paths to 103 bytes; TMPDIR can be long.
        let path = PathBuf::from("/tmp").join(format!("vgi-{}-{nonce}", std::process::id()));
        std::fs::create_dir(&path).expect("create isolated gateway directory");
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn client(proxy: Option<&str>) -> ureq::Agent {
    let mut builder = ureq::AgentBuilder::new()
        .resolver(vm_core::ipv4::resolve)
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(Duration::from_secs(8));
    if let Some(url) = proxy {
        builder = builder.proxy(ureq::Proxy::new(url).expect("gateway proxy URL"));
    }
    builder.build()
}

// Parse a DNS name, including compression pointers, without following pointers.
fn skip_name(data: &[u8], pos: &mut usize) -> Option<()> {
    loop {
        let len = *data.get(*pos)?;
        *pos += 1;
        if len == 0 {
            return Some(());
        }
        if len & 0xc0 == 0xc0 {
            let _ = data.get(*pos)?;
            *pos += 1;
            return Some(());
        }
        if len & 0xc0 != 0 || len > 63 {
            return None;
        }
        *pos = pos.checked_add(len as usize)?;
        if *pos > data.len() {
            return None;
        }
    }
}

fn has_example_a(data: &[u8], query: &[u8]) -> bool {
    if data.len() < 12
        || data[..2] != query[..2]
        || data[2] & 0x80 == 0
        || data[3] & 0x0f != 0
        || u16::from_be_bytes([data[4], data[5]]) != 1
    {
        return false;
    }
    let answers = u16::from_be_bytes([data[6], data[7]]);
    let mut pos = 12;
    if skip_name(data, &mut pos).is_none() || data.get(pos..pos + 4) != Some(&[0, 1, 0, 1]) {
        return false;
    }
    pos += 4;
    for _ in 0..answers {
        if skip_name(data, &mut pos).is_none() || pos + 10 > data.len() {
            return false;
        }
        let kind = u16::from_be_bytes([data[pos], data[pos + 1]]);
        let class = u16::from_be_bytes([data[pos + 2], data[pos + 3]]);
        let len = u16::from_be_bytes([data[pos + 8], data[pos + 9]]) as usize;
        pos += 10;
        if pos + len > data.len() {
            return false;
        }
        if kind == 1 && class == 1 && len == 4 {
            return true;
        }
        pos += len;
    }
    false
}

fn check_doh(client: &ureq::Agent, url: &str) -> bool {
    // example.com IN A; the response must contain a valid IPv4 answer.
    let query =
        b"\x4a\x71\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00\x07example\x03com\x00\x00\x01\x00\x01";
    let Ok(response) = client
        .post(url)
        .set("Content-Type", "application/dns-message")
        .set("Accept", "application/dns-message")
        .send_bytes(query)
    else {
        return false;
    };
    let mut data = Vec::new();
    response
        .into_reader()
        .take(65_536)
        .read_to_end(&mut data)
        .is_ok()
        && has_example_a(&data, query)
}

fn detected_ip(client: &ureq::Agent, url: &str) -> Option<IpAddr> {
    let response = client.get(url).call().ok()?;
    let mut body = Vec::new();
    response
        .into_reader()
        .take(1_048_577)
        .read_to_end(&mut body)
        .ok()?;
    if body.len() > 1_048_576 {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&body).ok()?;
    if json.get("error").and_then(|v| v.as_bool()) == Some(true)
        || json.get("success").and_then(|v| v.as_bool()) == Some(false)
    {
        return None;
    }
    let ip: IpAddr = json.get("ip")?.as_str()?.parse().ok()?;
    ip.is_ipv4().then_some(ip)
}

fn route_interface(address: &str) -> Option<String> {
    let output = Command::new("/sbin/route")
        .args(["-n", "get", address])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("interface:")
                .map(|s| s.trim().to_owned())
        })
}

#[test]
#[ignore = "accesses public DNS and IP detection services through a temporary macOS gateway"]
fn bound_interface_gateway_dns_https_and_system_route() {
    let selected = std::env::var("CABIN_TEST_INTERFACE").unwrap_or_else(|_| "en0".into());
    vm_core::network_interfaces::require_available(&selected)
        .expect("test interface must be active with IPv4");
    let temp = TempDir::new();
    let gateway = Gateway::start_closed(
        &temp.0.join("gateway.sock"),
        vec![],
        None,
        (Ipv4Addr::LOCALHOST, 0).into(),
    )
    .expect("start temporary management gateway");
    let ssh = gateway.ssh_addr().unwrap();
    assert!(!gateway.connected(), "test must not connect a VM");
    assert!(gateway.proxy_url().is_none());
    let upstream = Upstream::BoundDirect(selected.clone());
    gateway.set_upstream(&upstream).unwrap();
    gateway.set_egress_mode(EgressMode::Open);
    let proxy = gateway.proxy_url().expect("gateway HTTP relay");
    let bound = client(Some(&proxy));
    let system = client(None);

    // The project gateway's DoH endpoints; Cloudflare is deliberately omitted
    // because it may be unreachable on a selected physical interface.
    let endpoints = [
        (
            "dns.alidns.com",
            "https://dns.alidns.com/dns-query",
            "223.5.5.5",
        ),
        ("doh.pub", "https://doh.pub/dns-query", "1.12.12.12"),
    ];
    let mut working_doh = None;
    for (name, url, route_target) in endpoints {
        let bound_ok = check_doh(&bound, url);
        let system_ok = check_doh(&system, url);
        println!(
            "DoH {name}: {selected} relay={}, system route={}",
            if bound_ok { "ok" } else { "failed" },
            if system_ok { "ok" } else { "failed" }
        );
        if let Some(interface) = route_interface(route_target) {
            println!(
                "System route for {name} reference address uses {selected}: {}",
                interface == selected
            );
        }
        if bound_ok && system_ok {
            working_doh = Some(url);
            break;
        }
    }
    let working_doh = working_doh.expect("no common DoH endpoint returned a valid DNS A answer over both paths");

    // These URLs are the project's ip-detection services. Compare one and the
    // same service on both paths; never print response bodies or IP addresses.
    let services = [
        ("ip.sb", "https://api.ip.sb/geoip"),
        ("ipwho.is", "https://ipwho.is/"),
        ("ipapi.is", "https://api.ipapi.is/"),
        ("ipapi.co", "https://ipapi.co/json"),
        ("geojs.io", "https://get.geojs.io/v1/ip/geo.json"),
        ("skk.moe", "https://ip.api.skk.moe/cf-geoip"),
    ];
    let mut compared = false;
    for (name, url) in services {
        let Some(bound_ip) = detected_ip(&bound, url) else {
            println!("IP detection {name}: {selected} relay unavailable");
            continue;
        };
        let Some(system_ip) = detected_ip(&system, url) else {
            println!("IP detection {name}: system route unavailable");
            continue;
        };
        println!(
            "IP detection {name}: {selected} and system route differ: {}",
            bound_ip != system_ip
        );
        compared = true;
        break;
    }
    assert!(
        compared,
        "no project IP detection service succeeded over both paths"
    );
    assert!(
        gateway.error().is_none(),
        "gateway packet stack reported an error"
    );
    gateway.clear_upstream();
    assert!(gateway.proxy_url().is_none());
    assert_eq!(gateway.ssh_addr(), Some(ssh));
    assert!(std::net::TcpStream::connect(ssh).is_ok());
    gateway.set_upstream(&upstream).unwrap();
    gateway.set_egress_mode(EgressMode::Open);
    let rebuilt = gateway.proxy_url().unwrap();
    assert!(check_doh(&client(Some(&rebuilt)), working_doh));
    assert!(gateway.error().is_none());
    assert_eq!(gateway.ssh_addr(), Some(ssh));
    drop(gateway);
    assert!(
        !temp.0.join("gateway.sock").exists(),
        "gateway socket was not removed"
    );
}
