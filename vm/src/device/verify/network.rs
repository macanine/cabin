//! Network checks: MACs, driver, addresses, routes, DNS, ARP, leases and IPv6.

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::py_json;

use super::shared::{Context, file_text, repr_string, truncate_chars};

/// Values the summary needs from this section.
pub(super) struct Output {
    pub(super) macs: Map<String, Value>,
    pub(super) addresses: Vec<(String, String)>,
    pub(super) gateways: Vec<String>,
    pub(super) dns: Vec<String>,
}

pub(super) fn run(ctx: &mut Context<'_>) -> Output {
    let raw = ctx.raw;
    let network = ctx.network;
    let cfg = ctx.cfg;
    let allow_user_network = ctx.allow_user_network;
    let report = &mut ctx.report;

    // -------------------------------------------------------------- NETWORK
    let macs = iface_macs(raw);
    let expected_mac = network
        .get("mac")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let bad_macs: Map<String, Value> = macs
        .iter()
        .filter(|(_, mac)| {
            let text = mac.as_str().unwrap_or("");
            text.starts_with("52:54:00") || text.starts_with("52:55:0a")
        })
        .map(|(name, mac)| (name.clone(), mac.clone()))
        .collect();
    // Gateway mode intentionally has a restricted slirp management NIC and a
    // gateway NIC with a QEMU OUI. Keep these fingerprints visible as warnings.
    let lab_severity = if allow_user_network || cfg.isolated() {
        "warning"
    } else {
        "error"
    };
    if !bad_macs.is_empty() {
        report.add(
            "network",
            "network.mac",
            lab_severity,
            "A QEMU default MAC prefix is visible",
            "52:54:00 is the QEMU OUI and 52:55:0a is the slirp gateway OUI.",
            Some(&json!(bad_macs)),
            "Set device.network.mac_oui to a real NIC vendor OUI.",
            false,
        );
    } else if !expected_mac.is_empty()
        && !macs
            .values()
            .any(|mac| mac.as_str() == Some(expected_mac.as_str()))
    {
        report.add(
            "network",
            "network.mac",
            "warning",
            "Expected MAC is not present",
            &format!(
                "expected {expected_mac}; observed {}",
                py_json::py_repr(&json!(macs))
            ),
            Some(&json!(macs)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.mac",
            "info",
            "NIC MAC uses a real vendor OUI",
            &py_json::dumps_default(&json!(macs), true),
            Some(&json!(macs)),
            "",
            false,
        );
    }

    let interface = network
        .get("interface")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut ethtool_driver = String::new();
    let names: Vec<&str> = (if interface.is_empty() {
        Vec::new()
    } else {
        vec![interface]
    })
    .into_iter()
    .chain(macs.keys().map(|name| name.as_str()))
    .collect();
    let driver_re = Regex::new(r"(?m)^driver:\s*(\S+)").ok();
    for name in names {
        let data = raw
            .get("net")
            .and_then(|value| value.get("ethtool"))
            .and_then(|value| value.get(name))
            .unwrap_or(&Value::Null);
        let text = data.get("out").and_then(Value::as_str).unwrap_or("");
        if let Some(captures) = driver_re
            .as_ref()
            .and_then(|pattern| pattern.captures(text))
        {
            ethtool_driver = captures[1].to_string();
            break;
        }
    }
    if ethtool_driver == "virtio_net" {
        report.add(
            "network",
            "network.driver",
            "error",
            "NIC driver is virtio_net",
            "ethtool reports virtio_net",
            Some(&json!(ethtool_driver)),
            "",
            false,
        );
    } else if !ethtool_driver.is_empty() {
        report.add(
            "network",
            "network.driver",
            "info",
            "NIC driver is a real model",
            &format!("driver: {ethtool_driver}"),
            Some(&json!(ethtool_driver)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.driver",
            "warning",
            "Cannot read the NIC driver",
            "ethtool -i did not report a driver",
            Some(
                raw.get("net")
                    .and_then(|value| value.get("ethtool"))
                    .unwrap_or(&Value::Null),
            ),
            "",
            false,
        );
    }

    let addresses = iface_addresses(raw, "inet");
    let qemu_ips: Vec<(String, String)> = addresses
        .iter()
        .filter(|(_, address)| address.starts_with("10.0.2."))
        .cloned()
        .collect();
    if !qemu_ips.is_empty() {
        report.add(
            "network",
            "network.slirp_ip",
            lab_severity,
            "Slirp 10.0.2.0/24 address is visible",
            "QEMU user networking hands out 10.0.2.15/24 with gateway 10.0.2.2.",
            Some(&json!(qemu_ips)),
            "Use --network socket-vmnet with a bridged socket_vmnet daemon and a static LAN address, or a vmnet bridge.",
            false,
        );
    } else {
        report.add(
            "network",
            "network.slirp_ip",
            "info",
            "No slirp 10.0.2.x address",
            &py_json::dumps_default(&json!(addresses), false),
            Some(&json!(addresses)),
            "",
            false,
        );
    }
    if let Some(expected_address) = network
        .get("address")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let expected_ip = expected_address.split('/').next().unwrap_or("").to_string();
        if !addresses.iter().any(|(_, address)| address == &expected_ip) {
            report.add(
                "network",
                "network.static_ip",
                "error",
                "Configured static address is not present",
                &format!(
                    "expected {expected_ip}; observed {}",
                    repr_tuples(&addresses)
                ),
                Some(&json!(addresses)),
                "",
                false,
            );
        } else {
            report.add(
                "network",
                "network.static_ip",
                "info",
                "Configured static address is active",
                &expected_ip,
                None,
                "",
                false,
            );
        }
    }

    let gateways = default_gateways(raw);
    let bad_gateways: Vec<String> = gateways
        .iter()
        .filter(|gateway| gateway.starts_with("10.0.2.") || gateway.starts_with("192.168.105."))
        .cloned()
        .collect();
    if !bad_gateways.is_empty() {
        report.add(
            "network",
            "network.gateway",
            lab_severity,
            "QEMU/vmnet gateway is visible",
            &format!("default gateways: {}", py_json::py_repr(&json!(gateways))),
            Some(&json!(bad_gateways)),
            "Bridge the guest onto the physical LAN so the real router is the gateway.",
            false,
        );
    } else if !gateways.is_empty() {
        report.add(
            "network",
            "network.gateway",
            "info",
            "Default gateway is not a QEMU signature",
            &py_json::dumps_default(&json!(gateways), false),
            Some(&json!(gateways)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.gateway",
            "warning",
            "No default gateway",
            "ip route has no default route",
            None,
            "",
            false,
        );
    }

    let dns = dns_servers(raw);
    let bad_dns: Vec<String> = dns
        .iter()
        .filter(|server| server.starts_with("10.0.2.3") || server.starts_with("192.168.105.1"))
        .cloned()
        .collect();
    if !bad_dns.is_empty() {
        report.add(
            "network",
            "network.dns",
            lab_severity,
            "QEMU/vmnet DNS server is visible",
            &format!("nameservers: {}", py_json::py_repr(&json!(dns))),
            Some(&json!(bad_dns)),
            "Use the LAN DNS server through a bridged NIC.",
            false,
        );
    } else if !dns.is_empty() {
        report.add(
            "network",
            "network.dns",
            "info",
            "DNS servers are not QEMU signatures",
            &py_json::dumps_default(&json!(dns), false),
            Some(&json!(dns)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.dns",
            "warning",
            "No DNS server configured",
            "resolv.conf is empty",
            None,
            "",
            false,
        );
    }

    let arp = file_text(raw, "/proc/net/arp");
    let mut arp_rows = Vec::new();
    for line in arp.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 {
            arp_rows.push(json!({
                "ip": parts[0],
                "mac": parts[3].to_lowercase(),
                "device": if parts.len() > 5 { parts[5] } else { "" },
            }));
        }
    }
    let bad_arp: Vec<Value> = arp_rows
        .iter()
        .filter(|row| {
            let mac = row.get("mac").and_then(Value::as_str).unwrap_or("");
            mac.starts_with("52:55:0a") || mac.starts_with("52:54:00")
        })
        .cloned()
        .collect();
    if !bad_arp.is_empty() {
        report.add(
            "network",
            "network.arp",
            lab_severity,
            "QEMU gateway MAC is visible in the ARP table",
            "The slirp gateway uses 52:55:0a:00:02:02.",
            Some(&json!(bad_arp)),
            "Bridge the guest onto the physical LAN.",
            false,
        );
    } else {
        report.add(
            "network",
            "network.arp",
            "info",
            "ARP table has no QEMU gateway MAC",
            &py_json::dumps_default(&json!(arp_rows.iter().take(10).collect::<Vec<_>>()), false),
            Some(&json!(arp_rows.iter().take(20).collect::<Vec<_>>())),
            "",
            false,
        );
    }

    let lease_text = raw
        .get("leases")
        .and_then(Value::as_object)
        .map(|leases| {
            leases
                .values()
                .map(|info| {
                    info.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if lease_text.contains("10.0.2.2")
        || lease_text.contains("10.0.2.3")
        || lease_text.contains("10.0.2.15")
    {
        report.add(
            "network",
            "network.dhcp_lease",
            lab_severity,
            "DHCP lease identifies QEMU slirp",
            "A systemd-networkd/DHCP lease contains 10.0.2.2/10.0.2.3/10.0.2.15.",
            Some(&json!(truncate_chars(&lease_text, 2000))),
            "",
            false,
        );
    } else if !lease_text.is_empty() {
        report.add(
            "network",
            "network.dhcp_lease",
            "info",
            "DHCP lease does not identify QEMU",
            &truncate_chars(&lease_text, 1000),
            None,
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.dhcp_lease",
            "info",
            "No DHCP lease on disk (static addressing)",
            "",
            None,
            "",
            false,
        );
    }

    let ipv6 = iface_addresses(raw, "inet6");
    let eui64: Vec<(String, String)> = ipv6
        .iter()
        .filter(|(_, address)| {
            !address.to_lowercase().starts_with("fe80:") && address.to_lowercase().contains("ff:fe")
        })
        .cloned()
        .collect();
    if !eui64.is_empty() {
        report.add(
            "network",
            "network.ipv6_eui64",
            "warning",
            "IPv6 EUI-64 address may leak the MAC",
            "A link-local/global IPv6 address contains the MAC-derived ff:fe IID.",
            Some(&json!(eui64)),
            "netplan ipv6-privacy: true and addr-gen-mode: stable-privacy.",
            false,
        );
    } else {
        report.add(
            "network",
            "network.ipv6_eui64",
            "info",
            "No MAC-derived IPv6 address",
            &py_json::dumps_default(&json!(ipv6.iter().take(8).collect::<Vec<_>>()), false),
            None,
            "",
            false,
        );
    }
    let non_lo: Vec<String> = macs.keys().cloned().collect();
    if non_lo.len() > 1 {
        report.add(
            "network",
            "network.interface_count",
            "warning",
            "Multiple guest NICs are present",
            &format!(
                "interfaces: {}; the management NIC can leak 10.0.2.x",
                py_json::py_repr(&json!(non_lo))
            ),
            Some(&json!(non_lo)),
            "",
            false,
        );
    }

    Output {
        macs,
        addresses,
        gateways,
        dns,
    }
}

fn iface_macs(raw: &Value) -> Map<String, Value> {
    let mut result = Map::new();
    if let Some(items) = raw
        .get("net")
        .and_then(|value| value.get("interfaces"))
        .and_then(Value::as_array)
    {
        for item in items {
            let name = item.get("ifname").and_then(Value::as_str).unwrap_or("");
            let address = item.get("address").and_then(Value::as_str).unwrap_or("");
            if !name.is_empty() && !address.is_empty() && name != "lo" {
                result.insert(name.to_string(), json!(address.to_lowercase()));
            }
        }
    }
    result
}

fn iface_addresses(raw: &Value, family: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    if let Some(items) = raw
        .get("net")
        .and_then(|value| value.get("addresses"))
        .and_then(Value::as_array)
    {
        for item in items {
            let name = item.get("ifname").and_then(Value::as_str).unwrap_or("");
            if let Some(infos) = item.get("addr_info").and_then(Value::as_array) {
                for info in infos {
                    if info.get("family").and_then(Value::as_str) == Some(family)
                        && let Some(local) = info.get("local").and_then(Value::as_str)
                    {
                        result.push((name.to_string(), local.to_string()));
                    }
                }
            }
        }
    }
    result
}

fn routes(raw: &Value) -> Vec<Value> {
    raw.get("net")
        .and_then(|value| value.get("routes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn default_gateways(raw: &Value) -> Vec<String> {
    let mut values = Vec::new();
    for route in routes(raw) {
        let dst = route.get("dst").and_then(Value::as_str).unwrap_or("");
        if (dst == "default"
            || dst == "0.0.0.0/0"
            || route.get("dst").map(Value::is_null).unwrap_or(true))
            && route.get("gateway").is_some_and(|value| !value.is_null())
        {
            values.push(route["gateway"].to_string().trim_matches('"').to_string());
        }
    }
    values
}

pub(super) fn dns_servers(raw: &Value) -> Vec<String> {
    let text = file_text(raw, "/etc/resolv.conf");
    let Ok(re) = Regex::new(r"^\s*nameserver\s+(\S+)") else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for line in text.lines() {
        if let Some(captures) = re.captures(line) {
            values.push(captures[1].to_string());
        }
    }
    values
}

fn repr_tuples(values: &[(String, String)]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|(name, address)| format!("({}, {})", repr_string(name), repr_string(address)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
