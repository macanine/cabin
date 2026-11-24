//! The Mac-owned network: the node catalog, the VM's route and its gateway.
//!
//! The VM's only NIC is connected to shoes on the Mac (`vm_core::netgw`).
//! The route is either off (shoes sends the VM straight out, still cut off
//! from the LAN and the Mac) or a chain of catalog nodes, entry first. This
//! module also answers what the network dashboard shows: a chain checked
//! hop by hop, the exit's location (looked up through the gateway's own
//! listener, so it is exactly what the VM gets), the Mac's own location and
//! where each node's server is. Lookups use public IP services.
use agent_core::util::LockExt;
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use agent_core::model::{
    GeoInfo, IpClassification, NetworkCheckReport, NetworkConnectivity, NetworkGateMode,
    NetworkHistoryEntry, NetworkRoute, NetworkStatus, NetworkTraffic, ProxyCatalog,
};
use agent_core::proxy::{self, subscription::Node};
use agent_core::store::Store;
use agent_core::util::now_millis;
use vm_core::netgw::{EgressMode, Gateway, Upstream};
use vm_core::network_interfaces::{self, InterfacePath};

use crate::app_error::AppError;
use crate::network_gate::{GateState, GateToken, route_revision};
use crate::network_health::{Connectivity, HoldCheck, traffic_sample};

mod catalog;
mod gate;
mod exit_policy;
pub(crate) use exit_policy::ExitBaseline;
mod health;
mod paths;
#[cfg(test)]
mod test_support;

const ROUTE: &str = "network_route";
const PAUSED: &str = "network_paused";
const AUTO_RECOVER: &str = "network_auto_recover";
const REGION_FOLLOWS_EXIT: &str = "network_region_follows_exit";

type PlaceCache = HashMap<String, (i64, Result<GeoInfo, String>)>;
type QualityCache = HashMap<IpAddr, (Instant, Option<IpClassification>)>;

fn error(message: impl Into<String>) -> AppError {
    AppError {
        code: "network".into(),
        message: message.into(),
        details: None,
    }
}

#[derive(Default)]
struct Runtime {
    gateway: Option<Gateway>,
    failure: Option<String>,
    active_route: Option<NetworkRoute>,
    interface_paths: BTreeMap<String, InterfacePath>,
    grant: GateState,
    connectivity: Connectivity,
    hold_timeout_pending: bool,
    installing: bool,
    sleeping: bool,
    gateway_session: u64,
    upstream_revision: u64,
    path_revision: u64,
    hard_block_revision: u64,
    verified_exit: Option<Ipv4Addr>,
    exit_policy_error: Option<(String, String)>,
    verified_samples: Vec<Option<GeoInfo>>,
    exit_observation: Option<(PathIdentity, IpAddr)>,
    traffic: Option<NetworkTraffic>,
}

impl Runtime {
    fn apply_policy(&self) {
        if let Some(gateway) = &self.gateway {
            let mode = match (self.sleeping, self.installing, self.grant.mode, self.connectivity.mode) {
                (true, _, _, _) => EgressMode::Closed,
                (_, true, _, _) => EgressMode::Install,
                (_, _, NetworkGateMode::Checking, _) => EgressMode::checking(),
                (_, _, NetworkGateMode::Open, NetworkConnectivity::Normal) => EgressMode::Open,
                (_, _, NetworkGateMode::Open, NetworkConnectivity::Rechecking) => {
                    match self.connectivity.check {
                        Some(HoldCheck::Guest | HoldCheck::Verification) => EgressMode::Rechecking,
                        _ => EgressMode::Suspended,
                    }
                }
                _ => EgressMode::Closed,
            };
            gateway.set_egress_mode(mode);
        }
    }

    fn clear_upstream(&mut self) {
        if let Some(gateway) = &self.gateway {
            gateway.clear_upstream();
        }
        self.active_route = None;
        self.upstream_revision += 1;
        self.verified_exit = None;
        self.verified_samples.clear();
        self.exit_observation = None;
    }

    fn identity(&self) -> PathIdentity {
        PathIdentity {
            gateway: self.gateway_session,
            upstream: self.upstream_revision,
            path: self.path_revision,
        }
    }

    fn internet_allowed(&self) -> bool {
        self.grant.mode == NetworkGateMode::Open
            && self.connectivity.mode == NetworkConnectivity::Normal
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct PathIdentity {
    gateway: u64,
    upstream: u64,
    path: u64,
}

pub struct CheckAttempt {
    path: PathIdentity,
    token: GateToken,
    pub refresh: bool,
    held: bool,
}

impl CheckAttempt {
    pub fn checks_closed_boundary(&self) -> bool {
        !self.refresh || self.held
    }
}

pub struct RollbackPoint {
    gateway_session: u64,
    path_revision: u64,
    hard_block_revision: u64,
    grant: GateState,
}

pub struct HealthTarget {
    identity: PathIdentity,
    generation: u64,
    pub check: Option<HoldCheck>,
    pub proxy_url: Result<String, String>,
}

impl HealthTarget {
    fn is_current(&self, runtime: &Runtime) -> bool {
        self.identity == runtime.identity()
            && self.generation == runtime.grant.generation
            && self.check == runtime.connectivity.check
    }
}

#[derive(Default)]
pub struct HealthChange {
    pub changed: bool,
    pub hard_block: bool,
}

pub struct GlobalNetwork {
    store: Mutex<Store>,
    directory: PathBuf,
    // The policy and its publication to the gateway share this lock. A delayed
    // verifier cannot publish a policy computed before a newer transition.
    runtime: Mutex<Runtime>,
    health_wake: Mutex<Option<SyncSender<()>>>,
    /// Node server locations by host: (looked up at, result).
    places: Mutex<PlaceCache>,
    quality: Mutex<QualityCache>,
    /// Serialize changes across the host and guest transaction.
    pub transaction: Mutex<()>,
}

impl GlobalNetwork {
    pub fn open(dir: &Path) -> Result<Self, AppError> {
        std::fs::create_dir_all(dir).map_err(|e| error(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| error(e.to_string()))?;
        }
        let store = Store::open(&dir.join("network.sqlite3"))?;
        let paused = store.setting::<bool>(PAUSED)?.unwrap_or(false);
        let auto_recover = store.setting::<bool>(AUTO_RECOVER)?.unwrap_or(true);
        Ok(Self {
            store: Mutex::new(store),
            directory: dir.to_path_buf(),
            runtime: Mutex::new(Runtime {
                grant: GateState::with_preferences(paused, auto_recover),
                ..Runtime::default()
            }),
            health_wake: Mutex::new(None),
            places: Mutex::new(HashMap::new()),
            quality: Mutex::new(HashMap::new()),
            transaction: Mutex::new(()),
        })
    }

    fn with_store<T>(
        &self,
        f: impl FnOnce(&Store) -> agent_core::Result<T>,
    ) -> Result<T, AppError> {
        let store = self.store.locked();
        Ok(f(&store)?)
    }

    fn runtime(&self) -> MutexGuard<'_, Runtime> {
        self.runtime.locked()
    }

    fn journal(
        &self,
        runtime: &Runtime,
        kind: &str,
        message: &str,
        report: Option<&NetworkCheckReport>,
    ) {
        if let Err(error) = self.with_store(|store| {
            store.append_network_history(kind, message, runtime.connectivity.failed_checks, report)
        }) {
            cabin_diagnostics::event(cabin_diagnostics::Level::Warn, "network", "history_write_failed", format_args!("{}", error.code));
        }
    }

    pub fn record_check(&self, kind: &str, message: &str, report: Option<&NetworkCheckReport>) {
        self.journal(&self.runtime(), kind, message, report);
    }

    pub fn history(
        &self,
        before: Option<i64>,
        limit: u32,
    ) -> Result<Vec<NetworkHistoryEntry>, AppError> {
        self.with_store(|store| store.network_history(before, limit))
    }

    /// Close the on-disk database before deleting its directory. A temporary
    /// in-memory store lets status readers continue during destruction.
    pub fn suspend_store(&self) -> Result<(), AppError> {
        let empty = Store::in_memory()?;
        let mut runtime = self.runtime();
        *self.store.locked() = empty;
        runtime
            .grant
            .invalidate("The network configuration was removed");
        runtime.connectivity.clear();
        runtime.installing = false;
        runtime.apply_policy();
        Ok(())
    }

    /// Reopen after destruction (or after a failed attempt) so future installs
    /// start with a working, empty database and no detached SQLite connection.
    pub fn reopen_store(&self) -> Result<(), AppError> {
        std::fs::create_dir_all(&self.directory).map_err(|e| error(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| error(e.to_string()))?;
        }
        let reopened = Store::open(&self.directory.join("network.sqlite3"))?;
        let paused = reopened.setting::<bool>(PAUSED)?.unwrap_or(false);
        let auto_recover = reopened.setting::<bool>(AUTO_RECOVER)?.unwrap_or(true);
        let mut runtime = self.runtime();
        *self.store.locked() = reopened;
        runtime.failure = None;
        runtime.path_revision += 1;
        runtime.installing = false;
        runtime.connectivity.clear();
        runtime.grant = GateState::with_preferences(paused, auto_recover);
        runtime.apply_policy();
        Ok(())
    }

    // ------------------------------------------------------------- route

    pub fn route(&self) -> Result<NetworkRoute, AppError> {
        Ok(self
            .with_store(|store| store.setting(ROUTE))?
            .unwrap_or_default())
    }

    pub fn catalog(&self) -> Result<ProxyCatalog, AppError> {
        self.with_store(proxy::catalog)
    }

    /// The catalog nodes for `ids`, in order.
    fn nodes(&self, ids: &[String]) -> Result<Vec<Node>, AppError> {
        ids.iter()
            .map(|id| {
                self.with_store(|store| proxy::find_node(store, id))
                    .map_err(|_| error(format!("The node {id} is no longer in any subscription")))
            })
            .collect()
    }

    pub fn upstream(&self, route: &NetworkRoute) -> Result<Upstream, AppError> {
        if let Some(interface) = route.bind_interface.as_deref() {
            vm_core::network_interfaces::require_available(interface).map_err(|e| error(e.to_string()))?;
        }
        if !route.enabled {
            return Ok(match &route.bind_interface {
                Some(interface) => Upstream::BoundDirect(interface.clone()),
                None => Upstream::Direct,
            });
        }
        if route.chain.is_empty() {
            return Err(error("Add a node to the route, or turn routing off"));
        }
        let hops: Vec<_> = self
            .nodes(&route.chain)?
            .into_iter()
            .map(|node| node.config)
            .collect();
        // Validate the chain with shoes before touching the gateway.
        proxy::validate_on_interface(&proxy::render_chain_on_interface(&hops, 18080, route.bind_interface.as_deref())?, route.bind_interface.as_deref())?;
        Ok(match &route.bind_interface {
            Some(interface) => Upstream::BoundChain(hops, interface.clone()),
            None => Upstream::Chain(hops),
        })
    }

    /// Start the VM's gateway on `socket`; it also serves the VM's SSH on
    /// `127.0.0.1:ssh_port`.
    pub fn start(&self, socket: &Path, ssh_port: u16) -> Result<(), AppError> {
        self.start_gateway(socket, ssh_port, &self.route()?, true)
    }

    pub fn start_with(&self, socket: &Path, ssh_port: u16, route: &NetworkRoute) -> Result<(), AppError> {
        self.start_gateway(socket, ssh_port, route, false)
    }

    fn start_gateway(
        &self,
        socket: &Path,
        ssh_port: u16,
        route: &NetworkRoute,
        saved_route: bool,
    ) -> Result<(), AppError> {
        let mut runtime = self.runtime();
        if runtime.gateway.is_some() {
            return Ok(());
        }
        // A saved binding may be unplugged at boot. Start SSH before trying
        // that upstream during the startup network check. Installation still
        // needs a working path for package downloads.
        let defer = saved_route && !runtime.installing && route.bind_interface.is_some();
        runtime.interface_paths = network_interfaces::paths().unwrap_or_default();
        let result = (|| {
            let upstream = (!defer).then(|| self.upstream(route)).transpose()?;
            Gateway::start_closed(socket, Vec::new(), upstream.as_ref(), (Ipv4Addr::LOCALHOST, ssh_port).into())
                .map_err(|e| error(e.to_string()))
        })();
        match result {
            Ok(gateway) => {
                runtime.gateway = Some(gateway);
                runtime.failure = None;
                runtime.gateway_session += 1;
                runtime.active_route = (!defer).then(|| route.clone());
                runtime.connectivity.clear();
                runtime.verified_exit = None;
                runtime.verified_samples.clear();
                runtime.grant.set_revision(route_revision(route));
                runtime
                    .grant
                    .invalidate("A full check is required before internet access");
                runtime.apply_policy();
                self.journal(&runtime, "gatewayStarted", "Network gateway started", None);
                Ok(())
            }
            Err(error) => {
                runtime.failure = Some(error.message.clone());
                self.journal(&runtime, "gatewayFailed", &error.message, None);
                Err(error)
            }
        }
    }

    /// Point a running gateway at `route` (nothing to do when it is stopped).
    pub fn switch(&self, route: &NetworkRoute) -> Result<(), AppError> {
        let mut runtime = self.runtime();
        self.refresh_interface_paths(&mut runtime);
        let Some(gateway) = &runtime.gateway else { return Ok(()) };
        if runtime.active_route.as_ref() == Some(route) && gateway.error().is_none() {
            return Ok(());
        }
        let upstream = self.upstream(route)?;
        if runtime.grant.mode == NetworkGateMode::Open {
            self.cancel_hold(&mut runtime);
            runtime.grant.invalidate("The gateway path needs a fresh network check");
            runtime.apply_policy();
        }
        let gateway = runtime.gateway.as_ref().ok_or_else(|| error("The network gateway stopped during the route change"))?;
        gateway.set_upstream(&upstream).map_err(|e| error(e.to_string()))?;
        runtime.active_route = Some(route.clone());
        runtime.upstream_revision += 1;
        Ok(())
    }

    pub fn save(&self, route: &NetworkRoute) -> Result<(), AppError> {
        self.with_store(|store| store.set_setting(ROUTE, route))?;
        Ok(())
    }

    /// Test-only shortcut for exercising upstream rollback without a guest.
    #[cfg(test)]
    pub fn apply(&self, route: &NetworkRoute) -> Result<(), AppError> {
        let previous = self.route()?;
        let point = self.rollback_point();
        self.invalidate_gate("The route changed and needs a full check");
        if let Err(error) = self.switch(route).and_then(|()| self.save(route)) {
            let _ = self.switch(&previous);
            self.restore_gate(point);
            return Err(error);
        }
        Ok(())
    }

    /// Whether a gateway is present (its route may still be failing).
    pub fn is_up(&self) -> bool {
        self.runtime().gateway.is_some()
    }

    pub fn ssh_port(&self) -> Option<u16> {
        self.runtime()
            .gateway
            .as_ref()
            .and_then(Gateway::ssh_addr)
            .map(|addr| addr.port())
    }

    pub fn stop(&self) {
        let mut runtime = self.runtime();
        runtime.installing = false;
        runtime.hold_timeout_pending = false;
        runtime.grant.invalidate("The network gateway stopped");
        runtime.apply_policy();
        if runtime.gateway.take().is_some() {
            self.journal(&runtime, "gatewayStopped", "Network gateway stopped", None);
        }
        runtime.active_route = None;
        runtime.connectivity.clear();
        runtime.verified_exit = None;
        runtime.verified_samples.clear();
        runtime.traffic = None;
    }

    pub fn status(&self, available: bool) -> Result<NetworkStatus, AppError> {
        let mut runtime = self.runtime();
        self.expire(&mut runtime);
        let route = self.route();
        self.revoke_drifted_route(&mut runtime, route.as_ref().ok());
        let route = route?;
        let grant = &runtime.grant;
        let error = runtime
            .gateway
            .as_ref()
            .and_then(Gateway::error)
            .or_else(|| runtime.failure.clone());
        let running = runtime.gateway.is_some() && error.is_none();
        let gate = if !available || !running {
            NetworkGateMode::Closed
        } else if runtime.internet_allowed() {
            NetworkGateMode::Open
        } else if grant.mode == NetworkGateMode::Checking {
            NetworkGateMode::Checking
        } else {
            NetworkGateMode::Closed
        };
        let exit_policy_error = runtime.exit_policy_error.as_ref()
            .filter(|(revision, _)| *revision == route_revision(&route))
            .map(|(_, reason)| reason.clone());
        let next_retry_at = if exit_policy_error.is_some() { None } else { grant.next_retry_at };
        Ok(NetworkStatus {
            available,
            running,
            route,
            error,
            gate_error: grant.error.clone(),
            exit_policy_error,
            gate,
            connectivity: runtime.connectivity.mode,
            failed_checks: runtime.connectivity.failed_checks,
            connectivity_error: runtime.connectivity.error.clone(),
            gateway_session: runtime.gateway.as_ref().map(|_| runtime.gateway_session),
            paused: grant.paused,
            auto_recover: grant.auto_recover,
            region_follows_exit: self
                .with_store(|store| store.setting::<bool>(REGION_FOLLOWS_EXIT))
                .ok()
                .flatten()
                .unwrap_or(true),
            revision: grant.revision.clone(),
            generation: grant.generation,
            checked_at: grant.checked_at,
            expires_at: grant.expires_at,
            next_retry_at,
            retry_attempt: grant.retry_attempt,
            report: grant.report.clone(),
        })
    }

    pub fn traffic(&self) -> Option<NetworkTraffic> {
        let mut runtime = self.runtime();
        let counters = runtime.gateway.as_ref()?.traffic();
        let now = now_millis();
        if let Some(sample) = &runtime.traffic
            && (0..500).contains(&now.saturating_sub(sample.sampled_at))
        {
            return Some(sample.clone());
        }
        let sample = traffic_sample(
            runtime.gateway_session,
            now,
            counters.rx_bytes,
            counters.tx_bytes,
            runtime.traffic.as_ref(),
        );
        runtime.traffic = Some(sample.clone());
        Some(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_network::test_support::*;

    #[test]
    fn legacy_route_defaults_to_system_interface() {
        let route: NetworkRoute = serde_json::from_str(r#"{"enabled":false,"chain":[]}"#).unwrap();
        assert_eq!(route.bind_interface, None);
        let bound = NetworkRoute { bind_interface: Some("en0".into()), ..route.clone() };
        assert_ne!(route_revision(&route), route_revision(&bound));
        assert_eq!(serde_json::to_value(bound).unwrap()["bindInterface"], "en0");
    }

    #[test]
    fn unavailable_interface_is_rejected_for_direct_and_chain_routes() {
        let dir = temp("missing-interface");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        for enabled in [false, true] {
            let route = NetworkRoute { enabled, chain: vec![catalog.nodes[0].id.clone()], bind_interface: Some("cabin-nonexistent-interface".into()), ..NetworkRoute::default() };
            assert!(network.upstream(&route).unwrap_err().message.contains("unavailable"));
            assert!(network.check(&route).is_err());
        }
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn missing_saved_interface_keeps_ssh_up_with_no_outbound_listener() {
        let dir = temp("absent-adapter-start");
        let network = GlobalNetwork::open(&dir).unwrap();
        let bound = NetworkRoute { bind_interface: Some("en-missing".into()), ..NetworkRoute::default() };
        network.save(&bound).unwrap();
        network.start(&dir.join("gateway.sock"), 0).unwrap();
        let ssh = network.ssh_port().unwrap();
        assert!(std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, ssh)).is_ok());
        assert!(network.runtime().gateway.as_ref().unwrap().proxy_url().is_none());
        assert!(network.switch(&bound).is_err());
        network.fail_route_preflight("The selected interface is unavailable");
        let status = network.status(true).unwrap();
        assert_eq!(status.route, bound);
        assert_eq!(status.gate, NetworkGateMode::Closed);
        assert_eq!(status.retry_attempt, 1);
        assert!(status.next_retry_at.unwrap() > now_millis());
        network.switch(&NetworkRoute::default()).unwrap();
        assert_eq!(network.ssh_port(), Some(ssh));
        assert!(network.runtime().gateway.as_ref().unwrap().proxy_url().is_some());
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_enabled_route_is_never_direct_by_accident() {
        let dir = temp("invalid");
        let network = GlobalNetwork::open(&dir).unwrap();
        let empty = NetworkRoute {
            enabled: true,
            chain: vec![],
            bind_interface: None,
            ..NetworkRoute::default()
        };
        let missing = NetworkRoute {
            enabled: true,
            chain: vec!["gone::n1".into()],
            bind_interface: None,
            ..NetworkRoute::default()
        };
        assert!(network.upstream(&empty).is_err());
        assert!(network.upstream(&missing).is_err());
        let off = NetworkRoute {
            enabled: false,
            chain: vec!["gone::n1".into()],
            bind_interface: None,
            ..NetworkRoute::default()
        };
        assert!(matches!(network.upstream(&off).unwrap(), Upstream::Direct));
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_apply_keeps_the_previous_gateway_saved_route_and_grant() {
        let dir = temp("switch");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&dir.join("gateway.sock"), 0).unwrap();
        open_gate(&network, &NetworkRoute::default());
        let invalid = NetworkRoute {
            enabled: true,
            chain: vec!["missing::n1".into()],
            bind_interface: None,
            ..NetworkRoute::default()
        };
        assert!(network.apply(&invalid).is_err());
        assert_eq!(network.route().unwrap(), NetworkRoute::default());
        assert!(network.is_up());
        let status = network.status(true).unwrap();
        assert!(status.running);
        assert_eq!(status.gate, NetworkGateMode::Open);
        assert!(status.gate_error.is_none());
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn destruction_closes_the_database_and_reinstall_can_save_fresh_settings() {
        let dir = temp("reset");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network
            .import(
                "old",
                None,
                Some("proxies:\n  - {name: old, type: socks5, server: 127.0.0.1, port: 1080}\n"),
            )
            .unwrap();
        network
            .save(&NetworkRoute {
                enabled: true,
                chain: vec![catalog.nodes[0].id.clone()],
                bind_interface: None,
            ..NetworkRoute::default()
            })
            .unwrap();
        network.suspend_store().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(network.catalog().unwrap().nodes.is_empty());
        assert!(!network.route().unwrap().enabled);
        network.reopen_store().unwrap();
        network
            .import(
                "new",
                None,
                Some("proxies:\n  - {name: new, type: socks5, server: 127.0.0.1, port: 1081}\n"),
            )
            .unwrap();
        drop(network);
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.catalog().unwrap();
        assert_eq!(catalog.sources.len(), 1);
        assert_eq!(catalog.sources[0].name, "new");
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }
}
