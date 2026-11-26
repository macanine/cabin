//! Which Mac interface carries internet traffic on the system route, and
//! whether that is a tunnel: a VPN, or a TUN proxy such as Clash/mihomo,
//! sing-box or Surge that captures the default route with split routes
//! (`1/8`, `2/7`, …) and often answers DNS with fake IPs in 198.18.0.0/15.
//!
//! The network page uses this to say plainly where "System route" goes and to
//! offer binding the physical adapter, which bypasses the tunnel.

use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub(crate) struct SystemRoute {
    /// The interface a public destination leaves through (e.g. `utun1024`).
    pub internet_interface: Option<String>,
    /// The interface of the `default` route (the physical adapter, e.g. `en0`).
    pub default_interface: Option<String>,
    /// Internet traffic is captured by a tunnel instead of the physical adapter.
    pub tunnel: bool,
    /// The tunnel routes through a fake-IP gateway (198.18.0.0/15), typical
    /// of Clash/mihomo and sing-box TUN modes.
    pub fake_ip: bool,
}

/// `gateway:` and `interface:` from `route -n get <destination>`.
#[cfg(any(target_os = "macos", test))]
fn parse_route_get(output: &str) -> (Option<String>, Option<std::net::Ipv4Addr>) {
    let field = |name: &str| {
        output.lines().find_map(|line| {
            let (key, value) = line.trim().split_once(':')?;
            (key.trim() == name)
                .then(|| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
    };
    (
        field("interface"),
        field("gateway").and_then(|value| value.parse().ok()),
    )
}

#[cfg(any(target_os = "macos", test))]
fn is_tunnel_name(name: &str) -> bool {
    ["utun", "ipsec", "ppp", "tun", "tap", "wg", "gif"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

#[cfg(any(target_os = "macos", test))]
fn classify(internet: &str, default: &str) -> SystemRoute {
    let (internet_interface, gateway) = parse_route_get(internet);
    let (default_interface, _) = parse_route_get(default);
    let tunnel = internet_interface.as_deref().is_some_and(|name| {
        is_tunnel_name(name)
            || default_interface
                .as_deref()
                .is_some_and(|default| default != name)
    });
    let fake_ip =
        tunnel && gateway.is_some_and(|ip| ip.octets()[0] == 198 && (ip.octets()[1] & 0xfe) == 18);
    SystemRoute {
        internet_interface,
        default_interface,
        tunnel,
        fake_ip,
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn detect() -> SystemRoute {
    let route = |destination: &str| {
        std::process::Command::new("/sbin/route")
            .args(["-n", "get", destination])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    };
    // A public address the gateway's own checks also use.
    classify(&route("1.1.1.1"), &route("default"))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn detect() -> SystemRoute {
    SystemRoute::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASH: &str = "   route to: one.one.one.one\ndestination: 1.0.0.0\n       mask: 255.0.0.0\n    gateway: 198.18.0.1\n  interface: utun1024\n      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING>\n";
    const WIFI: &str = "   route to: default\ndestination: default\n       mask: default\n    gateway: 10.196.0.1\n  interface: en0\n";

    #[test]
    fn a_fake_ip_tun_and_a_plain_adapter_are_told_apart() {
        let clash = classify(CLASH, WIFI);
        assert_eq!(clash.internet_interface.as_deref(), Some("utun1024"));
        assert_eq!(clash.default_interface.as_deref(), Some("en0"));
        assert!(clash.tunnel && clash.fake_ip);

        let plain = classify(WIFI, WIFI);
        assert!(!plain.tunnel && !plain.fake_ip);

        let vpn = classify(&CLASH.replace("198.18.0.1", "10.8.0.1"), WIFI);
        assert!(vpn.tunnel && !vpn.fake_ip);

        assert_eq!(classify("", ""), SystemRoute::default());
    }
}
