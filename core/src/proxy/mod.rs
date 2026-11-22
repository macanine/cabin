//! Proxy: subscription catalog, shoes config rendering, and the embedded
//! in-process shoes listener.

pub mod nodes;
pub mod subscription;

use crate::util::LockExt;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_yaml::Value as Yaml;

use crate::error::{Error, Result};
use crate::model::{ProbeResult, ProxyCatalog, ProxyNode, ProxySource};
use crate::store::{Store, StoredSource};
use crate::util::{clean_name, new_id, now_millis};
use subscription::{Node, key, map, string};

const LOCAL_MASKS: [&str; 6] = [
    "127.0.0.0/8",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16",
    "localhost",
];

fn node_id(source_id: &str, node: &Node) -> String {
    format!("{source_id}::{}", node.key)
}

fn source_nodes(source: &StoredSource) -> Result<Vec<Node>> {
    Ok(serde_json::from_str(&source.nodes_json)?)
}

// ------------------------------------------------------------------ catalog

pub fn catalog(store: &Store) -> Result<ProxyCatalog> {
    let mut catalog = ProxyCatalog::default();
    let sources = store.proxy_sources()?;
    if sources.is_empty() {
        catalog.groups = nodes::groups(store, &catalog.nodes)?;
        return Ok(catalog);
    }
    let salt = nodes::revision_salt(store)?;
    for source in sources {
        let nodes = source_nodes(&source)?;
        catalog.sources.push(ProxySource {
            id: source.id.clone(),
            name: source.name.clone(),
            has_url: source.url.is_some(),
            node_count: nodes.len() as u32,
            updated_at: source.updated_at,
        });
        for node in &nodes {
            catalog.nodes.push(ProxyNode {
                id: node_id(&source.id, node),
                name: node.name.clone(),
                source_id: source.id.clone(),
                source_name: source.name.clone(),
                protocol: node.protocol.clone(),
                config_revision: nodes::config_revision(&salt, node)?,
            });
        }
    }
    catalog.groups = nodes::groups(store, &catalog.nodes)?;
    Ok(catalog)
}

/// Import a subscription from a URL or from pasted/file contents.
pub fn import(
    store: &Store,
    name: &str,
    url: Option<&str>,
    contents: Option<&str>,
    via_proxy: Option<&str>,
) -> Result<ProxyCatalog> {
    let name = clean_name(name, "subscription")?;
    let url = url.map(str::trim).filter(|value| !value.is_empty());
    let text = match (url, contents) {
        (Some(url), _) => subscription::fetch(url, via_proxy)?,
        (None, Some(text)) if !text.trim().is_empty() => text.to_string(),
        _ => return Err(Error::invalid("Paste a subscription or enter its URL")),
    };
    let nodes = subscription::parse_nodes(&text)?;
    store.upsert_proxy_source(&StoredSource {
        id: new_id("src"),
        name,
        url: url.map(str::to_string),
        nodes_json: serde_json::to_string(&nodes)?,
        updated_at: now_millis(),
    })?;
    catalog(store)
}

pub fn refresh_source(store: &Store, id: &str) -> Result<StoredSource> {
    store
        .proxy_sources()?
        .into_iter()
        .find(|source| source.id == id)
        .ok_or_else(|| Error::NotFound("subscription".into()))
}

pub fn fetch_refresh(source: &StoredSource, via_proxy: Option<&str>) -> Result<Vec<Node>> {
    let url = source
        .url
        .as_deref()
        .ok_or_else(|| Error::invalid("This subscription was pasted and has no URL to refresh"))?;
    subscription::parse_nodes(&subscription::fetch(url, via_proxy)?)
}

pub fn save_refresh(
    store: &Store,
    original: &StoredSource,
    nodes: &[Node],
    active: &[String],
) -> Result<ProxyCatalog> {
    let mut source = refresh_source(store, &original.id)?;
    if &source != original {
        return Err(Error::invalid("The subscription changed while it was being downloaded"));
    }
    if !active.is_empty() {
        let previous = source_nodes(&source)?;
        let find = |list: &[Node], id: &str| list.iter().position(|node| node_id(&source.id, node) == id);
        if active.iter().any(|id| match (find(&previous, id), find(nodes, id)) {
            (Some(old), Some(new)) => previous[old] != nodes[new],
            _ => true,
        }) {
            return Err(Error::invalid("A node in the route changed or disappeared in the refreshed subscription. Take it out of the route first."));
        }
    }
    source.nodes_json = serde_json::to_string(nodes)?;
    source.updated_at = now_millis();
    store.upsert_proxy_source(&source)?;
    catalog(store)
}

pub fn find_node(store: &Store, id: &str) -> Result<Node> {
    let (source_id, _) = id
        .split_once("::")
        .ok_or_else(|| Error::NotFound("proxy node".into()))?;
    for source in store.proxy_sources()? {
        if source.id != source_id {
            continue;
        }
        for node in source_nodes(&source)? {
            if node_id(&source.id, &node) == id {
                return Ok(node);
            }
        }
    }
    Err(Error::NotFound("proxy node".into()))
}

// ------------------------------------------------------------------- render

/// A manual proxy URL (`http`, `https`, `socks5`, `socks5h`, with optional
/// credentials) as subscription text holding one node, for `import`.
pub fn manual_subscription(raw: &str) -> Result<String> {
    let url = url::Url::parse(raw.trim())
        .map_err(|e| Error::invalid(format!("Invalid proxy URL: {e}")))?;
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return Err(Error::invalid(
            "Use an HTTP, HTTPS or SOCKS5 proxy URL with a host and port",
        ));
    };
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h")
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::invalid(
            "Use an HTTP, HTTPS or SOCKS5 proxy URL with a host and port",
        ));
    }
    let decode = |s: &str| {
        percent_encoding::percent_decode_str(s)
            .decode_utf8()
            .map(|v| v.into_owned())
            .map_err(|_| Error::invalid("Invalid proxy credentials"))
    };
    let mut node = serde_json::json!({
        "name": format!("{host}:{port}"),
        "type": if url.scheme().starts_with("socks") { "socks5" } else { "http" },
        "server": host,
        "port": port,
        "tls": url.scheme() == "https",
    });
    if !url.username().is_empty() {
        node["username"] = serde_json::json!(decode(url.username())?);
    }
    if let Some(password) = url.password() {
        node["password"] = serde_json::json!(decode(password)?);
    }
    Ok(serde_json::json!({"proxies": [node]}).to_string())
}

/// The server a node connects to (its `address` without the port).
pub fn node_host(node: &Node) -> Option<String> {
    let address = node.config.get("address")?.as_str()?;
    let host = match address.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => host,
        _ => address,
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (!host.is_empty()).then(|| host.to_string())
}

/// A shoes `client_chain` for `hops`, entry first: one hop as a mapping,
/// several as a sequence (each hop reached through the ones before it).
pub fn chain_value(hops: &[Yaml]) -> Result<Yaml> {
    chain_value_on_interface(hops, None)
}

/// Apply the interface only to the first physical connection in the chain.
pub fn chain_value_on_interface(hops: &[Yaml], interface: Option<&str>) -> Result<Yaml> {
    let mut clean = Vec::with_capacity(hops.len());
    for (index, hop) in hops.iter().enumerate() {
        let mut hop = hop
            .as_mapping()
            .cloned()
            .ok_or_else(|| Error::invalid("Invalid stored proxy node"))?;
        hop.remove(key("name"));
        hop.remove(key("bind_interface"));
        if index == 0 && let Some(interface) = interface {
            hop.insert(key("bind_interface"), string(interface));
        }
        clean.push(Yaml::Mapping(hop));
    }
    match clean.len() {
        0 => Err(Error::invalid("The route has no nodes")),
        1 => Ok(clean.remove(0)),
        _ => Ok(Yaml::Sequence(clean)),
    }
}

/// Render a shoes config: one mixed (HTTP + SOCKS5) listener on loopback that
/// sends local networks direct and everything else through `hop`.
pub fn render(hop: &Yaml, port: u16) -> Result<String> {
    render_chain(std::slice::from_ref(hop), port)
}

/// `render` for a chain of hops, entry first.
pub fn render_chain(hops: &[Yaml], port: u16) -> Result<String> {
    render_listener(hops, port, true, None)
}

pub fn render_chain_on_interface(hops: &[Yaml], port: u16, interface: Option<&str>) -> Result<String> {
    render_listener(hops, port, false, interface)
}

fn render_listener(hops: &[Yaml], port: u16, allow_local_direct: bool, interface: Option<&str>) -> Result<String> {
    let direct = Yaml::Mapping(map([(
        "protocol",
        Yaml::Mapping(map([("type", string("direct"))])),
    )]));
    let local_rule = Yaml::Mapping(map([
        (
            "masks",
            Yaml::Sequence(LOCAL_MASKS.iter().map(|mask| string(mask)).collect()),
        ),
        ("action", string("allow")),
        ("client_chain", direct),
    ]));
    let proxy_rule = Yaml::Mapping(map([
        (
            "masks",
            Yaml::Sequence(vec![string("0.0.0.0/0")]),
        ),
        ("action", string("allow")),
        ("client_chain", chain_value_on_interface(hops, interface)?),
    ]));
    let rules = if allow_local_direct {
        vec![local_rule, proxy_rule]
    } else {
        vec![proxy_rule]
    };
    let listener = Yaml::Mapping(map([
        ("address", string(&format!("127.0.0.1:{port}"))),
        (
            "protocol",
            Yaml::Mapping(map([
                ("type", string("mixed")),
                ("udp_enabled", Yaml::Bool(true)),
            ])),
        ),
        ("rules", Yaml::Sequence(rules)),
    ]));
    serde_yaml::to_string(&Yaml::Sequence(vec![listener]))
        .map_err(|error| Error::Internal(error.to_string()))
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("shoes")
        .enable_all()
        .build()
        .map_err(|error| Error::io("start proxy runtime", error))
}

/// Validate a rendered config with shoes itself, without binding sockets.
pub fn validate(config: &str) -> Result<()> {
    validate_on_interface(config, None)
}

pub fn validate_on_interface(config: &str, interface: Option<&str>) -> Result<()> {
    let dns = interface.map(bound_dns).transpose()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::io("start proxy runtime", error))?;
    runtime.block_on(async {
        if let Some(dns) = &dns {
            shoes::embed::validate_with_bound_dns(config, dns).await
        } else {
            shoes::embed::validate(config).await
        }
    }).map_err(|error| Error::invalid(format!("Proxy configuration rejected: {error}")))
}

pub fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub fn free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| Error::io("find a free port", error))?;
    Ok(listener
        .local_addr()
        .map_err(|error| Error::io("find a free port", error))?
        .port())
}

// ------------------------------------------------------------------ service

struct Running {
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

/// Per-listener bootstrap DNS; no global interface state is shared by tests.
pub fn bound_dns(interface: &str) -> Result<shoes::embed::BoundDns> {
    let servers = [([223, 5, 5, 5], 53), ([119, 29, 29, 29], 53), ([1, 1, 1, 1], 53)]
        .into_iter().map(SocketAddr::from).collect();
    shoes::embed::BoundDns::new(interface.to_string(), servers)
        .map_err(|error| Error::Network(format!("Cannot configure interface DNS: {error}")))
}

/// The in-process shoes listener. At most one runs per instance.
#[derive(Default)]
pub struct EmbeddedProxy {
    running: Mutex<Option<Running>>,
}

impl EmbeddedProxy {
    pub fn start(&self, config: String, port: u16) -> Result<()> {
        self.start_on_interface(config, port, None)
    }

    pub fn start_on_interface(&self, config: String, port: u16, interface: Option<&str>) -> Result<()> {
        let interface = interface.map(str::to_owned);
        self.stop();
        if !port_free(port) {
            return Err(Error::Conflict(format!(
                "Port {port} on 127.0.0.1 is already in use; choose another proxy port"
            )));
        }
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<String>();
        let thread = std::thread::Builder::new()
            .name("embedded-proxy".into())
            .spawn(move || {
                let outcome = runtime().and_then(|runtime| {
                    let dns = interface.as_deref().map(bound_dns).transpose()?;
                    runtime.block_on(async {
                        if let Some(dns) = dns {
                            shoes::embed::run_with_bound_dns(&config, shutdown_rx, dns).await
                        } else {
                            shoes::embed::run(&config, shutdown_rx).await
                        }
                    }).map_err(|error| Error::Network(error.to_string()))
                });
                if let Err(error) = outcome {
                    let _ = done_tx.send(error.to_string());
                }
            })
            .map_err(|error| Error::io("start proxy thread", error))?;

        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let started = Instant::now();
        loop {
            if let Ok(message) = done_rx.try_recv() {
                let _ = thread.join();
                return Err(Error::Network(format!("Proxy failed to start: {message}")));
            }
            if TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok() {
                break;
            }
            if started.elapsed() > Duration::from_secs(8) {
                let _ = shutdown_tx.send(());
                let _ = thread.join();
                return Err(Error::Network(
                    "Proxy did not start listening within 8 seconds".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        *self.running.locked() = Some(Running {
            shutdown: Some(shutdown_tx),
            thread: Some(thread),
        });
        Ok(())
    }

    pub fn stop(&self) {
        let running = self
            .running.locked()
            .take();
        if let Some(mut running) = running {
            if let Some(shutdown) = running.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(thread) = running.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// Whether the listener is up (its thread has not ended).
    pub fn running(&self) -> bool {
        self.running.locked()
            .as_ref()
            .and_then(|running| running.thread.as_ref())
            .is_some_and(|thread| !thread.is_finished())
    }
}

impl Drop for EmbeddedProxy {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Measure a TCP connection to `target` (`host:port`), optionally through an
/// HTTP proxy, using an HTTP request (any HTTP response counts as reachable).
pub fn probe(target: &str, proxy_url: Option<&str>) -> ProbeResult {
    let host = target
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(target);
    let mut builder = ureq::AgentBuilder::new()
        .resolver(crate::ipv4::resolve)
        .timeout(Duration::from_secs(8))
        .redirects(0);
    if let Some(proxy) = proxy_url {
        match ureq::Proxy::new(proxy) {
            Ok(proxy) => builder = builder.proxy(proxy),
            Err(error) => {
                return ProbeResult {
                    ok: false,
                    latency_ms: None,
                    detail: format!("Invalid proxy URL: {error}"),
                };
            }
        }
    }
    let started = Instant::now();
    let result = builder.build().head(&format!("https://{host}/")).call();
    let elapsed = started.elapsed().as_millis().min(u32::MAX as u128) as u32;
    match result {
        Ok(_) | Err(ureq::Error::Status(_, _)) => ProbeResult {
            ok: true,
            latency_ms: Some(elapsed),
            detail: format!(
                "Reached {host} {} in {elapsed} ms",
                if proxy_url.is_some() {
                    "through the proxy"
                } else {
                    "directly"
                }
            ),
        },
        Err(error) => ProbeResult {
            ok: false,
            latency_ms: None,
            detail: format!("Could not reach {host}: {error}"),
        },
    }
}

/// Start a throwaway listener for `node`, probe through it, and stop it.
pub fn probe_node(node: &Node, target: &str) -> Result<ProbeResult> {
    probe_chain(std::slice::from_ref(node), target)
}

/// `probe_node` through a chain of nodes, entry first.
pub fn probe_chain(nodes: &[Node], target: &str) -> Result<ProbeResult> {
    probe_chain_on_interface(nodes, target, None)
}

pub fn probe_chain_on_interface(nodes: &[Node], target: &str, interface: Option<&str>) -> Result<ProbeResult> {
    let port = free_port()?;
    let hops: Vec<Yaml> = nodes.iter().map(|node| node.config.clone()).collect();
    let config = render_chain_on_interface(&hops, port, interface)?;
    let proxy = EmbeddedProxy::default();
    proxy.start_on_interface(config, port, interface)?;
    let result = probe(target, Some(&format!("http://127.0.0.1:{port}")));
    proxy.stop();
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socks_node() -> Node {
        subscription::parse_nodes(
            "proxies:\n  - {name: up, type: socks5, server: 127.0.0.1, port: 1}\n",
        )
        .unwrap()
        .remove(0)
    }

    #[test]
    fn an_obsolete_refresh_leaves_the_current_source_in_place() -> Result<()> {
        let store = Store::in_memory()?;
        let node = socks_node();
        let mut source = StoredSource {
            id: "test".into(),
            name: "test".into(),
            url: Some("https://example.com/subscription".into()),
            nodes_json: serde_json::to_string(std::slice::from_ref(&node))?,
            updated_at: 1,
        };
        store.upsert_proxy_source(&source)?;
        let original = refresh_source(&store, &source.id)?;
        source.updated_at = 2;
        store.upsert_proxy_source(&source)?;
        assert!(save_refresh(&store, &original, &[node], &[]).is_err());
        assert_eq!(refresh_source(&store, &source.id)?, source);
        Ok(())
    }

    #[test]
    fn rendered_config_is_accepted_by_shoes() -> Result<()> {
        let config = render(&socks_node().config, 18999)?;
        assert!(config.contains("type: mixed"), "{config}");
        validate(&config)
    }

    #[test]
    fn a_chain_renders_as_ordered_hops_and_shoes_accepts_it() -> Result<()> {
        let nodes = subscription::parse_nodes(
            "proxies:\n  - {name: entry, type: socks5, server: 127.0.0.1, port: 1}\n  - {name: exit, type: socks5, server: 127.0.0.2, port: 2}\n",
        )?;
        let hops: Vec<Yaml> = nodes.iter().map(|n| n.config.clone()).collect();
        let chain = chain_value(&hops)?;
        let list = chain.as_sequence().expect("two hops render as a sequence");
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|hop| hop.get("name").is_none()));
        assert!(
            serde_yaml::to_string(&list[0])
                .unwrap()
                .contains("127.0.0.1")
        );
        assert!(chain_value(&hops[..1])?.is_mapping());
        assert!(chain_value(&[]).is_err());
        validate(&render_chain(&hops, 18998)?)
    }

    #[test]
    fn interface_is_applied_only_to_the_first_hop() -> Result<()> {
        let node = socks_node();
        let mut contaminated = node.config.clone();
        contaminated.as_mapping_mut().unwrap().insert(key("bind_interface"), string("old"));
        let hops = vec![contaminated.clone(), contaminated];
        let chain = chain_value_on_interface(&hops, Some("en8"))?;
        let hops = chain.as_sequence().unwrap();
        assert_eq!(hops[0].get("bind_interface").and_then(Yaml::as_str), Some("en8"));
        assert!(hops[1].get("bind_interface").is_none());
        let system = chain_value_on_interface(&hops.to_vec(), None)?;
        assert!(system.as_sequence().unwrap().iter().all(|hop| hop.get("bind_interface").is_none()));
        Ok(())
    }

    #[test]
    fn manual_urls_become_one_node_and_hosts_are_read_back() -> Result<()> {
        let text = manual_subscription("socks5://user:p%40ss@203.0.113.5:1080")?;
        let node = subscription::parse_nodes(&text)?.remove(0);
        assert_eq!(node.name, "203.0.113.5:1080");
        assert_eq!(node_host(&node).as_deref(), Some("203.0.113.5"));
        assert!(text.contains("p@ss"));
        assert!(manual_subscription("ftp://example.com").is_err());
        assert!(manual_subscription("http://example.com/path").is_err());
        Ok(())
    }

    #[test]
    fn invalid_config_is_rejected_by_shoes() {
        assert!(validate("- address: nope\n  protocol: {type: mystery}\n").is_err());
        assert!(validate("- address: '127.0.0.1:18999'\n  protocol: {type: http}\n  dns: 'tcp://[::1]:53'\n").is_err());
    }

    #[test]
    fn embedded_listener_starts_and_stops() -> Result<()> {
        let port = free_port()?;
        let proxy = EmbeddedProxy::default();
        proxy.start(render(&socks_node().config, port)?, port)?;
        assert!(proxy.running());
        assert!(!port_free(port));
        proxy.stop();
        assert!(!proxy.running());
        assert!(port_free(port));
        Ok(())
    }

    #[test]
    fn embedded_proxy_pins_each_hop_to_ipv4_and_rejects_ipv6() -> Result<()> {
        use std::io::{Read, Write};
        use std::net::Ipv4Addr;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let upstream = listener.local_addr()?;
        let server = std::thread::spawn(move || -> std::io::Result<Vec<[u8; 4]>> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut targets = Vec::new();
            for _ in 0..2 {
                let mut prefix = [0u8; 7];
                stream.read_exact(&mut prefix)?;
                if prefix != [5, 1, 0, 5, 1, 0, 1] {
                    return Err(std::io::Error::other(format!("unexpected SOCKS request: {prefix:?}")));
                }
                let mut address = [0u8; 6];
                stream.read_exact(&mut address)?;
                targets.push(address[..4].try_into().unwrap());
                stream.write_all(&[5, 0, 5, 0, 0, 1, 127, 0, 0, 1, 0, 1])?;
            }
            Ok(targets)
        });
        let hops = [
            serde_yaml::from_str::<Yaml>(&format!("address: '{upstream}'\nprotocol: {{type: socks}}" )).unwrap(),
            serde_yaml::from_str::<Yaml>("address: 'localhost:1234'\nprotocol: {type: socks}").unwrap(),
        ];
        let port = free_port()?;
        let proxy = EmbeddedProxy::default();
        proxy.start(render_listener(&hops, port, false, None)?, port)?;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        client.set_read_timeout(Some(Duration::from_secs(4)))?;
        client.write_all(b"CONNECT localhost:8080 HTTP/1.1\r\nHost: localhost:8080\r\n\r\n")?;
        let mut response = [0u8; 128];
        let size = client.read(&mut response)?;
        assert!(String::from_utf8_lossy(&response[..size]).contains("200"));
        let targets = server.join().unwrap()?;
        assert_eq!(targets, vec![[127, 0, 0, 1], [127, 0, 0, 1]]);
        drop(client);
        proxy.stop();

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let upstream = listener.local_addr()?;
        let hop: Yaml = serde_yaml::from_str(&format!("address: '{upstream}'\nprotocol: {{type: socks}}" )).unwrap();
        let port = free_port()?;
        proxy.start(render_listener(&[hop], port, false, None)?, port)?;
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        client.set_read_timeout(Some(Duration::from_secs(3)))?;
        client.write_all(b"CONNECT [::ffff:127.0.0.1]:443 HTTP/1.1\r\nHost: [::ffff:127.0.0.1]:443\r\n\r\n")?;
        let size = client.read(&mut response).unwrap_or(0);
        assert!(!String::from_utf8_lossy(&response[..size]).contains("200"));
        assert!(listener.accept().is_err(), "IPv6 request must not reach the upstream");
        proxy.stop();
        Ok(())
    }

    #[test]
    fn catalog_import_and_lookup() -> Result<()> {
        let store = Store::in_memory()?;
        let catalog = import(
            &store,
            "home",
            None,
            Some("proxies:\n  - {name: a, type: socks5, server: h, port: 1}\n"),
            None,
        )?;
        assert_eq!(catalog.nodes.len(), 1);
        let node = find_node(&store, &catalog.nodes[0].id)?;
        assert_eq!(node.name, "a");
        assert!(import(&store, "home", None, Some("proxies: []"), None).is_err());
        Ok(())
    }
}
