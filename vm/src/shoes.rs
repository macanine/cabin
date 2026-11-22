//! Subscription sources and managed configuration for the embedded shoes proxy.
//!
//! Node lists are imported from the common YAML subscription shape and
//! compiled into native shoes client chains. No second proxy engine is used.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_yaml::{Mapping, Value as Yaml};

use crate::paths::{ResourcePaths, StatePaths};
use crate::util::{atomic_write_json, capture, ensure_dir, read_json};
use crate::{Error, Result};

// Keep test binaries local to each test thread, so parallel tests never share
// a temporary executable or change the production resource path.
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_BINARY_PATH: std::cell::RefCell<Option<PathBuf>> = const {
        std::cell::RefCell::new(None)
    };
    pub(crate) static TEST_LISTEN_PORT: std::cell::Cell<Option<u16>> = const {
        std::cell::Cell::new(None)
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Node {
    key: String,
    name: String,
    protocol: String,
    config: Yaml,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Source {
    id: String,
    name: String,
    url: Option<String>,
    nodes: Vec<Node>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSummary {
    pub id: String,
    pub name: String,
    pub has_url: bool,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeSummary {
    pub id: String,
    pub name: String,
    pub source: String,
    pub protocol: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Selection {
    pub selected: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Catalog {
    pub sources: Vec<SourceSummary>,
    pub nodes: Vec<NodeSummary>,
    pub selection: Selection,
}

fn directory(home: &Path) -> PathBuf {
    StatePaths::new(home).shoes()
}

pub(crate) fn sources_path(home: &Path) -> PathBuf {
    StatePaths::new(home).shoes_sources()
}

pub(crate) fn selection_path(home: &Path) -> PathBuf {
    StatePaths::new(home).shoes_selection()
}

pub fn config_path(home: &Path) -> PathBuf {
    StatePaths::new(home).shoes_config()
}

pub fn binary_path() -> PathBuf {
    #[cfg(test)]
    if let Some(path) = TEST_BINARY_PATH.with(|path| path.borrow().clone()) {
        return path;
    }
    ResourcePaths::current().shoes_binary()
}

pub(crate) fn listen_port() -> u16 {
    #[cfg(test)]
    if let Some(port) = TEST_LISTEN_PORT.with(|port| port.get()) {
        return port;
    }
    18080
}

fn sources(home: &Path) -> Result<Vec<Source>> {
    let path = sources_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

fn save_sources(home: &Path, items: &[Source]) -> Result<()> {
    ensure_dir(&directory(home))?;
    atomic_write_json(&sources_path(home), &serde_json::to_value(items)?, 0o600)
}

fn key(name: &str) -> Yaml {
    Yaml::String(name.to_string())
}

fn map(pairs: impl IntoIterator<Item = (&'static str, Yaml)>) -> Mapping {
    pairs
        .into_iter()
        .map(|(name, value)| (key(name), value))
        .collect()
}

fn string(value: &str) -> Yaml {
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
        .ok_or_else(|| Error::Message(format!("{label} is missing {name}")))
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
        .ok_or_else(|| Error::Message(format!("{label} has an invalid port")))?;
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
            return Err(Error::Message(format!(
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
            Err(Error::Message(format!(
                "{label} enables Reality without reality-opts"
            )))
        } else {
            Ok((inner, false))
        };
    };
    let public_key = yaml_string(options, "public-key")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| Error::Message(format!("{label} is missing the Reality public key")))?;
    if !yaml_string(node, "network")
        .is_none_or(|network| network.is_empty() || network.eq_ignore_ascii_case("tcp"))
    {
        return Err(Error::Message(format!(
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
        .ok_or_else(|| Error::Message(format!("{label} uses obfs without plugin-opts")))?;
    let mode = yaml_string(options, "mode")
        .unwrap_or("http")
        .to_ascii_lowercase();
    if mode != "http" && mode != "tls" {
        return Err(Error::Message(format!(
            "{label} uses unsupported obfs mode {mode}"
        )));
    }
    let host = yaml_string(options, "host")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| yaml_string(node, "server"))
        .ok_or_else(|| Error::Message(format!("{label} is missing the obfs host")))?;
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
                        return Err(Error::Message(format!(
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
                                .ok_or_else(|| Error::Message(format!("{name} is missing uuid")))?,
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
                            .ok_or_else(|| Error::Message(format!("{name} is missing uuid")))?,
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
            return Err(Error::Message(format!(
                "{name} uses {kind}, which this proxy core supports as a server only"
            )));
        }
        "snell" => {
            let version = node
                .get(key("version"))
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
                .unwrap_or(3);
            if version != 3 {
                return Err(Error::Message(format!("{name} requires Snell v3")));
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
                                .ok_or_else(|| Error::Message(format!("{name} is missing psk")))?,
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
            return Err(Error::Message(format!(
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
        .ok_or_else(|| Error::Message(format!("{name} is missing protocol")))?;
    if protocol.as_mapping().is_none() {
        return Err(Error::Message(format!("{name} has an invalid protocol")));
    }
    let mut hop = node.clone();
    hop.remove(key("name"));
    Ok((name, protocol_label(&protocol), Yaml::Mapping(hop)))
}

fn parse_document(text: &str) -> Result<Yaml> {
    serde_yaml::from_str(text)
        .or_else(|_| {
            let compact = text.split_whitespace().collect::<String>();
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(compact)
                .map_err(|_| ())?;
            let decoded = String::from_utf8(decoded).map_err(|_| ())?;
            serde_yaml::from_str(&decoded).map_err(|_| ())
        })
        .map_err(|_| Error::Message("Invalid subscription YAML".into()))
}

fn parse_nodes(text: &str) -> Result<Vec<Node>> {
    if text.len() > 2 * 1024 * 1024 {
        return Err(Error::Message("Subscription is too large".into()));
    }
    let document = parse_document(text)?;
    let values = match &document {
        Yaml::Sequence(items) => items.as_slice(),
        Yaml::Mapping(root) => root
            .get(key("proxies"))
            .and_then(Yaml::as_sequence)
            .map(Vec::as_slice)
            .ok_or_else(|| Error::Message("Subscription has no proxy nodes".into()))?,
        _ => return Err(Error::Message("Subscription has no proxy nodes".into())),
    };
    if values.is_empty() || values.len() > 2048 {
        return Err(Error::Message(
            "Subscription must contain 1–2048 nodes".into(),
        ));
    }

    let mut names = std::collections::HashSet::new();
    let mut nodes = Vec::with_capacity(values.len());
    let mut skipped = Vec::new();
    for (index, value) in values.iter().enumerate() {
        // Nodes this import cannot use (an unsupported plugin, unknown
        // transport or protocol, missing fields) are skipped; keys keep positions.
        let converted = value
            .as_mapping()
            .ok_or_else(|| Error::Message(format!("Invalid proxy node at position {}", index + 1)))
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
            return Err(Error::Message("Duplicate node name".into()));
        }
        nodes.push(Node {
            key: format!("n{}", index + 1),
            name,
            protocol,
            config,
        });
    }
    if nodes.is_empty() {
        skipped.dedup();
        let shown = skipped
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        return Err(Error::Message(format!("No usable proxy nodes: {shown}")));
    }
    Ok(nodes)
}

fn fetch(url: &str) -> Result<String> {
    let parsed =
        url::Url::parse(url).map_err(|_| Error::Message("Invalid subscription URL".into()))?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str().is_none()
        || url.len() > 8192
        || url.contains(['\n', '\r'])
    {
        return Err(Error::Message("Invalid subscription URL".into()));
    }
    let config = format!(
        "url = \"{}\"\n",
        url.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let argv = vector_args(&[
        "/usr/bin/curl",
        "-q",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-time",
        "45",
        "--max-filesize",
        "2097152",
        "--config",
        "-",
    ]);
    let output = capture(&argv, std::time::Duration::from_secs(50), Some(&config));
    if output.code != 0 {
        return Err(Error::Message("Could not download subscription".into()));
    }
    if output.output.len() > 2 * 1024 * 1024 {
        return Err(Error::Message("Subscription is too large".into()));
    }
    Ok(output.output)
}

fn vector_args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn node_id(source: &Source, node: &Node) -> String {
    format!("{}::{}", source.id, node.key)
}

pub fn catalog(home: &Path) -> Result<Catalog> {
    let sources = sources(home)?;
    let mut nodes = Vec::new();
    let mut result = Vec::new();
    for source in &sources {
        for node in &source.nodes {
            nodes.push(NodeSummary {
                id: node_id(source, node),
                name: node.name.clone(),
                source: source.name.clone(),
                protocol: node.protocol.clone(),
            });
        }
        result.push(SourceSummary {
            id: source.id.clone(),
            name: source.name.clone(),
            has_url: source.url.is_some(),
            count: source.nodes.len(),
        });
    }
    let selection = match read_json(&selection_path(home)) {
        Some(value) => serde_json::from_value(value)?,
        None => Selection::default(),
    };
    Ok(Catalog {
        sources: result,
        nodes,
        selection,
    })
}

pub fn import(
    home: &Path,
    name: &str,
    url: Option<&str>,
    contents: Option<&str>,
) -> Result<Catalog> {
    let name = name.trim();
    if name.is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
        return Err(Error::Message("Enter a subscription name".into()));
    }
    let url = url.map(str::trim).filter(|value| !value.is_empty());
    let text = if let Some(url) = url {
        fetch(url)?
    } else {
        contents.unwrap_or("").to_string()
    };
    let nodes = parse_nodes(&text)?;
    let mut items = sources(home)?;
    if items.iter().any(|source| source.name == name) {
        return Err(Error::Message("Subscription name already exists".into()));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros();
    items.push(Source {
        id: format!("s{stamp}"),
        name: name.into(),
        url: url.map(str::to_string),
        nodes,
    });
    save_sources(home, &items)?;
    catalog(home)
}

pub fn refresh(home: &Path, id: &str) -> Result<Catalog> {
    let mut items = sources(home)?;
    let source = items
        .iter_mut()
        .find(|source| source.id == id)
        .ok_or_else(|| Error::Message("Subscription not found".into()))?;
    let url = source
        .url
        .as_deref()
        .ok_or_else(|| Error::Message("This subscription has no URL".into()))?;
    source.nodes = parse_nodes(&fetch(url)?)?;
    save_sources(home, &items)?;
    catalog(home)
}

pub fn remove(home: &Path, id: &str) -> Result<Catalog> {
    let mut items = sources(home)?;
    let previous_len = items.len();
    items.retain(|source| source.id != id);
    if items.len() == previous_len {
        return Err(Error::Message("Proxy source not found".into()));
    }
    save_sources(home, &items)?;
    catalog(home)
}

pub fn selected(home: &Path) -> Option<String> {
    read_json(&selection_path(home)).and_then(|value| {
        value
            .get("selected")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

pub fn selection_valid(home: &Path, selected: &str) -> Result<bool> {
    Ok(sources(home)?
        .iter()
        .flat_map(|source| source.nodes.iter().map(move |node| node_id(source, node)))
        .any(|id| id == selected))
}

pub fn check(path: &Path) -> Result<()> {
    let binary = binary_path();
    if !binary.is_file() {
        return Err(Error::Message(format!(
            "embedded shoes binary not found: {}",
            binary.display()
        )));
    }
    let argv = vec![
        binary.display().to_string(),
        "check".into(),
        path.display().to_string(),
    ];
    let result = capture(&argv, std::time::Duration::from_secs(30), None);
    if result.code != 0 {
        return Err(Error::Message(format!(
            "shoes configuration is invalid: {}",
            result.output.trim()
        )));
    }
    Ok(())
}

pub(crate) fn controller_info(path: &Path) -> Result<(String, String)> {
    let text = std::fs::read_to_string(path)?;
    let document: Yaml = serde_yaml::from_str(&text)
        .map_err(|_| Error::Message("Invalid shoes controller configuration".into()))?;
    let entries = document
        .as_sequence()
        .ok_or_else(|| Error::Message("Invalid shoes configuration".into()))?;
    for entry in entries {
        let Some(entry) = entry.as_mapping() else {
            continue;
        };
        let Some(address) = yaml_string(entry, "clash_api_address") else {
            continue;
        };
        let secret = yaml_string(entry, "secret")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Error::Message("Shoes controller has no secret".into()))?;
        return Ok((address.to_string(), secret.to_string()));
    }
    Err(Error::Message("Shoes controller is not configured".into()))
}

/// Probe one configured leaf through the shoes controller. Credentials and the
/// bearer token are passed over stdin so they never appear in the process argv.
pub(crate) fn node_delay(
    config: &Path,
    node: &str,
    probe_url: &str,
    timeout_ms: u64,
) -> Result<Value> {
    let (controller, secret) = controller_info(config)?;
    let mut url = url::Url::parse(&format!("http://{controller}/proxies"))
        .map_err(|error| Error::Message(error.to_string()))?;
    url.path_segments_mut()
        .map_err(|_| Error::Message("Invalid shoes controller URL".into()))?
        .extend([node, "delay"]);
    url.query_pairs_mut()
        .append_pair("url", probe_url)
        .append_pair("timeout", &timeout_ms.to_string());
    let curl = format!(
        "header = \"Authorization: Bearer {}\"\nurl = \"{}\"\n",
        secret,
        url.as_str().replace('\\', "\\\\").replace('"', "\\\"")
    );
    let argv = vector_args(&[
        "/usr/bin/curl",
        "-q",
        "--silent",
        "--show-error",
        "--max-time",
        "8",
        "--config",
        "-",
    ]);
    let result = capture(&argv, std::time::Duration::from_secs(10), Some(&curl));
    if result.code != 0 {
        return Err(Error::Message(if result.code == 124 {
            "Latency test timed out".into()
        } else {
            brief_output(&result.output)
        }));
    }
    serde_json::from_str(&result.output).map_err(|_| {
        Error::Message(if result.output.trim().is_empty() {
            "Shoes returned no latency result".into()
        } else {
            brief_output(&result.output)
        })
    })
}

fn brief_output(output: &str) -> String {
    let text = output.trim();
    let mut result: String = text.chars().take(240).collect();
    if text.chars().count() > 240 {
        result.push('…');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_reality_and_obfs_and_rejects_server_only_protocols() {
        let nodes = parse_nodes(
            r#"proxies:
  - {name: r, type: vless, server: r.example.com, port: 443, uuid: 00000000-0000-0000-0000-000000000000, tls: true, flow: xtls-rprx-vision, servername: cdn.example.com, reality-opts: {public-key: AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE, short-id: "0123abcd"}}
  - {name: o, type: ss, server: o.example.com, port: 2377, cipher: aes-128-gcm, password: pw, plugin: obfs, plugin-opts: {mode: tls, host: cdn.example.com}}
"#,
        )
        .unwrap();
        assert_eq!(nodes.len(), 2);
        let reality = serde_yaml::to_string(&nodes[0].config).unwrap();
        assert!(reality.contains("type: reality"), "{reality}");
        assert!(reality.contains("vision: true"), "{reality}");
        assert!(!reality.contains("type: tls"), "{reality}");
        let obfs = serde_yaml::to_string(&nodes[1].config).unwrap();
        assert!(obfs.contains("plugin:"), "{obfs}");
        assert!(obfs.contains("mode: tls"), "{obfs}");
        let error = parse_nodes(
            "proxies:\n  - {name: h, type: hysteria2, server: h.example.com, port: 443, password: pw}\n",
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("server only"), "{error}");
    }

    #[test]
    fn accepts_native_shoes_client_hops() {
        let parsed = parse_nodes(
            r#"
- name: local
  address: 127.0.0.1:1080
  protocol:
    type: socks
"#,
        )
        .unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].protocol, "socks");
    }
}
