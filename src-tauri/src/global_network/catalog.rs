//! The proxy catalog, chain checks and public IP lookups.
use agent_core::util::LockExt;
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use agent_core::model::{
    GeoInfo, HopCheck, IpClassification, IpType, NetworkGateMode, NetworkRoute, NodePlace,
    ProxyCatalog, RouteCheck,
};
use agent_core::proxy::{self, subscription::Node};
use agent_core::store::StoredSource;
use agent_core::util::now_millis;
use cabin_ip_detection::{Detector, DetectorBuilder, IpInfo, Service, public_ip};
use vm_core::netgw::Gateway;

use super::{GlobalNetwork, Runtime, error};
use crate::app_error::AppError;

/// What a chain check connects to through each prefix.
const CHECK_TARGET: &str = "www.google.com:443";
/// Node servers rarely move; their locations are kept this long.
const PLACE_TTL_MS: i64 = 6 * 60 * 60 * 1000;
/// A failed or location-less lookup is tried again after this long.
const PLACE_RETRY_MS: i64 = 2 * 60 * 1000;
/// Parallel lookups when locating several nodes.
const LOCATE_WORKERS: usize = 6;

fn ip_type(kind: cabin_ip_detection::IpType) -> IpType {
    use cabin_ip_detection::IpType as Detected;
    match kind {
        Detected::Mobile => IpType::Mobile,
        Detected::Residential => IpType::Residential,
        Detected::Commercial => IpType::Commercial,
        Detected::Server => IpType::Server,
    }
}

fn geo(info: IpInfo) -> GeoInfo {
    GeoInfo {
        isp: info.isp().map(str::to_string),
        ip_type: info.ip_type.map(ip_type),
        ip: info.ip.to_string(),
        country_code: info.country_code,
        continent_code: info.continent_code,
        country: info.country,
        region: info.region,
        city: info.city,
        asn: info.asn,
        latitude: info.latitude,
        longitude: info.longitude,
        timezone: info.timezone,
        source: info.source.name().into(),
        checked_at: i64::try_from(info.fetched_at_unix_ms).unwrap_or(i64::MAX),
    }
}

fn detector_builder() -> DetectorBuilder {
    Detector::builder()
        .ipv4_only()
        .timeout(Duration::from_secs(6))
        .retries(0)
}

fn detector(proxy_url: Option<&str>) -> Result<Detector, AppError> {
    let builder = detector_builder();
    match proxy_url {
        Some(url) => builder.proxy_url(url),
        None => builder.direct(),
    }
    .build()
    .map_err(|e| error(e.to_string()))
}

/// The IPv4 address a node's host stands for (resolved on the Mac).
fn resolve(host: &str) -> Result<IpAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return if ip.is_ipv4() {
            Ok(ip)
        } else {
            Err(format!("{host} has no IPv4 address"))
        };
    }
    (host, 443)
        .to_socket_addrs()
        .map_err(|e| format!("Could not resolve {host}: {e}"))?
        .map(|address| address.ip())
        .find(IpAddr::is_ipv4)
        .ok_or_else(|| format!("{host} has no IPv4 address"))
}

fn lookup_location(proxy_url: Option<&str>) -> Result<GeoInfo, AppError> {
    detector(proxy_url)?
        .detect()
        .map(geo)
        .map_err(|failure| error(failure.to_string()))
}

fn public_ipv4(value: &str) -> bool {
    value
        .parse()
        .is_ok_and(|address: IpAddr| address.is_ipv4() && public_ip(address))
}

impl GlobalNetwork {
    // ------------------------------------------------------------ catalog

    pub fn import(
        &self,
        name: &str,
        url: Option<&str>,
        contents: Option<&str>,
    ) -> Result<ProxyCatalog, AppError> {
        let via = if url.is_some() {
            self.download_proxy()?
        } else {
            None
        };
        self.with_store(|store| proxy::import(store, name, url, contents, via.as_deref()))
    }

    /// A manual proxy URL becomes a one-node source of its own.
    pub fn add_manual(&self, url: &str) -> Result<ProxyCatalog, AppError> {
        let text = proxy::manual_subscription(url)?;
        let node = proxy::subscription::parse_nodes(&text)?
            .into_iter()
            .next()
            .ok_or_else(|| error("The proxy URL did not produce a node"))?;
        self.with_store(|store| {
            proxy::import(
                store,
                &format!("Manual · {}", node.name),
                None,
                Some(&text),
                None,
            )
        })
    }

    /// Downloads use the gateway for a proxy chain or an explicitly bound
    /// interface; only an unbound direct route uses the system route.
    fn download_proxy(&self) -> Result<Option<String>, AppError> {
        let route = self.route()?;
        if !route.enabled && route.bind_interface.is_none() {
            return Ok(None);
        }
        let mut runtime = self.runtime();
        self.expire(&mut runtime);
        if !runtime.internet_allowed() {
            return Err(error(
                "Internet access is closed; refresh the source from this Mac or pass a complete check first",
            ));
        }
        runtime
            .gateway
            .as_ref()
            .and_then(Gateway::proxy_url)
            .map(Some)
            .ok_or_else(|| error("The route is unavailable; the download was not sent directly"))
    }

    /// The route's nodes that belong to source `id`.
    fn route_nodes_in(&self, id: &str) -> Result<Vec<String>, AppError> {
        let route = self.route()?;
        Ok(route
            .chain
            .into_iter()
            .filter(|node| route.enabled && node.starts_with(&format!("{id}::")))
            .collect())
    }

    pub fn refresh(&self, id: &str, automatic: bool) -> Result<ProxyCatalog, AppError> {
        if !automatic {
            let _guard = self.transaction.locked();
            let via = self.download_proxy()?;
            return self.refresh_with(id, via.as_deref());
        }

        let (source, via, path, generation) = {
            let _guard = self.transaction.locked();
            let source = self.with_store(|store| proxy::refresh_source(store, id))?;
            let mut runtime = self.runtime();
            self.expire(&mut runtime);
            if !runtime.internet_allowed() {
                return Err(error("Internet access is closed"));
            }
            let via = runtime
                .gateway
                .as_ref()
                .and_then(Gateway::proxy_url)
                .ok_or_else(|| error("The route is unavailable"))?;
            (source, via, runtime.identity(), runtime.grant.generation)
        };

        // A slow provider cannot hold up a route change or a catalog read.
        let nodes = proxy::fetch_refresh(&source, Some(&via))?;
        let _guard = self.transaction.locked();
        {
            let mut runtime = self.runtime();
            self.expire(&mut runtime);
            if !runtime.internet_allowed()
                || runtime.identity() != path
                || runtime.grant.generation != generation
            {
                return Err(error(
                    "The network changed while the subscription was being downloaded",
                ));
            }
        }
        self.save_refreshed_source(&source, &nodes)
    }

    /// Recovery for an existing subscription URL when the route itself
    /// cannot carry the download. This only updates the catalog; it never
    /// opens machine traffic or changes the saved route.
    pub fn refresh_from_host(&self, id: &str) -> Result<ProxyCatalog, AppError> {
        if !self.host_refresh_allowed() {
            return Err(error(
                "Pause internet access before refreshing a subscription from this Mac",
            ));
        }
        self.refresh_with(id, None)
    }

    fn host_refresh_allowed(&self) -> bool {
        let runtime = self.runtime();
        runtime.gateway.is_none() || runtime.grant.mode == NetworkGateMode::Closed
    }

    fn refresh_with(&self, id: &str, via: Option<&str>) -> Result<ProxyCatalog, AppError> {
        let source = self.with_store(|store| proxy::refresh_source(store, id))?;
        let nodes = proxy::fetch_refresh(&source, via)?;
        self.save_refreshed_source(&source, &nodes)
    }

    fn save_refreshed_source(
        &self,
        source: &StoredSource,
        nodes: &[Node],
    ) -> Result<ProxyCatalog, AppError> {
        let used = self.route_nodes_in(&source.id)?;
        self.with_store(|store| proxy::save_refresh(store, source, nodes, &used))
    }

    pub fn remove(&self, id: &str) -> Result<ProxyCatalog, AppError> {
        if !self.route_nodes_in(id)?.is_empty() {
            return Err(error(
                "The route uses a node from this subscription. Take it out of the route first.",
            ));
        }
        self.with_store(|store| store.delete_proxy_source(id))?;
        self.catalog()
    }

    pub fn save_node_group(
        &self,
        id: Option<&str>,
        name: &str,
        node_ids: &[String],
    ) -> Result<ProxyCatalog, AppError> {
        self.with_store(|store| proxy::nodes::save_group(store, id, name, node_ids))
    }

    pub fn remove_node_group(&self, id: &str) -> Result<ProxyCatalog, AppError> {
        self.with_store(|store| proxy::nodes::remove_group(store, id))
    }

    // ------------------------------------------------------------ checks

    pub fn test_node(
        &self,
        id: &str,
        revision: &str,
        kind: agent_core::model::ProxyTestKind,
        interface: Option<&str>,
    ) -> Result<agent_core::model::ProxyTestResult, AppError> {
        if let Some(name) = interface {
            vm_core::network_interfaces::require_available(name)
                .map_err(|e| error(e.to_string()))?;
        }
        let identity = self.runtime().identity();
        let node = self.with_store(|store| proxy::nodes::test_node(store, id, revision))?;
        let result = proxy::nodes::measure(&node, id, revision, kind, CHECK_TARGET, interface);
        if self.runtime().identity() != identity {
            return Err(error("The network changed while the node was being tested"));
        }
        if let Some(name) = interface {
            vm_core::network_interfaces::require_available(name)
                .map_err(|e| error(e.to_string()))?;
        }
        Ok(result)
    }

    /// One node on its own.
    pub fn probe(&self, id: &str) -> Result<agent_core::model::ProbeResult, AppError> {
        let route = self.route()?;
        if let Some(name) = route.bind_interface.as_deref() {
            vm_core::network_interfaces::require_available(name)
                .map_err(|e| error(e.to_string()))?;
        }
        let identity = self.runtime().identity();
        let nodes = self.nodes(&[id.to_string()])?;
        let result =
            proxy::probe_chain_on_interface(&nodes, CHECK_TARGET, route.bind_interface.as_deref())?;
        if self.runtime().identity() != identity || self.route()? != route {
            return Err(error("The network changed while the node was being tested"));
        }
        Ok(result)
    }

    /// `chain` prefix by prefix: hop 0 alone, then 0→1, and so on. The first
    /// prefix that fails names the broken link; later hops are not tried.
    pub fn check(&self, route: &NetworkRoute) -> Result<RouteCheck, AppError> {
        self.upstream(route)?;
        let identity = self.runtime().identity();
        let chain = if route.enabled { &route.chain[..] } else { &[] };
        let nodes = self.nodes(chain)?;
        let mut hops = Vec::with_capacity(nodes.len());
        let mut broken_at = None;
        for (index, id) in chain.iter().enumerate() {
            // A hop whose listener cannot even start is just as broken.
            let result = proxy::probe_chain_on_interface(
                &nodes[..=index],
                CHECK_TARGET,
                route.bind_interface.as_deref(),
            )
            .unwrap_or_else(|e| agent_core::model::ProbeResult {
                ok: false,
                latency_ms: None,
                detail: e.to_string(),
            });
            hops.push(HopCheck {
                node_id: id.clone(),
                ok: result.ok,
                latency_ms: result.latency_ms,
                detail: result.detail,
            });
            if !result.ok {
                broken_at = Some(index as u32);
                break;
            }
        }
        if self.runtime().identity() != identity {
            return Err(error(
                "The network changed while the route was being checked",
            ));
        }
        if let Some(name) = route.bind_interface.as_deref() {
            vm_core::network_interfaces::require_available(name)
                .map_err(|e| error(e.to_string()))?;
        }
        Ok(RouteCheck {
            hops,
            broken_at,
            checked_at: now_millis(),
        })
    }

    fn proxy_endpoint(runtime: &Runtime) -> Result<String, AppError> {
        runtime
            .gateway
            .as_ref()
            .and_then(Gateway::proxy_url)
            .ok_or_else(|| error("The network gateway is not running; start the machine first"))
    }

    /// Look up the saved route using the gateway's own listener. Discard
    /// results if a candidate took over or the gateway changed in flight.
    pub fn exit(&self) -> Result<GeoInfo, AppError> {
        let (identity, url) = {
            let runtime = self.runtime();
            if runtime.active_route.as_ref() != Some(&self.route()?) {
                return Err(error(
                    "Wait for the route check to finish before looking up the exit",
                ));
            }
            (runtime.identity(), Self::proxy_endpoint(&runtime)?)
        };
        let mut exit = lookup_location(Some(&url))?;
        let mut runtime = self.runtime();
        if runtime.identity() != identity {
            return Err(error(
                "The network changed while the exit was being looked up",
            ));
        }
        let ip = exit.ip.parse().map_err(|_| error("Invalid exit address"))?;
        runtime.exit_observation = Some((identity, ip));
        // The fastest answer often only has a country. Reuse the verified
        // location only when it describes the very same public address.
        if let Some(reference) = runtime.verified_samples.iter().flatten().find(|sample| {
            sample.ip == exit.ip
                && exit
                    .country_code
                    .as_ref()
                    .is_none_or(|code| sample.country_code.as_ref() == Some(code))
        }) {
            exit.country_code = exit.country_code.or_else(|| reference.country_code.clone());
            exit.country = exit.country.or_else(|| reference.country.clone());
            exit.city = exit.city.or_else(|| reference.city.clone());
            exit.region = exit.region.or_else(|| reference.region.clone());
            exit.timezone = exit.timezone.or_else(|| reference.timezone.clone());
            exit.isp = exit.isp.or_else(|| reference.isp.clone());
            exit.asn = exit.asn.or(reference.asn);
            exit.latitude = exit.latitude.or(reference.latitude);
            exit.longitude = exit.longitude.or(reference.longitude);
        }
        Ok(exit)
    }

    /// Classify the last observed exit through that same gateway path.
    pub fn exit_type(
        &self,
        requested: &str,
        refresh: bool,
    ) -> Result<Option<IpClassification>, AppError> {
        let ip = requested
            .parse::<IpAddr>()
            .map_err(|_| error("Invalid exit address"))?;
        let (identity, url) = {
            let runtime = self.runtime();
            let identity = runtime.identity();
            if runtime.exit_observation != Some((identity, ip))
                || runtime.active_route.as_ref() != Some(&self.route()?)
            {
                return Err(error(
                    "The exit changed while its IP type was being looked up",
                ));
            }
            (identity, Self::proxy_endpoint(&runtime)?)
        };
        let cached = {
            let mut cache = self.quality.locked();
            cache.retain(|_, (at, reply)| {
                at.elapsed()
                    < Duration::from_secs(if reply.is_some() { 3 * 24 * 3600 } else { 300 })
            });
            (!refresh)
                .then(|| cache.get(&ip).map(|(_, reply)| reply.clone()))
                .flatten()
        };
        if let Some(reply) = cached {
            let runtime = self.runtime();
            if runtime.identity() != identity || runtime.exit_observation != Some((identity, ip)) {
                return Err(error(
                    "The exit changed while its IP type was being looked up",
                ));
            }
            return Ok(reply);
        }
        let reply = detector(Some(&url))?
            .classify(ip)
            .map(|result| IpClassification {
                ip_type: ip_type(result.ip_type),
                inferred: result.inferred,
                organization: result.organization,
                asn: result.asn,
                source: result.source.name().into(),
            });
        let runtime = self.runtime();
        if runtime.exit_observation != Some((identity, ip)) || runtime.identity() != identity {
            return Err(error(
                "The exit changed while its IP type was being looked up",
            ));
        }
        let mut cache = self.quality.locked();
        if cache.len() >= 256
            && let Some(old) = cache
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(ip, _)| *ip)
        {
            cache.remove(&old);
        }
        cache.insert(ip, (Instant::now(), reply.clone()));
        Ok(reply)
    }

    /// Query independent location providers through the same route. Every
    /// source remains in the result, including failures and disagreements.
    pub fn exit_samples(&self) -> Result<Vec<Result<GeoInfo, String>>, AppError> {
        let url = Self::proxy_endpoint(&self.runtime())?;
        // These FlClash address providers can also report an IANA time zone.
        let services = [Service::IpSb, Service::IpWhoIs, Service::GeoJs];
        std::thread::scope(|scope| {
            let handles = services
                .into_iter()
                .map(|service| {
                    let url = &url;
                    std::thread::Builder::new()
                        .name("network-location-sample".into())
                        .spawn_scoped(scope, move || {
                            detector(Some(url))
                                .map_err(|error| error.message)?
                                .detect_service(service)
                                .map_err(|failure| failure.to_string())
                                .map(geo)
                                .and_then(|geo| {
                                    if public_ipv4(&geo.ip) {
                                        Ok(geo)
                                    } else {
                                        Err("The exit sample was not a public IPv4 address".into())
                                    }
                                })
                        })
                })
                .collect::<Vec<_>>();
            Ok(handles
                .into_iter()
                .map(|worker| {
                    worker
                        .map_err(|error| format!("Could not start a location sample: {error}"))?
                        .join()
                        .unwrap_or_else(|_| Err("The location sample thread stopped".into()))
                })
                .collect())
        })
    }

    /// Resolve the guest's exact trace address when a rotating route's other probes saw different IPs.
    pub fn locate_guest_exit(&self, ip: std::net::Ipv4Addr) -> Result<GeoInfo, AppError> {
        let url = Self::proxy_endpoint(&self.runtime())?;
        detector_builder()
            .services([Service::GeoJs, Service::IpWhoIs])
            .proxy_url(&url)
            .build()
            .map_err(|failure| error(failure.to_string()))?
            .lookup(ip.into())
            .map(geo)
            .map_err(|failure| error(failure.to_string()))
    }

    pub fn gateway_connected(&self) -> bool {
        self.runtime()
            .gateway
            .as_ref()
            .is_some_and(Gateway::connected)
    }

    /// The Mac's own public address (the start of every route).
    pub fn origin(&self) -> Result<GeoInfo, AppError> {
        let detector = detector(None)?;
        let mut info = detector
            .detect()
            .map_err(|failure| error(failure.to_string()))?;
        // The map needs coordinates. Enrich the confirmed Mac address using
        // an explicit address lookup, retaining a fast result if that fails.
        if info.latitude.zip(info.longitude).is_none()
            && let Ok(located) = detector.lookup(info.ip)
        {
            info = located;
        }
        Ok(geo(info))
    }

    /// Where each node's server is. Hosts are resolved on the Mac and looked
    /// up directly; results are cached per host.
    pub fn locate(&self, ids: &[String]) -> Result<Vec<NodePlace>, AppError> {
        let nodes = self.nodes(ids)?;
        let hosts: Vec<Option<String>> = nodes.iter().map(proxy::node_host).collect();
        let now = now_millis();
        let missing: Vec<String> = {
            let places = self.places.locked();
            let mut missing: Vec<String> = hosts
                .iter()
                .flatten()
                .filter(|host| {
                    places.get(*host).is_none_or(|(at, result)| {
                        let located = matches!(result, Ok(geo) if geo.country_code.is_some() || geo.latitude.is_some());
                        now - at > if located { PLACE_TTL_MS } else { PLACE_RETRY_MS }
                    })
                })
                .cloned()
                .collect();
            missing.sort();
            missing.dedup();
            missing
        };
        if !missing.is_empty() {
            let detector = detector(None)?;
            let next = Mutex::new(missing.iter());
            let found = Mutex::new(Vec::new());
            let locate = || {
                loop {
                    let Some(host) = next.locked().next().cloned() else {
                        break;
                    };
                    let result = resolve(&host).and_then(|ip| {
                        detector
                            .lookup(ip)
                            .map(geo)
                            .map_err(|e| format!("Could not locate {ip}: {e}"))
                    });
                    found.locked().push((host, result));
                }
            };
            let stopped = std::thread::scope(|scope| {
                let workers: Vec<_> = (1..LOCATE_WORKERS.min(missing.len()))
                    .filter_map(|_| {
                        std::thread::Builder::new()
                            .name("network-node-location".into())
                            .spawn_scoped(scope, locate)
                            .ok()
                    })
                    .collect();
                // The caller also drains the queue if worker creation fails.
                locate();
                let mut stopped = false;
                for worker in workers {
                    stopped |= worker.join().is_err();
                }
                stopped
            });
            if stopped {
                return Err(error("A node location worker stopped"));
            }
            let mut places = self.places.locked();
            for (host, result) in found
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
            {
                places.insert(host, (now, result));
            }
        }
        let places = self.places.locked();
        Ok(ids
            .iter()
            .zip(hosts)
            .map(|(id, host)| {
                let Some(host) = host else {
                    return NodePlace {
                        node_id: id.clone(),
                        host: String::new(),
                        geo: None,
                        error: Some("The node has no server address".into()),
                    };
                };
                let (geo, error) = match places.get(&host).map(|(_, result)| result.clone()) {
                    Some(Ok(geo)) => (Some(geo), None),
                    Some(Err(message)) => (None, Some(message)),
                    None => (None, Some("Not looked up".into())),
                };
                NodePlace {
                    node_id: id.clone(),
                    host,
                    geo,
                    error,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_network::test_support::*;
    use agent_core::store::Store;
    use vm_core::netgw::Upstream;

    #[test]
    fn a_chain_route_survives_restart_and_protects_its_subscription() {
        let dir = temp("route");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        let route = NetworkRoute {
            enabled: true,
            chain: catalog.nodes.iter().map(|n| n.id.clone()).collect(),
            bind_interface: None,
            ..NetworkRoute::default()
        };
        match network.upstream(&route).unwrap() {
            Upstream::Chain(hops) => assert_eq!(hops.len(), 2),
            _ => panic!("an enabled route must use the configured chain"),
        }
        network.apply(&route).unwrap();
        assert!(
            network.download_proxy().is_err(),
            "a stopped route must not fall back to direct downloads"
        );
        assert!(network.remove(&catalog.sources[0].id).is_err());
        drop(network);
        let network = GlobalNetwork::open(&dir).unwrap();
        assert_eq!(network.route().unwrap(), route);
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_chain_check_stops_at_the_first_broken_link() {
        let dir = temp("check");
        let network = GlobalNetwork::open(&dir).unwrap();
        // Nothing listens on these ports, so the entry already fails.
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        let chain: Vec<String> = catalog.nodes.iter().map(|n| n.id.clone()).collect();
        let check = network
            .check(&NetworkRoute {
                enabled: true,
                chain,
                ..NetworkRoute::default()
            })
            .unwrap();
        assert_eq!(check.broken_at, Some(0));
        assert_eq!(
            check.hops.len(),
            1,
            "hops after a broken link are not tried"
        );
        assert!(!check.hops[0].ok);
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn bound_direct_subscription_downloads_require_gateway_and_gate() {
        let dir = temp("bd");
        let network = GlobalNetwork::open(&dir).unwrap();
        assert_eq!(network.download_proxy().unwrap(), None);
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        let mut source = network.with_store(Store::proxy_sources).unwrap().remove(0);
        let (url, server) = serve_once(TWO);
        source.url = Some(url.clone());
        network
            .with_store(|store| store.upsert_proxy_source(&source))
            .unwrap();
        let bound = NetworkRoute {
            bind_interface: Some("en-test".into()),
            ..NetworkRoute::default()
        };
        network.save(&bound).unwrap();
        assert!(
            network.download_proxy().is_err(),
            "a missing gateway cannot fall back to direct"
        );

        // Start a local gateway and model the active bound route without using
        // a real host interface or making external network requests.
        network
            .start_with(&dir.join("gateway.sock"), 0, &NetworkRoute::default())
            .unwrap();
        network.runtime().active_route = Some(bound.clone());
        assert!(
            network.download_proxy().is_err(),
            "a closed gate blocks downloads"
        );
        assert!(network.import("blocked", Some(&url), None).is_err());
        assert!(network.refresh(&catalog.sources[0].id, true).is_err());
        open_gate(&network, &bound);
        let gateway_url = network
            .runtime()
            .gateway
            .as_ref()
            .unwrap()
            .proxy_url()
            .unwrap();
        assert_eq!(network.download_proxy().unwrap(), Some(gateway_url));
        assert!(network.refresh_from_host(&catalog.sources[0].id).is_err());

        network.invalidate_gate("test closed gate");
        let refreshed = network.refresh_from_host(&catalog.sources[0].id).unwrap();
        assert_eq!(refreshed.nodes.len(), 2);
        server.join().unwrap();
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn refresh_from_host_is_rejected_while_a_grant_is_open() {
        let dir = temp("host-refresh-open");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        let mut source = network
            .with_store(Store::proxy_sources)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        source.url = Some("http://127.0.0.1:9/unused".into());
        network
            .with_store(|store| store.upsert_proxy_source(&source))
            .unwrap();
        let route = NetworkRoute {
            enabled: true,
            chain: vec![catalog.nodes[0].id.clone()],
            bind_interface: None,
            ..NetworkRoute::default()
        };
        network.save(&route).unwrap();
        network.start(&dir.join("gateway.sock"), 0).unwrap();
        open_gate(&network, &route);
        let before = network.gate_snapshot();

        let failure = network
            .refresh_from_host(&catalog.sources[0].id)
            .unwrap_err();
        assert!(failure.message.contains("Pause internet access"));
        assert_eq!(network.gate_snapshot(), before);
        assert_eq!(network.route().unwrap(), route);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn host_refresh_rejects_changes_to_a_routed_node_before_saving() {
        let dir = temp("host-refresh-change");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        let route = NetworkRoute {
            enabled: true,
            chain: vec![catalog.nodes[0].id.clone()],
            bind_interface: None,
            ..NetworkRoute::default()
        };
        network.save(&route).unwrap();
        let old_nodes = network.nodes(&route.chain).unwrap();
        let mut source = network.with_store(Store::proxy_sources).unwrap().remove(0);
        let changed = "proxies:\n  - {name: entry, type: socks5, server: 127.0.0.3, port: 1080}\n";
        let (url, server) = serve_once(changed);
        source.url = Some(url);
        network
            .with_store(|store| store.upsert_proxy_source(&source))
            .unwrap();
        let gate_before = network.gate_snapshot();

        let failure = network
            .refresh_from_host(&catalog.sources[0].id)
            .unwrap_err();
        server.join().unwrap();
        assert!(failure.message.contains("changed or disappeared"));
        assert_eq!(network.route().unwrap(), route);
        assert_eq!(network.nodes(&route.chain).unwrap(), old_nodes);
        assert_eq!(network.gate_snapshot(), gate_before);
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn host_refresh_updates_an_unused_source_without_touching_the_gate() {
        let dir = temp("host-refresh-source");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        let mut source = network.with_store(Store::proxy_sources).unwrap().remove(0);
        let (url, server) = serve_once(TWO);
        source.url = Some(url);
        network
            .with_store(|store| store.upsert_proxy_source(&source))
            .unwrap();
        let gate_before = network.gate_snapshot();

        let refreshed = network.refresh_from_host(&catalog.sources[0].id).unwrap();
        server.join().unwrap();
        assert_eq!(refreshed.nodes.len(), 2);
        assert_eq!(network.route().unwrap(), NetworkRoute::default());
        assert_eq!(network.gate_snapshot(), gate_before);
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn manual_proxies_are_nodes_and_unknown_hosts_are_reported() {
        let dir = temp("manual");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.add_manual("socks5://127.0.0.1:1080").unwrap();
        assert_eq!(catalog.sources[0].name, "Manual · 127.0.0.1:1080");
        assert!(network.add_manual("ftp://example.com").is_err());
        assert!(resolve("203.0.113.4").is_ok_and(|ip| ip.to_string() == "203.0.113.4"));
        assert!(resolve("2001:db8::1").is_err());
        assert!(resolve("::ffff:203.0.113.4").is_err());
        assert!(resolve("no-such-host.invalid").is_err());
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }
}
