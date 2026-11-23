//! Host network interfaces usable as an explicitly selected IPv4 egress.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;

use netdev::Interface;
use netdev::interface::types::InterfaceType;
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

/// The name interfaces are selected by: the adapter's friendly name on
/// Windows (the numeric device name is a GUID), the BSD/Linux name elsewhere.
fn key(interface: &Interface) -> String {
    if cfg!(windows) {
        interface.friendly_name.clone().unwrap_or_else(|| interface.name.clone())
    } else {
        interface.name.clone()
    }
}

fn kind(interface: &Interface) -> &'static str {
    let name = interface.name.to_ascii_lowercase();
    let label = interface.friendly_name.as_deref().unwrap_or_default().to_ascii_lowercase();
    // Friendly names are localized, so the numeric type decides where it can.
    let tunnel = matches!(
        interface.if_type,
        InterfaceType::Tunnel | InterfaceType::Ppp | InterfaceType::ProprietaryVirtual
    );
    if tunnel
        || ["utun", "tun", "ppp", "ipsec", "wg"].iter().any(|prefix| name.starts_with(prefix))
        || ["tap", "wireguard", "vpn", "tunnel"].iter().any(|word| label.contains(word))
    {
        return "vpn";
    }
    if !label.is_empty() {
        if interface.if_type == InterfaceType::Wireless80211 {
            return "wifi";
        }
        if label.contains("ethernet") || label.contains("lan") {
            return "ethernet";
        }
        return "hardware";
    }
    // Ethernet-class device names remain identifiable without a port label.
    if ["eth", "en", "wl"].iter().any(|prefix| name.starts_with(prefix)) {
        "hardware"
    } else {
        "other"
    }
}

/// One non-loopback interface's state, merged by selection name.
struct Link {
    index: u32,
    usable_link: bool,
    ipv4: Vec<(Ipv4Addr, Option<Ipv4Addr>)>,
    interface: Interface,
}

impl Link {
    fn usable(&self) -> bool {
        self.usable_link && self.ipv4.iter().any(|(ip, _)| !ip.is_link_local())
    }

    fn label(&self, name: &str) -> String {
        match self.interface.friendly_name.as_deref() {
            Some(label) if label != name => format!("{label} ({name})"),
            _ => name.to_string(),
        }
    }
}

fn links() -> BTreeMap<String, Link> {
    let mut found = BTreeMap::new();
    for interface in netdev::get_interfaces().into_iter().filter(|item| !item.is_loopback()) {
        let mut ipv4: Vec<_> = interface
            .ipv4
            .iter()
            .map(|net| (net.addr(), Some(net.netmask())))
            .filter(|(ip, _)| !ip.is_unspecified() && !ip.is_loopback())
            .collect();
        ipv4.sort_unstable();
        ipv4.dedup();
        found.insert(
            key(&interface),
            Link {
                index: interface.index,
                usable_link: interface.is_up() && interface.is_running(),
                ipv4,
                interface,
            },
        );
    }
    found
}

/// Enumerate interfaces with their display labels and current IPv4 addresses.
pub fn list() -> Result<Vec<NetworkInterface>> {
    Ok(links()
        .into_iter()
        .map(|(name, link)| NetworkInterface {
            kind: kind(&link.interface).into(),
            label: link.label(&name),
            available: link.usable(),
            ipv4: link.ipv4.iter().map(|(ip, _)| ip.to_string()).collect(),
            name,
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
    Ok(links()
        .into_iter()
        .filter(|(_, link)| link.usable())
        .map(|(name, mut link)| {
            link.ipv4.retain(|(ip, _)| !ip.is_link_local());
            (name, InterfacePath { index: link.index, ipv4: link.ipv4 })
        })
        .collect())
}

/// Refuse a missing or inactive interface instead of falling back to the OS route.
pub fn require_available(name: &str) -> Result<()> {
    if name.is_empty() || name.trim() != name || name.as_bytes().contains(&0) {
        return Err(Error::Message("Invalid network interface name".into()));
    }
    if !links().get(name).is_some_and(Link::usable) {
        return Err(Error::Message(format!(
            "Network interface {name} is unavailable or has no usable IPv4 address"
        )));
    }
    Ok(())
}

/// The first IPv4 address of the named interface when it is usable, otherwise
/// of the interface the OS routes through.
pub fn host_ip(name: &str) -> Option<Ipv4Addr> {
    let links = links();
    let named = links.get(name).filter(|link| link.usable());
    let routed = || {
        let default = netdev::get_default_interface().ok()?;
        links.get(&key(&default)).filter(|link| link.usable())
    };
    named.or_else(routed)?.ipv4.iter().map(|(ip, _)| *ip).find(|ip| !ip.is_link_local())
}

/// Whether an address belongs to one of this host's interfaces.
pub fn is_host_address(ip: Ipv4Addr) -> bool {
    netdev::get_interfaces().iter().any(|item| item.ipv4.iter().any(|net| net.addr() == ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interface(name: &str, label: Option<&str>, if_type: InterfaceType) -> Interface {
        let mut interface = Interface::dummy();
        interface.name = name.into();
        interface.friendly_name = label.map(Into::into);
        interface.if_type = if_type;
        interface
    }

    #[test]
    fn kinds_come_from_type_label_and_name() {
        let kind = |name, label, ty| super::kind(&interface(name, label, ty));
        assert_eq!(kind("en0", Some("Ethernet"), InterfaceType::Ethernet), "ethernet");
        assert_eq!(kind("en8", Some("Wi-Fi"), InterfaceType::Wireless80211), "wifi");
        assert_eq!(kind("en9", Some("USB adapter"), InterfaceType::Ethernet), "hardware");
        assert_eq!(kind("en9", None, InterfaceType::Ethernet), "hardware");
        assert_eq!(kind("utun3", None, InterfaceType::Tunnel), "vpn");
        assert_eq!(kind("{guid}", Some("WireGuard Tunnel"), InterfaceType::Ethernet), "vpn");
        assert_eq!(kind("{guid}", Some("Dial-up"), InterfaceType::Ppp), "vpn");
        assert_eq!(kind("awdl0", None, InterfaceType::PeerToPeerWireless), "other");
    }

    #[test]
    fn missing_interface_fails_closed() {
        assert!(require_available("cabin-nonexistent-interface").is_err());
        assert!(require_available("").is_err());
        assert!(list().is_ok());
    }
}
