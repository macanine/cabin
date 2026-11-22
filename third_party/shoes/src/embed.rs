//! Embedding entry points for desktop hosts.
//!
//! Added by Cabin on top of upstream shoes 0.3.2 (see VENDORED.txt).
//! Upstream only exposes a TUN-oriented entry point for mobile FFI; desktop
//! hosts need plain listeners (HTTP/SOCKS/mixed) started in-process.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

#[cfg(unix)]
pub use crate::tun::{RelayGate, RelayTicket};

use crate::config::{
    BindLocation, ClientChainHop, Config, ConfigSelection, RuleActionConfig, ServerProxyConfig,
    ValidatedConfigs, convert_cert_paths, create_server_configs,
};
use crate::dns::build_dns_registry;
use crate::resolver::Resolver;
use crate::tcp::tcp_client_handler_factory::create_tcp_client_proxy_selector;
use crate::tcp::tcp_handler::TcpServerHandler;
use crate::tcp::tcp_server::{process_stream, start_servers};
use crate::tcp::tcp_server_handler_factory::create_tcp_server_handler;

/// Explicit DNS bootstrap for an interface-bound embedded proxy.
///
/// Servers are IP literals, queried over cleartext UDP through `interface`.
/// The caller chooses servers reachable on that interface. No system resolver
/// fallback is used. Servers are tried in order, each with a two-second timeout
/// and one attempt, so a lookup can take up to two seconds per server.
#[derive(Clone, Debug)]
pub struct BoundDns {
    interface: String,
    servers: Vec<SocketAddr>,
}

impl BoundDns {
    pub fn new(interface: String, servers: Vec<SocketAddr>) -> std::io::Result<Self> {
        crate::socket_util::validate_interface(&interface)?;
        if servers.is_empty() || servers.iter().any(|server| {
            server.port() == 0 || server.ip().is_unspecified() || server.ip().is_multicast()
                || matches!(server, SocketAddr::V6(v6) if v6.scope_id() != 0 || v6.ip().is_unicast_link_local())
        }) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput,
                "bound DNS requires usable IP-literal servers with nonzero ports; IPv6 link-local servers are unsupported"));
        }
        Ok(Self { interface, servers })
    }

    fn resolver(&self) -> std::io::Result<Arc<dyn Resolver>> {
        use crate::client_proxy_chain::{ClientChainGroup, ClientProxyChain, InitialHopEntry};
        use crate::dns::IpStrategy;
        use crate::dns::composite_resolver::CompositeResolver;
        use crate::dns::hickory_resolver::{HickoryResolver, HickoryResolverOptions};
        use crate::tcp::socket_connector_impl::SocketConnectorImpl;

        let socket = SocketConnectorImpl::new_tcp(Some(self.interface.clone()), true);
        let chain = ClientProxyChain::new(vec![InitialHopEntry::Direct(Box::new(socket))], vec![]);
        let group = Arc::new(ClientChainGroup::new(vec![chain]));
        // All upstreams are IP literals; any accidental bootstrap lookup must fail closed.
        let bootstrap: Arc<dyn Resolver> = Arc::new(NoBootstrapResolver);
        let options = HickoryResolverOptions {
            ip_strategy: IpStrategy::Ipv4Only,
            request_timeout: Some(Duration::from_secs(2)),
            connect_timeout: Duration::from_secs(2),
            attempts: 1,
            use_hosts_file: false,
        };
        // A single Hickory pool shares one deadline across its parallel servers;
        // build one resolver per server so every configured fallback gets a turn.
        let resolvers = self
            .servers
            .iter()
            .map(|server| {
                HickoryResolver::udp(*server, group.clone(), bootstrap.clone(), options).map(
                    |resolver| {
                        Arc::new(crate::resolver::TimeoutResolver::with_timeout(
                            resolver,
                            Duration::from_secs(2),
                        )) as Arc<dyn Resolver>
                    },
                )
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(crate::resolver::ipv4_only(Arc::new(
            CompositeResolver::new(resolvers),
        )))
    }
}

#[derive(Debug)]
struct NoBootstrapResolver;

impl Resolver for NoBootstrapResolver {
    fn resolve_location(
        &self,
        _: &crate::address::NetLocation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = std::io::Result<Vec<SocketAddr>>> + Send>>
    {
        Box::pin(async {
            Err(std::io::Error::other(
                "bound DNS bootstrap cannot resolve hostnames",
            ))
        })
    }
}

// Validated rules have their groups expanded. Every selectable first hop must
// use the same interface; an implicit direct chain would otherwise escape it.
fn validate_binding(configs: &[Config], bound: Option<&BoundDns>) -> std::io::Result<()> {
    for config in configs {
        let rules = match config {
            Config::Server(server) => {
                if bound.is_some() {
                    if !matches!(
                        server.protocol,
                        ServerProxyConfig::Http { .. }
                            | ServerProxyConfig::Socks { .. }
                            | ServerProxyConfig::Mixed { .. }
                    ) || !server.transport.is_default()
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::Unsupported,
                            "bound DNS supports embedded HTTP, SOCKS and mixed TCP listeners only",
                        ));
                    }
                    if let BindLocation::Address(addresses) = &server.bind_location {
                        if addresses.iter().any(|address| !address.is_ip_literal()) {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "bound DNS requires IP-literal listener addresses",
                            ));
                        }
                    }
                }
                &server.rules
            }
            Config::TunServer(tun) => &tun.rules,
            _ => continue,
        };
        for selection in rules.iter() {
            let ConfigSelection::Config(rule) = selection else {
                return Err(std::io::Error::other("unexpanded rule"));
            };
            let RuleActionConfig::Allow { client_chains, .. } = &rule.action else {
                continue;
            };
            if client_chains.is_empty() && bound.is_some() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "bound DNS requires bind_interface on every first hop",
                ));
            }
            for chain in client_chains.iter() {
                for (index, hop) in chain.hops.iter().enumerate() {
                    let selections: Vec<_> = match hop {
                        ClientChainHop::Single(selection) => vec![selection],
                        ClientChainHop::Pool(selections) => selections.iter().collect(),
                    };
                    for selection in selections {
                        let ConfigSelection::Config(client) = selection else {
                            return Err(std::io::Error::other("unexpanded proxy group"));
                        };
                        let interface = match &client.bind_interface {
                            crate::option_util::NoneOrOne::One(name) => Some(name.as_str()),
                            _ => None,
                        };
                        if bound.is_none() && interface.is_some() {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "bind_interface requires explicit BoundDns in embedded mode",
                            ));
                        }
                        if let Some(bound) = bound {
                            if (index == 0 && interface != Some(bound.interface.as_str()))
                                || (index > 0 && interface.is_some())
                            {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::InvalidInput,
                                    "every first hop must bind to the BoundDns interface; later hops must not specify bind_interface",
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn reject_external_dns(configs: &[Config], has_groups: bool) -> std::io::Result<()> {
    if has_groups
        || configs.iter().any(|config| match config {
            Config::Server(server) => server.dns.is_some(),
            Config::TunServer(tun) => tun.dns.is_some(),
            _ => false,
        })
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Custom DNS is unavailable in the IPv4-only embedded proxy",
        ));
    }
    Ok(())
}

fn reject_tun(configs: &[Config]) -> std::io::Result<()> {
    #[cfg(unix)]
    if configs
        .iter()
        .any(|config| matches!(config, Config::TunServer(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "TUN configs are not supported by the embedded entry point",
        ));
    }
    let _ = configs;
    Ok(())
}

fn parse(config_yaml: &str) -> std::io::Result<Vec<Config>> {
    serde_yaml::from_str::<Vec<Config>>(config_yaml).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Could not parse config string as config YAML: {error}"),
        )
    })
}

async fn load(config_yaml: &str) -> std::io::Result<ValidatedConfigs> {
    let configs = parse(config_yaml)?;
    let (configs, _) = convert_cert_paths(configs).await?;
    let validated = create_server_configs(configs)?;
    reject_tun(&validated.configs)?;
    reject_external_dns(&validated.configs, !validated.dns_groups.is_empty())?;
    Ok(validated)
}

/// Parse and validate a configuration without binding any socket.
pub async fn validate(config_yaml: &str) -> std::io::Result<()> {
    let validated = load(config_yaml).await?;
    validate_binding(&validated.configs, None)
}

/// Validate a listener config for the explicitly selected interface.
pub async fn validate_with_bound_dns(config_yaml: &str, bound: &BoundDns) -> std::io::Result<()> {
    let validated = load(config_yaml).await?;
    validate_binding(&validated.configs, Some(bound))
}

/// Start every server in `config_yaml` and run until `shutdown` fires
/// (or its sender is dropped). Listener tasks are aborted on return.
pub async fn run(config_yaml: &str, shutdown: oneshot::Receiver<()>) -> std::io::Result<()> {
    run_inner(config_yaml, shutdown, None).await
}

/// Run embedded listeners with DNS and all first-hop sockets on one interface.
pub async fn run_with_bound_dns(
    config_yaml: &str,
    shutdown: oneshot::Receiver<()>,
    bound: BoundDns,
) -> std::io::Result<()> {
    run_inner(config_yaml, shutdown, Some(bound)).await
}

async fn run_inner(
    config_yaml: &str,
    shutdown: oneshot::Receiver<()>,
    bound: Option<BoundDns>,
) -> std::io::Result<()> {
    let ValidatedConfigs {
        configs,
        dns_groups,
        global_limits,
    } = load(config_yaml).await?;
    validate_binding(&configs, bound.as_ref())?;
    let bound_resolver = bound.as_ref().map(BoundDns::resolver).transpose()?;
    crate::resources::configure(global_limits)?;
    let mut dns_registry = build_dns_registry(dns_groups).await?;

    let mut handles = Vec::new();
    let result = async {
        for config in configs {
            let dns = match &config {
                Config::Server(server) => server.dns.as_ref(),
                _ => None,
            };
            let resolver = match &bound_resolver {
                Some(resolver) => resolver.clone(),
                None => crate::resolver::ipv4_only(dns_registry.get_for_server(dns)),
            };
            handles.extend(start_servers(config, resolver).await?);
        }
        Ok::<(), std::io::Error>(())
    }
    .await;
    if result.is_ok() {
        let _ = shutdown.await;
    }
    for handle in &handles {
        handle.abort();
    }
    result
}

/// A single embedded HTTP proxy whose accepted sockets are owned by the caller.
/// Dropping a `relay` future cancels setup and stops forwarding that socket.
#[derive(Clone)]
pub struct HttpRelay {
    handler: Arc<dyn TcpServerHandler>,
    resolver: Arc<dyn Resolver>,
}

impl HttpRelay {
    pub async fn new(config_yaml: &str) -> std::io::Result<Self> {
        Self::new_inner(config_yaml, None).await
    }

    /// Construct a relay whose DNS and first-hop sockets use the specified interface.
    pub async fn new_with_bound_dns(config_yaml: &str, bound: BoundDns) -> std::io::Result<Self> {
        Self::new_inner(config_yaml, Some(bound)).await
    }

    async fn new_inner(config_yaml: &str, bound: Option<BoundDns>) -> std::io::Result<Self> {
        let ValidatedConfigs {
            configs,
            dns_groups,
            global_limits,
        } = load(config_yaml).await?;
        validate_binding(&configs, bound.as_ref())?;
        let [Config::Server(config)] = configs.as_slice() else {
            return Err(std::io::Error::other("expected one HTTP proxy"));
        };
        if !matches!(config.protocol, ServerProxyConfig::Http { .. })
            || !config.transport.is_default()
        {
            return Err(std::io::Error::other("expected an HTTP proxy over TCP"));
        }
        crate::resources::configure(global_limits)?;
        let resolver = match bound {
            Some(bound) => bound.resolver()?,
            None => crate::resolver::ipv4_only(
                build_dns_registry(dns_groups)
                    .await?
                    .get_for_server(config.dns.as_ref()),
            ),
        };
        let rules = config
            .rules
            .clone()
            .map(ConfigSelection::unwrap_config)
            .into_vec();
        let selector = Arc::new(create_tcp_client_proxy_selector(rules, resolver.clone()));
        let handler =
            create_tcp_server_handler(config.protocol.clone(), &selector, &resolver, None).into();
        Ok(Self { handler, resolver })
    }

    pub async fn relay(&self, stream: tokio::net::TcpStream) -> std::io::Result<()> {
        let _permit = crate::resources::try_connection(Some(stream.peer_addr()?.ip()))
            .ok_or_else(crate::resources::exhausted)?;
        process_stream(stream, self.handler.clone(), self.resolver.clone()).await
    }
}

/// Run the single TUN config in `config_yaml` on a packet descriptor the host
/// owns (its `device_fd`; never closed here) until `shutdown` fires. Packets
/// are raw IP without any header; `packet_information` must be false.
#[cfg(unix)]
pub async fn run_tun(config_yaml: &str, shutdown: oneshot::Receiver<()>) -> std::io::Result<()> {
    run_tun_controlled(config_yaml, shutdown, RelayGate::default()).await
}

/// Like `run_tun`, with a host-controlled pause barrier for packet and relay I/O.
#[cfg(unix)]
pub async fn run_tun_controlled(
    config_yaml: &str,
    shutdown: oneshot::Receiver<()>,
    gate: RelayGate,
) -> std::io::Result<()> {
    run_tun_inner(config_yaml, shutdown, gate, None).await
}

/// Run a controlled TUN relay with DNS and first-hop sockets bound to one interface.
#[cfg(unix)]
pub async fn run_tun_controlled_with_bound_dns(
    config_yaml: &str,
    shutdown: oneshot::Receiver<()>,
    gate: RelayGate,
    bound: BoundDns,
) -> std::io::Result<()> {
    run_tun_inner(config_yaml, shutdown, gate, Some(bound)).await
}

#[cfg(unix)]
async fn run_tun_inner(
    config_yaml: &str,
    shutdown: oneshot::Receiver<()>,
    gate: RelayGate,
    bound: Option<BoundDns>,
) -> std::io::Result<()> {
    let configs = parse(config_yaml)?;
    let (configs, _) = convert_cert_paths(configs).await?;
    let ValidatedConfigs {
        configs,
        dns_groups,
        global_limits,
    } = create_server_configs(configs)?;
    reject_external_dns(&configs, !dns_groups.is_empty())?;
    validate_binding(&configs, bound.as_ref())?;
    let mut tun = None;
    for config in configs {
        match config {
            Config::TunServer(config) if tun.is_none() => tun = Some(config),
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "run_tun takes exactly one TUN config and nothing else",
                ));
            }
        }
    }
    let tun = tun.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no TUN config given")
    })?;
    if tun.device_fd.is_none() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the TUN config needs device_fd",
        ));
    }
    crate::resources::configure(global_limits)?;
    let resolver = match bound {
        Some(bound) => bound.resolver()?,
        None => {
            let mut dns_registry = build_dns_registry(dns_groups).await?;
            crate::resolver::ipv4_only(dns_registry.get_for_server(None))
        }
    };
    crate::tun::run_tun_from_config_controlled(tun, shutdown, false, resolver, gate).await
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::time::timeout;

    fn loopback() -> &'static str {
        if cfg!(target_os = "macos") {
            "lo0"
        } else {
            "lo"
        }
    }

    fn config(interface: Option<&str>, proxy_port: u16) -> String {
        let binding = interface
            .map(|name| format!("        bind_interface: {name}\n"))
            .unwrap_or_default();
        format!(
            "- address: 127.0.0.1:0\n  protocol:\n    type: http\n  rules:\n    - masks: ['0.0.0.0/0']\n      client_chain:\n      - address: first.test:{proxy_port}\n{binding}        protocol:\n          type: socks\n      - address: second.test:1080\n        protocol:\n          type: socks\n"
        )
    }

    async fn dns_server() -> (
        SocketAddr,
        tokio::sync::mpsc::UnboundedReceiver<String>,
        tokio::task::JoinHandle<()>,
    ) {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            loop {
                let (len, peer) = socket.recv_from(&mut buf).await.unwrap();
                if len < 17 {
                    continue;
                }
                let mut cursor = 12;
                let mut parts = Vec::new();
                while cursor < len && buf[cursor] != 0 {
                    let size = buf[cursor] as usize;
                    cursor += 1;
                    if cursor + size > len {
                        break;
                    }
                    parts.push(String::from_utf8_lossy(&buf[cursor..cursor + size]).to_string());
                    cursor += size;
                }
                if cursor + 5 > len {
                    continue;
                }
                cursor += 5;
                tx.send(parts.join(".")).ok();
                let mut response = buf[..cursor].to_vec();
                response[2] = 0x81;
                response[3] = 0x80;
                response[6..8].copy_from_slice(&1u16.to_be_bytes());
                response[8..12].fill(0);
                response
                    .extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 30, 0, 4, 127, 0, 0, 1]);
                socket.send_to(&response, peer).await.unwrap();
            }
        });
        (addr, rx, handle)
    }

    #[tokio::test]
    async fn bound_dns_resolves_all_hops_and_destination_without_system_dns() {
        let (dns_addr, mut queries, dns_task) = dns_server().await;
        let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let proxy_task = tokio::spawn(async move {
            let (mut socket, _) = proxy.accept().await.unwrap();
            for _ in 0..2 {
                let mut greeting = [0u8; 3];
                socket.read_exact(&mut greeting).await.unwrap();
                assert_eq!(greeting, [5, 1, 0]);
                let mut request = [0u8; 10];
                socket.read_exact(&mut request).await.unwrap();
                assert_eq!(
                    &request[..4],
                    &[5, 1, 0, 1],
                    "proxy must receive an IPv4 literal"
                );
                assert_eq!(&request[4..8], &[127, 0, 0, 1]);
                socket
                    .write_all(&[5, 0, 5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let bound = BoundDns::new(loopback().into(), vec![dns_addr]).unwrap();
        let yaml = config(Some(loopback()), proxy_port);
        validate_with_bound_dns(&yaml, &bound).await.unwrap();
        assert!(
            validate(&yaml)
                .await
                .unwrap_err()
                .to_string()
                .contains("BoundDns")
        );
        assert!(
            validate_with_bound_dns(&config(None, proxy_port), &bound)
                .await
                .is_err()
        );
        let relay = HttpRelay::new_with_bound_dns(&yaml, bound).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server, _) = listener.accept().await.unwrap();
        let relay_task = tokio::spawn(async move { relay.relay(server).await });
        client
            .write_all(
                b"CONNECT destination.test:443 HTTP/1.1\r\nHost: destination.test:443\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = [0u8; 1024];
        let n = timeout(Duration::from_secs(5), client.read(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&response[..n]).contains("200"));
        proxy_task.await.unwrap();
        let mut seen = HashSet::new();
        while let Ok(name) = queries.try_recv() {
            seen.insert(name);
        }
        for name in ["first.test", "second.test", "destination.test"] {
            assert!(seen.contains(name), "missing {name}: {seen:?}");
        }
        relay_task.abort();
        dns_task.abort();
    }

    #[tokio::test]
    async fn bound_dns_failure_does_not_fall_back_to_system_resolver() {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bound = BoundDns::new(loopback().into(), vec![socket.local_addr().unwrap()]).unwrap();
        let resolver = bound.resolver().unwrap();
        let location = crate::address::NetLocation::from_str("unanswered.test:80", None).unwrap();
        let lookup = timeout(Duration::from_secs(5), resolver.resolve_location(&location));
        let received = async {
            let mut buf = [0; 1500];
            timeout(Duration::from_secs(3), socket.recv_from(&mut buf))
                .await
                .unwrap()
                .unwrap();
        };
        let (result, ()) = tokio::join!(lookup, received);
        assert!(
            result.unwrap().is_err(),
            "an unanswered query must not fall back to the system resolver"
        );
    }

    #[tokio::test]
    async fn bound_dns_reaches_third_server_after_two_silent_timeouts() {
        let first = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let second = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (third, mut queries, task) = dns_server().await;
        let bound = BoundDns::new(
            loopback().into(),
            vec![
                first.local_addr().unwrap(),
                second.local_addr().unwrap(),
                third,
            ],
        )
        .unwrap();
        let resolver = bound.resolver().unwrap();
        let location = crate::address::NetLocation::from_str("third.test:443", None).unwrap();
        let started = std::time::Instant::now();
        let result = timeout(Duration::from_secs(8), resolver.resolve_location(&location))
            .await
            .unwrap();
        assert!(
            started.elapsed() >= Duration::from_millis(3500),
            "both silent servers must time out before the third is tried"
        );
        let mut buf = [0u8; 1500];
        assert!(
            first.try_recv_from(&mut buf).is_ok(),
            "first server must receive a query"
        );
        assert!(
            second.try_recv_from(&mut buf).is_ok(),
            "second server must receive a query"
        );
        assert_eq!(queries.try_recv().unwrap(), "third.test");
        assert_eq!(result.unwrap()[0], "127.0.0.1:443".parse().unwrap());
        task.abort();
    }

    #[tokio::test]
    async fn bound_dns_can_use_multiple_literal_servers() {
        let (dns_addr, mut queries, task) = dns_server().await;
        let unused = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let dead_addr = unused.local_addr().unwrap();
        drop(unused);
        let bound = BoundDns::new(loopback().into(), vec![dead_addr, dns_addr]).unwrap();
        let resolver = bound.resolver().unwrap();
        let location = crate::address::NetLocation::from_str("backup.test:443", None).unwrap();
        let result = timeout(Duration::from_secs(5), resolver.resolve_location(&location))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result[0], "127.0.0.1:443".parse().unwrap());
        assert_eq!(queries.recv().await.unwrap(), "backup.test");
        task.abort();
    }
}
