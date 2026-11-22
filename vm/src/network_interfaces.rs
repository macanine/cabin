//! Host network interfaces usable as an explicitly selected IPv4 egress.

use std::collections::BTreeMap;
#[cfg(unix)]
use std::ffi::CStr;
use std::net::Ipv4Addr;

use serde::Serialize;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NetworkInterface {
    pub name: String,
    pub label: String,
    pub kind: String,
    pub available: bool,
    pub ipv4: Vec<String>,
}

/// Parse `networksetup -listallhardwareports`, whose names, not BSD device
/// prefixes, identify Wi-Fi, Ethernet, and other physical ports on macOS.
#[cfg(any(target_os = "macos", test))]
fn hardware_ports(text: &str) -> BTreeMap<String, String> {
    let mut ports = BTreeMap::new();
    let mut label = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Hardware Port: ") {
            label = Some(value.trim().to_string());
        } else if let Some(name) = line.strip_prefix("Device: ")
            && let Some(label) = label.take()
        {
            ports.insert(name.trim().to_string(), label);
        }
    }
    ports
}

#[cfg(windows)]
const IFF_UP: u32 = 0x1;
#[cfg(windows)]
const IFF_RUNNING: u32 = 0x40;
#[cfg(windows)]
const IF_TYPE_IEEE80211: u32 = 71;
#[cfg(windows)]
const IF_TYPE_PPP: u32 = 23;
#[cfg(windows)]
const IF_TYPE_TUNNEL: u32 = 53;
#[cfg(windows)]
const IF_TYPE_PROPVIRTUAL: u32 = 131;

/// Windows interface kind from the adapter type plus tunnel-ish names
/// (friendly names are localized, so the numeric type decides).
#[cfg(windows)]
fn windows_kind(if_type: u32, name: &str) -> &'static str {
    let lowered = name.to_ascii_lowercase();
    if lowered.contains("tap")
        || lowered.contains("wireguard")
        || lowered.contains("vpn")
        || lowered.contains("tunnel")
        || if_type == IF_TYPE_PPP
        || if_type == IF_TYPE_TUNNEL
        || if_type == IF_TYPE_PROPVIRTUAL
    {
        return "vpn";
    }
    match if_type {
        IF_TYPE_IEEE80211 => "wifi",
        6 => "ethernet",
        _ => "other",
    }
}

#[cfg(not(windows))]
fn kind(name: &str, hardware: Option<&str>) -> &'static str {
    if let Some(label) = hardware {
        let label = label.to_ascii_lowercase();
        if label.contains("wi-fi") || label.contains("wifi") || label.contains("airport") {
            return "wifi";
        }
        if label.contains("ethernet") || label.contains("lan") {
            return "ethernet";
        }
        return "hardware";
    }
    // BSD Ethernet-class devices remain identifiable without a port label.
    #[cfg(target_os = "macos")]
    if name.strip_prefix("en").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        return "hardware";
    }
    #[cfg(target_os = "linux")]
    if ["eth", "en", "wl"].iter().any(|prefix| name.starts_with(prefix)) {
        return "hardware";
    }
    if name.starts_with("utun")
        || name.starts_with("tun")
        || name.starts_with("ppp")
        || name.starts_with("ipsec")
        || name.starts_with("wg")
    {
        "vpn"
    } else {
        "other"
    }
}

#[derive(Default)]
struct InterfaceAddresses {
    flags: u32,
    ipv4: Vec<(Ipv4Addr, Option<Ipv4Addr>)>,
}

impl InterfaceAddresses {
    fn usable(&self) -> bool {
        #[cfg(unix)]
        let required = (libc::IFF_UP | libc::IFF_RUNNING) as u32;
        #[cfg(windows)]
        let required = IFF_UP | IFF_RUNNING;
        self.flags & required == required && self.ipv4.iter().any(|(ip, _)| !ip.is_link_local())
    }
}

#[cfg(unix)]
fn ipv4(address: *const libc::sockaddr) -> Option<Ipv4Addr> {
    if address.is_null() || unsafe { (*address).sa_family as i32 } != libc::AF_INET {
        return None;
    }
    let address = unsafe { &*address.cast::<libc::sockaddr_in>() };
    Some(Ipv4Addr::from(address.sin_addr.s_addr.to_ne_bytes()))
}

#[cfg(unix)]
fn addresses() -> Result<BTreeMap<String, InterfaceAddresses>> {
    let mut raw = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut raw) } != 0 {
        return Err(Error::Message(format!(
            "Cannot enumerate network interfaces: {}",
            std::io::Error::last_os_error()
        )));
    }
    struct Addresses(*mut libc::ifaddrs);
    impl Drop for Addresses {
        fn drop(&mut self) {
            unsafe { libc::freeifaddrs(self.0) }
        }
    }
    let addresses = Addresses(raw);
    let mut found: BTreeMap<String, InterfaceAddresses> = BTreeMap::new();
    let mut current = addresses.0;
    while !current.is_null() {
        let entry = unsafe { &*current };
        if !entry.ifa_name.is_null() && entry.ifa_flags & libc::IFF_LOOPBACK as u32 == 0 {
            let name = unsafe { CStr::from_ptr(entry.ifa_name) }
                .to_string_lossy()
                .into_owned();
            let item = found.entry(name).or_default();
            item.flags |= entry.ifa_flags;
            if let Some(ip) = ipv4(entry.ifa_addr)
                && !ip.is_unspecified() && !ip.is_loopback()
            {
                item.ipv4.push((ip, ipv4(entry.ifa_netmask)));
            }
        }
        current = entry.ifa_next;
    }
    Ok(found)
}

/// Windows: adapters from `GetAdaptersAddresses` (via `compat`), keyed by
/// their localized friendly name, up and running only.
#[cfg(windows)]
fn addresses() -> Result<BTreeMap<String, InterfaceAddresses>> {
    let adapters = crate::compat::adapters().map_err(|error| {
        Error::Message(format!("Cannot enumerate network interfaces: {error}"))
    })?;
    let mut found: BTreeMap<String, InterfaceAddresses> = BTreeMap::new();
    for adapter in adapters {
        let item = found.entry(adapter.label.clone()).or_default();
        if adapter.oper_up {
            item.flags |= IFF_UP | IFF_RUNNING;
        }
        item.ipv4.extend(adapter.ipv4);
        item.ipv4.sort_unstable();
        item.ipv4.dedup();
    }
    Ok(found)
}

/// Enumerate interfaces with their display labels and current IPv4 addresses.
pub fn list() -> Result<Vec<NetworkInterface>> {
    #[cfg(target_os = "macos")]
    let ports = std::process::Command::new("/usr/sbin/networksetup")
        .arg("-listallhardwareports")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| hardware_ports(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default();
    #[cfg(not(target_os = "macos"))]
    let ports: BTreeMap<String, String> = BTreeMap::new();

    let mut found = addresses()?;
    for name in ports.keys() {
        found.entry(name.clone()).or_default();
    }
    Ok(found
        .into_iter()
        .map(|(name, item)| {
            let hardware = ports.get(&name);
            let label = hardware
                .map(|port| format!("{port} ({name})"))
                .unwrap_or_else(|| name.clone());
            let available = item.usable();
            let mut ips: Vec<_> = item.ipv4.into_iter().map(|(ip, _)| ip).collect();
            ips.sort_unstable();
            ips.dedup();
            NetworkInterface {
                #[cfg(windows)]
                kind: windows_kind(
                    crate::compat::adapters()
                        .unwrap_or_default()
                        .into_iter()
                        .find(|adapter| adapter.label == name)
                        .map(|adapter| adapter.if_type)
                        .unwrap_or(0),
                    &name,
                )
                .into(),
                #[cfg(not(windows))]
                kind: kind(&name, hardware.map(String::as_str)).into(),
                name,
                label,
                available,
                ipv4: ips.into_iter().map(|ip| ip.to_string()).collect(),
            }
        })
        .collect())
}

#[derive(Debug, PartialEq, Eq)]
pub struct InterfacePath {
    pub index: u32,
    pub ipv4: Vec<(Ipv4Addr, Option<Ipv4Addr>)>,
}

/// A snapshot of usable links for detecting changes on adapters outside the default path.
pub fn paths() -> Result<BTreeMap<String, InterfacePath>> {
    #[cfg(unix)]
    {
        let mut paths = BTreeMap::new();
        for (name, mut item) in addresses()?.into_iter().filter(|(_, item)| item.usable()) {
            let Ok(c_name) = std::ffi::CString::new(name.as_str()) else { continue };
            let index = unsafe { libc::if_nametoindex(c_name.as_ptr()) };
            if index == 0 {
                continue;
            }
            item.ipv4.retain(|(ip, _)| !ip.is_link_local());
            item.ipv4.sort_unstable();
            item.ipv4.dedup();
            paths.insert(name, InterfacePath { index, ipv4: item.ipv4 });
        }
        Ok(paths)
    }
    #[cfg(windows)]
    {
        let mut paths = BTreeMap::new();
        for adapter in crate::compat::adapters().unwrap_or_default() {
            if !adapter.oper_up {
                continue;
            }
            let mut ipv4: Vec<_> = adapter
                .ipv4
                .into_iter()
                .filter(|(ip, _)| !ip.is_link_local())
                .collect();
            ipv4.sort_unstable();
            ipv4.dedup();
            if ipv4.is_empty() {
                continue;
            }
            paths.insert(
                adapter.label.clone(),
                InterfacePath {
                    index: adapter.if_index,
                    ipv4,
                },
            );
        }
        Ok(paths)
    }
}

/// Refuse a missing or inactive interface instead of falling back to the OS route.
pub fn require_available(name: &str) -> Result<()> {
    if name.is_empty() || name.trim() != name || name.as_bytes().contains(&0) {
        return Err(Error::Message("Invalid network interface name".into()));
    }
    if !addresses()?.get(name).is_some_and(InterfaceAddresses::usable) {
        return Err(Error::Message(format!(
            "Network interface {name} is unavailable or has no usable IPv4 address"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn hardware_labels_classify_ports_without_guessing_en_numbers() {
        let ports = hardware_ports(
            "Hardware Port: Ethernet\nDevice: en0\nEthernet Address: aa\n\nHardware Port: Wi-Fi\nDevice: en8\n",
        );
        assert_eq!(
            kind("en0", ports.get("en0").map(String::as_str)),
            "ethernet"
        );
        assert_eq!(kind("en8", ports.get("en8").map(String::as_str)), "wifi");
        assert_eq!(kind("en9", Some("USB adapter")), "hardware");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert_eq!(kind("en9", None), "hardware");
        assert_eq!(kind("utun3", None), "vpn");
    }

    #[cfg(windows)]
    #[test]
    fn windows_kinds_classify_by_adapter_type() {
        assert_eq!(windows_kind(6, "Ethernet"), "ethernet");
        assert_eq!(windows_kind(71, "WLAN"), "wifi");
        assert_eq!(windows_kind(23, "Something"), "vpn");
        assert_eq!(windows_kind(6, "WireGuard Tunnel"), "vpn");
        assert_eq!(windows_kind(1, "Other"), "other");
    }

    #[test]
    fn missing_interface_fails_closed() {
        assert!(require_available("cabin-nonexistent-interface").is_err());
        assert!(require_available("").is_err());
        assert!(list().is_ok());
    }
}
