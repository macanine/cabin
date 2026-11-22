//! Subscription import: Clash-style YAML (optionally base64 encoded) or native
//! shoes hops, converted into shoes client chain hops.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value as Yaml};

use crate::error::{Error, Result};

/// Maximum accepted subscription size.
pub const MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub key: String,
    pub name: String,
    pub protocol: String,
    /// A shoes client hop (`address`, `protocol`, optional `transport`).
    pub config: Yaml,
}

pub(crate) fn key(name: &str) -> Yaml {
    Yaml::String(name.to_string())
}

pub(crate) fn map(pairs: impl IntoIterator<Item = (&'static str, Yaml)>) -> Mapping {
    pairs
        .into_iter()
        .map(|(name, value)| (key(name), value))
        .collect()
}

pub(crate) fn string(value: &str) -> Yaml {
    Yaml::String(value.to_string())
}

fn boolean(value: bool) -> Yaml {
    Yaml::Bool(value)
}

fn yaml_string<'a>(map: &'a Mapping, name: &str) -> Option<&'a str> {
    map.get(key(name)).and_then(Yaml::as_str)
}

fn yaml_bool(map: &Mapping, name: &str) -> Option<bool> {
    map.get(key(name)).and_then(Yaml::as_bool)
}

fn yaml_mapping<'a>(map: &'a Mapping, name: &str) -> Option<&'a Mapping> {
    map.get(key(name)).and_then(Yaml::as_mapping)
}

fn required_string(map: &Mapping, name: &str, label: &str) -> Result<String> {
    yaml_string(map, name)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| Error::Invalid(format!("{label} is missing {name}")))
}

fn endpoint(map: &Mapping, label: &str) -> Result<String> {
    let server = required_string(map, "server", label)?;
    let port = map
        .get(key("port"))
        .and_then(|value| {
            value
                .as_u64()
                .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
        })
        .filter(|value| (1..=u16::MAX as u64).contains(value))
        .ok_or_else(|| Error::Invalid(format!("{label} has an invalid port")))?;
    let host = if server.contains(':') && !server.starts_with('[') {
        format!("[{server}]")
    } else {
        server
    };
    Ok(format!("{host}:{port}"))
}

fn option_string(map: &Mapping, name: &str) -> Option<Yaml> {
    yaml_string(map, name).map(string)
}

fn client_protocol(kind: &str, fields: Mapping) -> Yaml {
    let mut protocol = fields;
    protocol.insert(key("type"), string(kind));
    Yaml::Mapping(protocol)
}

fn nested_protocol(kind: &str, inner: Yaml, extra: Mapping) -> Yaml {
    let mut protocol = extra;
    protocol.insert(key("type"), string(kind));
    protocol.insert(key("protocol"), inner);
    Yaml::Mapping(protocol)
}

fn websocket_wrapper(inner: Yaml, node: &Mapping) -> Yaml {
    let options = yaml_mapping(node, "ws-opts");
    let mut fields = Mapping::new();
    if let Some(path) = options.and_then(|value| yaml_string(value, "path")) {
        fields.insert(key("matching_path"), string(path));
    }
    if let Some(headers) = options.and_then(|value| yaml_mapping(value, "headers")) {
        let values = headers
            .iter()
            .filter_map(|(name, value)| {
                Some((
                    Yaml::String(name.as_str()?.to_string()),
                    Yaml::String(value.as_str()?.to_string()),
                ))
            })
            .collect::<Mapping>();
        if !values.is_empty() {
            fields.insert(key("matching_headers"), Yaml::Mapping(values));
        }
    }
    nested_protocol("websocket", inner, fields)
}

fn default_sni(node: &Mapping, server: &str) -> String {
    yaml_string(node, "servername")
        .or_else(|| yaml_string(node, "sni"))
        .or_else(|| {
            yaml_mapping(node, "ws-opts")
                .and_then(|options| yaml_mapping(options, "headers"))
                .and_then(|headers| {
                    headers.iter().find_map(|(key, value)| {
                        (key.as_str()?.eq_ignore_ascii_case("host"))
                            .then(|| value.as_str())
                            .flatten()
                    })
                })
        })
        .unwrap_or(server)
        .to_string()
}

fn tls_wrapper(inner: Yaml, node: &Mapping, server: &str) -> Yaml {
    let mut fields = Mapping::new();
    fields.insert(key("sni_hostname"), string(&default_sni(node, server)));
    fields.insert(
        key("verify"),
        boolean(!yaml_bool(node, "skip-cert-verify").unwrap_or(false)),
    );
    if let Some(alpn) = node.get(key("alpn")).and_then(Yaml::as_sequence) {
        fields.insert(key("alpn_protocols"), Yaml::Sequence(alpn.clone()));
    }
    nested_protocol("tls", inner, fields)
}

fn transport_wrapped(mut inner: Yaml, node: &Mapping, label: &str) -> Result<Yaml> {
    match yaml_string(node, "network")
        .unwrap_or("tcp")
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "tcp" => {}
        "ws" | "websocket" => inner = websocket_wrapper(inner, node),
        other => {
            return Err(Error::Invalid(format!(
                "{label} uses unsupported transport {other}"
            )));
        }
    }
    Ok(inner)
}

/// REALITY replaces TLS: the outer ClientHello authenticates the handshake, so
/// no `tls:` wrapper may be added on top of it. Returns the wrapped protocol
/// and whether Reality was applied.
fn reality_wrapped(inner: Yaml, node: &Mapping, label: &str) -> Result<(Yaml, bool)> {
    let Some(options) = yaml_mapping(node, "reality-opts") else {
        return if yaml_bool(node, "reality").unwrap_or(false) {
            Err(Error::Invalid(format!(
                "{label} enables Reality without reality-opts"
            )))
        } else {
            Ok((inner, false))
        };
    };
    let public_key = yaml_string(options, "public-key")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Invalid(format!("{label} is missing the Reality public key")))?;
    if !yaml_string(node, "network")
        .is_none_or(|network| network.is_empty() || network.eq_ignore_ascii_case("tcp"))
    {
        return Err(Error::Invalid(format!(
            "{label} uses Reality with a transport other than tcp"
        )));
    }
    let mut fields = Mapping::new();
    fields.insert(key("public_key"), string(public_key));
    if let Some(short_id) =
        yaml_string(options, "short-id").filter(|value| !value.trim().is_empty())
    {
        fields.insert(key("short_id"), string(short_id));
    }
    if let Some(sni) = yaml_string(node, "servername")
        .or_else(|| yaml_string(node, "sni"))
        .filter(|value| !value.trim().is_empty())
    {
        fields.insert(key("sni_hostname"), string(sni));
    }
    let vision =
        yaml_string(node, "flow").is_some_and(|flow| flow.to_ascii_lowercase().contains("vision"));
    fields.insert(key("vision"), boolean(vision));
    Ok((nested_protocol("reality", inner, fields), true))
}

/// `plugin: obfs` (simple-obfs): the fake traffic is written on the raw stream
/// below the Shadowsocks cipher.
fn obfs_plugin(node: &Mapping, label: &str) -> Result<Yaml> {
    let options = yaml_mapping(node, "plugin-opts")
        .ok_or_else(|| Error::Invalid(format!("{label} uses obfs without plugin-opts")))?;
    let mode = yaml_string(options, "mode")
        .unwrap_or("http")
        .to_ascii_lowercase();
    if mode != "http" && mode != "tls" {
        return Err(Error::Invalid(format!(
            "{label} uses unsupported obfs mode {mode}"
        )));
    }
    let host = yaml_string(options, "host")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| yaml_string(node, "server"))
        .ok_or_else(|| Error::Invalid(format!("{label} is missing the obfs host")))?;
    Ok(Yaml::Mapping(map([
        ("mode", string(&mode)),
        ("host", string(host)),
    ])))
}

fn client_hop(address: String, protocol: Yaml) -> Mapping {
    map([("address", string(&address)), ("protocol", protocol)])
}

fn protocol_label(protocol: &Yaml) -> String {
    let Some(value) = protocol.as_mapping() else {
        return "unknown".into();
    };
    if let Some(kind) = yaml_string(value, "type") {
        return kind.to_string();
    }
    value
        .get(key("protocol"))
        .map(protocol_label)
        .unwrap_or_else(|| "unknown".into())
}

fn convert_common_node(node: &Mapping) -> Result<Yaml> {
    let name = required_string(node, "name", "Proxy node")?;
    let kind = required_string(node, "type", &name)?.to_ascii_lowercase();
    let address = endpoint(node, &name)?;
    let server = yaml_string(node, "server").unwrap_or("");
    let inner = match kind.as_str() {
        "ss" | "shadowsocks" => {
            let mut fields = map([
                ("cipher", string(&required_string(node, "cipher", &name)?)),
                (
                    "password",
                    string(&required_string(node, "password", &name)?),
                ),
            ]);
            if let Some(plugin) = yaml_string(node, "plugin") {
                match plugin.to_ascii_lowercase().as_str() {
                    "obfs" => {
                        fields.insert(key("plugin"), obfs_plugin(node, &name)?);
                    }
                    other => {
                        return Err(Error::Invalid(format!(
                            "{name} uses unsupported Shadowsocks plugin {other}"
                        )));
                    }
                }
            }
            client_protocol("shadowsocks", fields)
        }
        "socks" | "socks5" => {
            let mut fields = Mapping::new();
            if let Some(value) = option_string(node, "username") {
                fields.insert(key("username"), value);
            }
            if let Some(value) = option_string(node, "password") {
                fields.insert(key("password"), value);
            }
            client_protocol("socks", fields)
        }
        "http" => {
            let mut fields = Mapping::new();
            if let Some(value) = option_string(node, "username") {
                fields.insert(key("username"), value);
            }
            if let Some(value) = option_string(node, "password") {
                fields.insert(key("password"), value);
            }
            let inner = client_protocol("http", fields);
            if yaml_bool(node, "tls").unwrap_or(false) {
                tls_wrapper(inner, node, server)
            } else {
                inner
            }
        }
        "trojan" => {
            let inner = client_protocol(
                "trojan",
                map([(
                    "password",
                    string(&required_string(node, "password", &name)?),
                )]),
            );
            let inner = transport_wrapped(inner, node, &name)?;
            if yaml_bool(node, "tls").unwrap_or(true) {
                tls_wrapper(inner, node, server)
            } else {
                inner
            }
        }
        "vmess" => {
            let cipher = match yaml_string(node, "cipher").unwrap_or("auto") {
                "auto" => "any",
                value => value,
            };
            let inner = client_protocol(
                "vmess",
                map([
                    ("cipher", string(cipher)),
                    (
                        "user_id",
                        string(
                            yaml_string(node, "uuid")
                                .or_else(|| yaml_string(node, "user_id"))
                                .filter(|value| !value.is_empty())
                                .ok_or_else(|| Error::Invalid(format!("{name} is missing uuid")))?,
                        ),
                    ),
                ]),
            );
            let inner = transport_wrapped(inner, node, &name)?;
            if yaml_bool(node, "tls").unwrap_or(false) {
                tls_wrapper(inner, node, server)
            } else {
                inner
            }
        }
        "vless" => {
            let inner = client_protocol(
                "vless",
                map([(
                    "user_id",
                    string(
                        yaml_string(node, "uuid")
                            .or_else(|| yaml_string(node, "user_id"))
                            .filter(|value| !value.is_empty())
                            .ok_or_else(|| Error::Invalid(format!("{name} is missing uuid")))?,
                    ),
                )]),
            );
            let (inner, is_reality) = reality_wrapped(inner, node, &name)?;
            let inner = transport_wrapped(inner, node, &name)?;
            if !is_reality && yaml_bool(node, "tls").unwrap_or(false) {
                tls_wrapper(inner, node, server)
            } else {
                inner
            }
        }
        // The proxy core implements these protocols as servers only. Emitting
        // a client hop would import successfully and then fail at validation,
        // so refuse the node here with a reason the user can act on.
        "hysteria2" | "hy2" | "tuic" | "tuicv5" => {
            return Err(Error::Invalid(format!(
                "{name} uses {kind}, which this proxy core supports as a server only"
            )));
        }
        "snell" => {
            let version = node
                .get(key("version"))
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
                .unwrap_or(3);
            if version != 3 {
                return Err(Error::Invalid(format!("{name} requires Snell v3")));
            }
            client_protocol(
                "snell",
                map([
                    (
                        "cipher",
                        string(yaml_string(node, "cipher").unwrap_or("aes-128-gcm")),
                    ),
                    (
                        "password",
                        string(
                            yaml_string(node, "psk")
                                .or_else(|| yaml_string(node, "password"))
                                .filter(|value| !value.is_empty())
                                .ok_or_else(|| Error::Invalid(format!("{name} is missing psk")))?,
                        ),
                    ),
                ]),
            )
        }
        "anytls" => {
            let inner = client_protocol(
                "anytls",
                map([(
                    "password",
                    string(&required_string(node, "password", &name)?),
                )]),
            );
            tls_wrapper(inner, node, server)
        }
        other => {
            return Err(Error::Invalid(format!(
                "{name} uses unsupported protocol {other}"
            )));
        }
    };
    Ok(Yaml::Mapping(client_hop(address, inner)))
}

fn normalize_existing_hop(node: &Mapping) -> Result<(String, String, Yaml)> {
    let name = required_string(node, "name", "Proxy node")?;
    required_string(node, "address", &name)?;
    let protocol = node
        .get(key("protocol"))
        .cloned()
        .ok_or_else(|| Error::Invalid(format!("{name} is missing protocol")))?;
    if protocol.as_mapping().is_none() {
        return Err(Error::Invalid(format!("{name} has an invalid protocol")));
    }
    let mut hop = node.clone();
    hop.remove(key("name"));
    Ok((name, protocol_label(&protocol), Yaml::Mapping(hop)))
}

fn parse_document(text: &str) -> Result<Yaml> {
    let structured = |value: &Yaml| matches!(value, Yaml::Sequence(_) | Yaml::Mapping(_));
    if let Ok(value) = serde_yaml::from_str::<Yaml>(text)
        && structured(&value)
    {
        return Ok(value);
    }
    // Many providers serve the YAML base64-encoded; a bare base64 blob also
    // parses as a YAML string, so decode whenever the result is not structured.
    let compact = text.split_whitespace().collect::<String>();
    let engines = [
        base64::engine::general_purpose::STANDARD,
        base64::engine::general_purpose::STANDARD_NO_PAD,
        base64::engine::general_purpose::URL_SAFE,
        base64::engine::general_purpose::URL_SAFE_NO_PAD,
    ];
    engines
        .iter()
        .find_map(|engine| {
            let decoded = String::from_utf8(engine.decode(&compact).ok()?).ok()?;
            serde_yaml::from_str::<Yaml>(&decoded)
                .ok()
                .filter(structured)
        })
        .ok_or_else(|| {
            Error::Invalid(
                "Invalid subscription: expected Clash-style YAML with a proxies list".into(),
            )
        })
}

pub fn parse_nodes(text: &str) -> Result<Vec<Node>> {
    if text.len() > MAX_BYTES {
        return Err(Error::Invalid("Subscription is too large".into()));
    }
    let document = parse_document(text)?;
    let values = match &document {
        Yaml::Sequence(items) => items.as_slice(),
        Yaml::Mapping(root) => root
            .get(key("proxies"))
            .and_then(Yaml::as_sequence)
            .map(Vec::as_slice)
            .ok_or_else(|| Error::Invalid("Subscription has no proxy nodes".into()))?,
        _ => return Err(Error::Invalid("Subscription has no proxy nodes".into())),
    };
    if values.is_empty() || values.len() > 2048 {
        return Err(Error::Invalid(
            "Subscription must contain 1–2048 nodes".into(),
        ));
    }

    let mut names = std::collections::HashSet::new();
    let mut nodes = Vec::with_capacity(values.len());
    let mut skipped = Vec::new();
    for (index, value) in values.iter().enumerate() {
        // A node this import cannot use (Reality, a plugin, an unknown
        // transport, a missing field) is skipped; the rest still import.
        // Keys keep the node's position, so refreshes do not shift them.
        let converted = value
            .as_mapping()
            .ok_or_else(|| Error::Invalid(format!("Invalid proxy node at position {}", index + 1)))
            .and_then(|node| {
                if node.contains_key(key("address")) && node.contains_key(key("protocol")) {
                    normalize_existing_hop(node)
                } else {
                    let name = required_string(node, "name", "Proxy node")?;
                    let protocol = required_string(node, "type", &name)?;
                    Ok((
                        name,
                        protocol.to_ascii_lowercase(),
                        convert_common_node(node)?,
                    ))
                }
            });
        let (name, protocol, config) = match converted {
            Ok(node) => node,
            Err(error) => {
                skipped.push(error.to_string());
                continue;
            }
        };
        if !names.insert(name.clone()) {
            return Err(Error::Invalid("Duplicate node name".into()));
        }
        nodes.push(Node {
            key: format!("n{}", index + 1),
            name,
            protocol,
            config,
        });
    }
    if nodes.is_empty() {
        let mut reasons = skipped;
        reasons.dedup();
        let shown = reasons
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::Invalid(format!("No usable proxy nodes: {shown}")));
    }
    Ok(nodes)
}

/// Download a subscription, optionally through a proxy URL.
pub fn fetch(url: &str, proxy: Option<&str>) -> Result<String> {
    let parsed = url::Url::parse(url.trim())
        .map_err(|_| Error::invalid("Enter a valid http(s) subscription URL"))?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
        return Err(Error::invalid("Enter a valid http(s) subscription URL"));
    }
    let mut builder = ureq::AgentBuilder::new()
        .resolver(crate::ipv4::resolve)
        .timeout(std::time::Duration::from_secs(45))
        .user_agent("clash-verge/v2.0.0");
    if let Some(proxy) = proxy {
        builder = builder.proxy(
            ureq::Proxy::new(proxy).map_err(|error| Error::invalid(format!("proxy: {error}")))?,
        );
    }
    let response = builder
        .build()
        .get(parsed.as_str())
        .call()
        .map_err(|error| Error::Network(format!("Could not download subscription: {error}")))?;
    let mut text = String::new();
    use std::io::Read as _;
    response
        .into_reader()
        .take(MAX_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|error| Error::Network(format!("Could not read subscription: {error}")))?;
    if text.len() > MAX_BYTES {
        return Err(Error::invalid("Subscription is too large"));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASH: &str = r#"
proxies:
  - name: "tokyo"
    type: trojan
    server: example.com
    port: 443
    password: secret
    sni: cdn.example.com
  - name: "ss"
    type: ss
    server: 10.0.0.1
    port: 8388
    cipher: aes-128-gcm
    password: pw
  - name: "socks"
    type: socks5
    server: 10.0.0.2
    port: 1080
"#;

    #[test]
    fn parses_clash_yaml_into_shoes_hops() {
        let nodes = parse_nodes(CLASH).unwrap();
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0].name, "tokyo");
        let hop = serde_yaml::to_string(&nodes[0].config).unwrap();
        assert!(hop.contains("address: example.com:443"), "{hop}");
        assert!(hop.contains("type: tls"), "{hop}");
        assert!(hop.contains("sni_hostname: cdn.example.com"), "{hop}");
        assert!(hop.contains("type: trojan"), "{hop}");
        let socks = serde_yaml::to_string(&nodes[2].config).unwrap();
        assert!(socks.contains("type: socks"), "{socks}");
    }

    #[test]
    fn quic_only_protocols_are_rejected_with_a_reason() {
        // The core has no client transport for these; importing them would
        // only move the failure to validate time.
        for kind in ["hysteria2", "hy2", "tuic", "tuicv5"] {
            let only = format!(
                "proxies:\n  - {{name: h, type: {kind}, server: h.example.com, port: 443, password: pw, uuid: 00000000-0000-0000-0000-000000000000}}\n"
            );
            let error = parse_nodes(&only).unwrap_err().to_string();
            assert!(
                error.contains("server only") && error.contains(kind),
                "{kind}: {error}"
            );
        }
        // A usable node beside them still imports.
        let mixed = "proxies:\n  - {name: h, type: hysteria2, server: h.example.com, port: 443, password: pw}\n  - {name: ok, type: socks5, server: h, port: 1}\n";
        let nodes = parse_nodes(mixed).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "ok");
    }

    #[test]
    fn accepts_base64_encoded_documents() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(CLASH);
        assert_eq!(parse_nodes(&encoded).unwrap().len(), 3);
    }

    #[test]
    fn reality_nodes_become_reality_hops() {
        let reality = r#"proxies:
  - name: "reality"
    type: vless
    server: r.example.com
    port: 443
    uuid: 00000000-0000-0000-0000-000000000000
    tls: true
    flow: xtls-rprx-vision
    servername: updates.cdn-apple.com
    reality-opts: {public-key: AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE, short-id: "0123abcd"}
"#;
        let nodes = parse_nodes(reality).unwrap();
        assert_eq!(nodes.len(), 1);
        let hop = serde_yaml::to_string(&nodes[0].config).unwrap();
        assert!(hop.contains("type: reality"), "{hop}");
        assert!(
            hop.contains("public_key: AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE"),
            "{hop}"
        );
        assert!(hop.contains("short_id: 0123abcd"), "{hop}");
        assert!(hop.contains("sni_hostname: updates.cdn-apple.com"), "{hop}");
        assert!(hop.contains("vision: true"), "{hop}");
        assert!(hop.contains("type: vless"), "{hop}");
        // REALITY replaces TLS; a tls: wrapper would break the handshake.
        assert!(!hop.contains("type: tls"), "{hop}");
    }

    #[test]
    fn obfs_plugin_is_mapped_below_shadowsocks() {
        let obfs = r#"proxies:
  - name: "obfs"
    type: ss
    server: o.example.com
    port: 2377
    cipher: aes-128-gcm
    password: pw
    plugin: obfs
    plugin-opts: {mode: tls, host: "a.example:252587"}
  - name: "plain"
    type: ss
    server: p.example.com
    port: 8388
    cipher: aes-128-gcm
    password: pw
"#;
        let nodes = parse_nodes(obfs).unwrap();
        assert_eq!(nodes.len(), 2);
        let hop = serde_yaml::to_string(&nodes[0].config).unwrap();
        assert!(hop.contains("type: shadowsocks"), "{hop}");
        assert!(hop.contains("mode: tls"), "{hop}");
        assert!(hop.contains("host: a.example:252587"), "{hop}");
        let plain = serde_yaml::to_string(&nodes[1].config).unwrap();
        assert!(!plain.contains("plugin"), "{plain}");
    }

    #[test]
    fn anytls_is_wrapped_in_tls() {
        let anytls = r#"proxies:
  - name: "any"
    type: anytls
    server: a.example.com
    port: 2577
    password: pw
    sni: default.example.com
"#;
        let nodes = parse_nodes(anytls).unwrap();
        let hop = serde_yaml::to_string(&nodes[0].config).unwrap();
        assert!(hop.contains("type: anytls"), "{hop}");
        assert!(hop.contains("type: tls"), "{hop}");
        assert!(hop.contains("sni_hostname: default.example.com"), "{hop}");
    }

    #[test]
    fn unsupported_nodes_are_skipped_and_the_rest_import() {
        // An unsupported plugin and protocol are skipped; the supported node
        // keeps its original position.
        let mixed = r#"proxies:
  - {name: v2, type: ss, server: h, port: 8388, cipher: aes-128-gcm, password: p, plugin: v2ray-plugin, plugin-opts: {mode: websocket}}
  - {name: wg, type: wireguard, server: h, port: 1}
  - {name: ok, type: socks5, server: h, port: 1}
"#;
        let nodes = parse_nodes(mixed).unwrap();
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].name, "ok");
        assert_eq!(nodes[0].key, "n3", "keys keep the original position");
    }

    #[test]
    fn imported_hops_are_accepted_by_shoes() {
        let fixtures = r#"proxies:
  - name: "reality"
    type: vless
    server: r.example.com
    port: 443
    uuid: 00000000-0000-0000-0000-000000000000
    tls: true
    flow: xtls-rprx-vision
    servername: updates.cdn-apple.com
    reality-opts: {public-key: AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE, short-id: "0123abcd"}
  - name: "obfs"
    type: ss
    server: o.example.com
    port: 2377
    cipher: chacha20-ietf-poly1305
    password: pw
    plugin: obfs
    plugin-opts: {mode: tls, host: "a.example:252587"}
  - name: "any"
    type: anytls
    server: a.example.com
    port: 2577
    password: pw
    sni: default.example.com
"#;
        let nodes = parse_nodes(fixtures).unwrap();
        assert_eq!(nodes.len(), 3);
        for node in &nodes {
            let rendered = crate::proxy::render(&node.config, 1080).unwrap();
            if let Err(error) = crate::proxy::validate(&rendered) {
                panic!("{} was rejected: {error}\n{rendered}", node.name);
            }
        }
    }

    #[test]
    fn rejects_garbage_and_duplicates() {
        assert!(parse_nodes("not: [valid").is_err());
        let duplicate = "proxies:\n  - {name: a, type: socks5, server: h, port: 1}\n  - {name: a, type: socks5, server: h, port: 2}\n";
        assert!(parse_nodes(duplicate).is_err());
    }
}
