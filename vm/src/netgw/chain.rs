//! Where guest traffic goes: the upstream and the shoes configuration for it.

use super::MTU;
use super::frame::BLOCKED;
use crate::{Error, Result};

/// Where the VM's internet traffic goes.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Upstream {
    /// Straight out from the Mac (still isolated from LAN and the Mac).
    Direct,
    /// Through shoes client hops (subscription nodes), entry first; each hop
    /// is reached through the ones before it.
    Chain(Vec<serde_yaml::Value>),
    /// Direct traffic bound to a host interface.
    BoundDirect(String),
    /// A proxy chain whose first hop is bound to a host interface.
    BoundChain(Vec<serde_yaml::Value>, String),
}

impl Upstream {
    pub(super) fn interface(&self) -> Option<&str> {
        match self {
            Self::BoundDirect(name) | Self::BoundChain(_, name) => Some(name),
            _ => None,
        }
    }
}

/// The shoes `client_chain` for `upstream`: direct, one hop, or a sequence.
pub(super) fn client_chain(upstream: &Upstream) -> Result<serde_yaml::Value> {
    if let Some(name) = upstream.interface() {
        crate::network_interfaces::require_available(name)?;
    }
    client_chain_value(upstream)
}

fn client_chain_value(upstream: &Upstream) -> Result<serde_yaml::Value> {
    use serde_yaml::{Mapping, Value};
    let string = |s: &str| Value::String(s.to_string());
    let (hops, interface) = match upstream {
        Upstream::Direct => (None, None),
        Upstream::BoundDirect(name) => (None, Some(name)),
        Upstream::Chain(hops) => (Some(hops), None),
        Upstream::BoundChain(hops, name) => (Some(hops), Some(name)),
    };
    let mut clean = if let Some(hops) = hops {
        hops.iter()
            .map(|hop| {
                let mut hop = hop
                    .as_mapping()
                    .cloned()
                    .ok_or_else(|| Error::Message("invalid proxy node".into()))?;
                hop.remove(string("name"));
                hop.remove(string("bind_interface"));
                Ok(hop)
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        let mut protocol = Mapping::new();
        protocol.insert(string("type"), string("direct"));
        let mut hop = Mapping::new();
        hop.insert(string("protocol"), Value::Mapping(protocol));
        vec![hop]
    };
    if clean.is_empty() {
        return Err(Error::Message("the route has no nodes".into()));
    }
    if let Some(name) = interface {
        clean[0].insert(string("bind_interface"), string(name));
    }
    let mut clean: Vec<_> = clean.into_iter().map(Value::Mapping).collect();
    if clean.len() == 1 {
        Ok(clean.remove(0))
    } else {
        Ok(Value::Sequence(clean))
    }
}

/// The shoes TUN config for `upstream`. The packet device is a socket passed
/// to the stack, so `device_fd` is only the schema tag that marks a TUN config.
pub fn shoes_config(upstream: &Upstream) -> Result<String> {
    shoes_config_blocking(upstream, &BLOCKED)
}

pub(super) fn shoes_config_blocking(upstream: &Upstream, blocked_masks: &[&str]) -> Result<String> {
    use serde_yaml::{Mapping, Value};
    let string = |s: &str| Value::String(s.to_string());
    let mut block = Mapping::new();
    block.insert(
        string("masks"),
        Value::Sequence(blocked_masks.iter().map(|m| string(m)).collect()),
    );
    block.insert(string("action"), string("block"));
    let chain = client_chain(upstream)?;
    let mut allow = Mapping::new();
    allow.insert(string("masks"), Value::Sequence(vec![string("0.0.0.0/0")]));
    allow.insert(string("action"), string("allow"));
    allow.insert(string("client_chain"), chain);
    let mut rules = Vec::new();
    if !blocked_masks.is_empty() {
        rules.push(Value::Mapping(block));
    }
    rules.push(Value::Mapping(allow));
    let mut tun = Mapping::new();
    tun.insert(string("device_fd"), Value::Number(0.into()));
    tun.insert(string("packet_information"), Value::Bool(false));
    tun.insert(string("mtu"), Value::Number((MTU as u64).into()));
    tun.insert(string("tcp_enabled"), Value::Bool(true));
    tun.insert(string("udp_enabled"), Value::Bool(true));
    tun.insert(string("icmp_enabled"), Value::Bool(false));
    tun.insert(string("rules"), Value::Sequence(rules));
    serde_yaml::to_string(&Value::Sequence(vec![Value::Mapping(tun)]))
        .map_err(|error| Error::Message(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_upstreams_only_set_the_interface_on_the_first_hop() {
        let direct = client_chain_value(&Upstream::BoundDirect("en8".into())).unwrap();
        assert_eq!(direct["bind_interface"].as_str(), Some("en8"));
        let hop: serde_yaml::Value =
            serde_yaml::from_str("name: proxy\nbind_interface: stale\nprotocol: {type: socks}\n")
                .unwrap();
        let chain = client_chain_value(&Upstream::BoundChain(vec![hop.clone(), hop], "en8".into()))
            .unwrap();
        let hops = chain.as_sequence().unwrap();
        assert_eq!(hops[0]["bind_interface"].as_str(), Some("en8"));
        assert!(hops[1].get("bind_interface").is_none());
        assert!(
            client_chain(&Upstream::BoundDirect("cabin-nonexistent-interface".into())).is_err()
        );
    }

    #[test]
    fn shoes_config_blocks_private_ranges_first() {
        let yaml = shoes_config(&Upstream::Direct).unwrap();
        let block = yaml.find("action: block").unwrap();
        let allow = yaml.find("action: allow").unwrap();
        assert!(block < allow, "{yaml}");
        assert!(
            yaml.contains("device_fd: 0") && yaml.contains("packet_information: false"),
            "{yaml}"
        );
        assert!(yaml.contains("192.168.0.0/16"));
    }

    #[test]
    fn a_chain_goes_out_through_its_hops_in_order() {
        let hop = |name: &str, address: &str| {
            serde_yaml::from_str::<serde_yaml::Value>(&format!(
                "{{name: {name}, address: \"{address}\", protocol: {{type: socks}}}}"
            ))
            .unwrap()
        };
        let upstream = Upstream::Chain(vec![
            hop("entry", "198.51.100.1:1080"),
            hop("exit", "203.0.113.1:1080"),
        ]);
        let yaml = shoes_config(&upstream).unwrap();
        let entry = yaml.find("198.51.100.1:1080").unwrap();
        let exit = yaml.find("203.0.113.1:1080").unwrap();
        assert!(entry < exit, "{yaml}");
        assert!(!yaml.contains("name: entry"), "{yaml}");
        assert!(shoes_config(&Upstream::Chain(Vec::new())).is_err());
    }
}
