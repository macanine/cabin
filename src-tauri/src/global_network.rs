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
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::SyncSender;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use agent_core::model::{
    GeoInfo, GuestNetworkProbe, HopCheck, IpClassification, IpType, NetworkCheckReport, NetworkConnectivity,
    NetworkGateMode, NetworkHistoryEntry, NetworkRoute, NetworkStatus, NetworkTraffic, NodePlace,
    ProxyCatalog, RouteCheck,
};
use agent_core::proxy::{self, subscription::Node};
use agent_core::store::{Store, StoredSource};
use agent_core::util::now_millis;
use cabin_ip_detection::{Detector, IpInfo, Service, public_ip};
use vm_core::netgw::{EgressMode, Gateway, Upstream};
use vm_core::network_interfaces::{self, InterfacePath};

use crate::app_error::AppError;
use crate::network_gate::{GateState, GateToken, route_revision};
use crate::network_health::{Connectivity, HoldCheck, traffic_sample};

const ROUTE: &str = "network_route";
const PAUSED: &str = "network_paused";
const AUTO_RECOVER: &str = "network_auto_recover";
/// What a chain check connects to through each prefix.
const CHECK_TARGET: &str = "www.google.com:443";
/// Node servers rarely move; their locations are kept this long.
const PLACE_TTL_MS: i64 = 6 * 60 * 60 * 1000;
/// A failed or location-less lookup is tried again after this long.
const PLACE_RETRY_MS: i64 = 2 * 60 * 1000;
/// Parallel lookups when locating several nodes.
const LOCATE_WORKERS: usize = 6;

type PlaceCache = HashMap<String, (i64, Result<GeoInfo, String>)>;
type QualityCache = HashMap<IpAddr, (Instant, Option<IpClassification>)>;

fn error(message: impl Into<String>) -> AppError {
    AppError {
        code: "network".into(),
        message: message.into(),
        details: None,
    }
}

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

fn detector(proxy_url: Option<&str>) -> Result<Detector, AppError> {
    let builder = Detector::builder()
        .ipv4_only()
        .timeout(Duration::from_secs(6))
        .retries(0);
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
        return if ip.is_ipv4() { Ok(ip) } else { Err(format!("{host} has no IPv4 address")) };
    }
    let addresses: Vec<IpAddr> = (host, 443)
        .to_socket_addrs()
        .map_err(|e| format!("Could not resolve {host}: {e}"))?
        .map(|address| address.ip())
        .collect();
    addresses
        .iter()
        .find(|ip| ip.is_ipv4())
        .copied()
        .ok_or_else(|| format!("{host} has no IPv4 address"))
}

fn lookup_location(proxy_url: Option<&str>) -> Result<GeoInfo, AppError> {
    detector(proxy_url)?.detect().map(geo).map_err(|failure| error(failure.to_string()))
}

fn public_ipv4(value: &str) -> bool {
    value.parse().is_ok_and(|address: IpAddr| address.is_ipv4() && public_ip(address))
}

#[derive(Default)]
struct Runtime {
    gateway: Option<Gateway>,
    failure: Option<String>,
    active_route: Option<NetworkRoute>,
    interface_paths: BTreeMap<String, InterfacePath>,
    grant: GateState,
    connectivity: Connectivity,
    installing: bool,
    sleeping: bool,
    gateway_session: u64,
    upstream_revision: u64,
    path_revision: u64,
    hard_block_revision: u64,
    verified_exit: Option<Ipv4Addr>,
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
            eprintln!("cabin: network history: {}", error.message);
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
    pub fn start(&self, socket: &vm_core::sockets::Endpoint, ssh_port: u16) -> Result<(), AppError> {
        self.start_gateway(socket, ssh_port, &self.route()?, true)
    }

    pub fn start_with(&self, socket: &vm_core::sockets::Endpoint, ssh_port: u16, route: &NetworkRoute) -> Result<(), AppError> {
        self.start_gateway(socket, ssh_port, route, false)
    }

    fn start_gateway(
        &self,
        socket: &vm_core::sockets::Endpoint,
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
        runtime.gateway.as_ref().unwrap().set_upstream(&upstream).map_err(|e| error(e.to_string()))?;
        runtime.active_route = Some(route.clone());
        runtime.upstream_revision += 1;
        Ok(())
    }

    /// Save only the adapter preference while full guest verification is deferred.
    pub fn save_interface(&self, route: &NetworkRoute) -> Result<(), AppError> {
        let mut runtime = self.runtime();
        let saved = self.route()?;
        if runtime.installing || route.enabled != saved.enabled || route.chain != saved.chain {
            return Err(error("Start the machine and resume internet access to apply proxy route changes"));
        }
        if let Some(interface) = route.bind_interface.as_deref() {
            network_interfaces::require_available(interface).map_err(|e| error(e.to_string()))?;
        }
        self.save(route)?;
        runtime.clear_upstream();
        runtime.connectivity.clear();
        runtime.grant.set_revision(route_revision(route));
        let reason = "The network interface was saved and needs a check before internet access opens";
        runtime.grant.invalidate(reason);
        runtime.apply_policy();
        self.journal(&runtime, "interfaceSaved", reason, None);
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
        Ok(NetworkStatus {
            available,
            running,
            route,
            error,
            gate_error: grant.error.clone(),
            gate,
            connectivity: runtime.connectivity.mode,
            failed_checks: runtime.connectivity.failed_checks,
            connectivity_error: runtime.connectivity.error.clone(),
            gateway_session: runtime.gateway.as_ref().map(|_| runtime.gateway_session),
            paused: grant.paused,
            auto_recover: grant.auto_recover,
            revision: grant.revision.clone(),
            generation: grant.generation,
            checked_at: grant.checked_at,
            expires_at: grant.expires_at,
            next_retry_at: grant.next_retry_at,
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

    // --------------------------------------------------------------- gate

    pub fn begin_install(&self) {
        let mut runtime = self.runtime();
        runtime.installing = true;
        runtime.apply_policy();
    }

    pub fn end_install(&self) {
        let mut runtime = self.runtime();
        runtime.installing = false;
        runtime.apply_policy();
    }

    #[cfg(test)]
    fn gate_snapshot(&self) -> GateState {
        self.runtime().grant.clone()
    }

    pub fn rollback_point(&self) -> RollbackPoint {
        let runtime = self.runtime();
        RollbackPoint {
            gateway_session: runtime.gateway_session,
            path_revision: runtime.path_revision,
            hard_block_revision: runtime.hard_block_revision,
            grant: runtime.grant.clone(),
        }
    }

    pub fn restore_gate(&self, point: RollbackPoint) {
        let mut runtime = self.runtime();
        self.refresh_interface_paths(&mut runtime);
        if (point.grant.mode != NetworkGateMode::Open || runtime.active_route.is_some())
            && point.gateway_session == runtime.gateway_session
            && point.path_revision == runtime.path_revision
            && point.hard_block_revision == runtime.hard_block_revision
            && point.grant != runtime.grant
        {
            runtime.grant.restore_if_current(point.grant);
            runtime.apply_policy();
        }
    }

    pub fn invalidate_gate(&self, reason: impl Into<String>) {
        let mut runtime = self.runtime();
        let reason = reason.into();
        if runtime.grant.mode == NetworkGateMode::Closed
            && runtime.grant.error.as_deref() == Some(&reason)
        {
            return;
        }
        self.cancel_hold(&mut runtime);
        runtime.grant.invalidate(reason.clone());
        runtime.apply_policy();
        self.journal(&runtime, "grantInvalidated", &reason, None);
    }

    pub fn fail_route_preflight(&self, reason: &str) {
        let mut runtime = self.runtime();
        self.cancel_hold(&mut runtime);
        runtime.grant.record_failure(reason);
        runtime.apply_policy();
    }

    pub fn abandon_upstream(&self, reason: &str) {
        let mut runtime = self.runtime();
        runtime.clear_upstream();
        runtime.connectivity.clear();
        runtime.grant.invalidate(reason);
        runtime.apply_policy();
        self.journal(&runtime, "grantInvalidated", reason, None);
    }

    pub fn observe_environment_revision(&self, revision: Option<String>) -> bool {
        let mut runtime = self.runtime();
        if !runtime.grant.set_environment_revision(revision) {
            return false;
        }
        self.cancel_hold(&mut runtime);
        runtime.apply_policy();
        self.journal(
            &runtime,
            "environmentChanged",
            "Environment settings changed",
            None,
        );
        true
    }

    pub fn begin_check(&self, revision: String, preserve_grant: bool) -> Option<CheckAttempt> {
        let mut runtime = self.runtime();
        self.refresh_interface_paths(&mut runtime);
        self.expire(&mut runtime);
        if runtime.sleeping || runtime.active_route.is_none() {
            return None;
        }
        let refresh = preserve_grant && runtime.grant.mode == NetworkGateMode::Open;
        let held = runtime.connectivity.check == Some(HoldCheck::Verification);
        if runtime.connectivity.check.is_some() && (!refresh || !held) {
            return None;
        }
        let token = if refresh {
            runtime.grant.begin_refresh()
        } else {
            runtime.grant.begin(revision)?
        };
        runtime.apply_policy();
        Some(CheckAttempt {
            path: runtime.identity(),
            token,
            refresh,
            held,
        })
    }

    pub fn finish_check(
        &self,
        attempt: &CheckAttempt,
        report: NetworkCheckReport,
        public_ip: Option<Ipv4Addr>,
        samples: &[Result<GeoInfo, String>],
        candidate: Option<&NetworkRoute>,
    ) -> Result<bool, AppError> {
        let mut runtime = self.runtime();
        self.refresh_interface_paths(&mut runtime);
        self.expire(&mut runtime);
        let matching = runtime.identity() == attempt.path && runtime.grant.matches(&attempt.token);
        if matching && candidate.is_none() {
            let saved = self.route().ok();
            if runtime.active_route != saved {
                if !self.revoke_drifted_route(&mut runtime, saved.as_ref()) {
                    runtime.grant.fail(&attempt.token, "The saved route changed during verification");
                    runtime.apply_policy();
                }
                return Ok(false);
            }
        }
        let gateway_failed = runtime.gateway.as_ref().is_none_or(|gateway| gateway.error().is_some());
        if matching && attempt.held && gateway_failed {
            return Err(error("The gateway stopped during verification"));
        }
        if !matching || gateway_failed {
            if runtime.grant.matches(&attempt.token) {
                if attempt.refresh {
                    runtime.grant.defer_refresh(&attempt.token);
                } else {
                    runtime
                        .grant
                        .fail(&attempt.token, "The network changed during verification");
                }
                runtime.apply_policy();
            }
            return Ok(false);
        }
        if attempt.refresh && matches!((runtime.verified_exit, public_ip), (Some(old), Some(new)) if old != new) {
            return Err(error("The public exit no longer matches the verified route"));
        }
        if attempt.held && public_ip.zip(runtime.verified_exit).is_none() {
            return Err(error("Could not confirm the verified public exit"));
        }
        if let Some(route) = candidate.filter(|_| report.can_open) {
            self.save(route)?;
        }
        let opened = runtime.grant.finish_observation(&attempt.token, report);
        if opened {
            runtime.connectivity.clear();
            runtime.verified_exit = public_ip;
            runtime.verified_samples = samples
                .iter()
                .map(|sample| sample.as_ref().ok().cloned())
                .collect();
        }
        runtime.apply_policy();
        if opened && attempt.held {
            self.journal(&runtime, "recovered", "Network verification recovered. Held traffic resumed.", None);
        }
        Ok(opened)
    }

    pub fn fail_check(&self, attempt: &CheckAttempt, message: &str) {
        let mut runtime = self.runtime();
        runtime.grant.fail(&attempt.token, message);
        runtime.apply_policy();
    }

    /// The machine must still observe the same public IP and country. A
    /// previous provider sample may fill in a temporary service failure, with
    /// its original timestamp visible in the report.
    pub fn reuse_exit_samples(
        &self,
        attempt: &CheckAttempt,
        guest: &GuestNetworkProbe,
        samples: &mut [Result<GeoInfo, String>],
    ) {
        let runtime = self.runtime();
        if !attempt.refresh
            || runtime.identity() != attempt.path
            || !runtime.grant.matches(&attempt.token)
        {
            return;
        }
        for (sample, prior) in samples.iter_mut().zip(&runtime.verified_samples) {
            if sample.is_ok() {
                continue;
            }
            if let Some(prior) = prior
                && guest.public_ip.as_deref() == Some(&prior.ip)
                && guest
                    .trace_location
                    .as_deref()
                    .zip(prior.country_code.as_deref())
                    .is_some_and(|(observed, trusted)| observed.eq_ignore_ascii_case(trusted))
            {
                let mut prior = prior.clone();
                if !prior.source.starts_with("Previously verified · ") {
                    prior.source = format!("Previously verified · {}", prior.source);
                }
                *sample = Ok(prior);
            }
        }
    }

    pub fn blocked_reason(&self) -> Option<String> {
        let mut runtime = self.runtime();
        self.expire(&mut runtime);
        if runtime.grant.mode == NetworkGateMode::Open
            && runtime.connectivity.mode != NetworkConnectivity::Blocked
        {
            return None;
        }
        Some(
            runtime
                .connectivity
                .error
                .clone()
                .or_else(|| runtime.grant.error.clone())
                .unwrap_or_else(|| {
                    "The machine needs a passing Network Check before it can use the internet"
                        .into()
                }),
        )
    }

    pub fn set_paused(&self, paused: bool) -> Result<(), AppError> {
        let mut runtime = self.runtime();
        self.with_store(|store| store.set_setting(PAUSED, &paused))?;
        if runtime.grant.paused != paused {
            runtime.grant.pause(paused);
            runtime.connectivity.clear();
            runtime.path_revision += 1;
            runtime.apply_policy();
            let message = if paused {
                "Internet access paused"
            } else {
                "Internet access resumed pending verification"
            };
            self.journal(
                &runtime,
                if paused { "paused" } else { "resumed" },
                message,
                None,
            );
        }
        Ok(())
    }

    pub fn set_auto_recover(&self, enabled: bool) -> Result<(), AppError> {
        let mut runtime = self.runtime();
        self.with_store(|store| store.set_setting(AUTO_RECOVER, &enabled))?;
        if runtime.grant.auto_recover != enabled {
            runtime.grant.set_auto_recover(enabled);
            self.journal(
                &runtime,
                "recoveryPreference",
                if enabled { "Automatic recovery enabled" } else { "Automatic recovery disabled" },
                None,
            );
        }
        Ok(())
    }

    pub fn expire_hold_now(&self) -> bool {
        self.expire_hold(&mut self.runtime(), Instant::now())
    }

    fn expire_hold(&self, runtime: &mut Runtime, now: Instant) -> bool {
        if !runtime.connectivity.expire(now) {
            return false;
        }
        let reason = "The network recheck timed out. A fresh check is required.";
        self.block_hold(runtime, reason);
        runtime.apply_policy();
        self.journal(runtime, "blocked", reason, None);
        true
    }

    fn block_hold(&self, runtime: &mut Runtime, reason: &str) {
        runtime.hard_block_revision += 1;
        runtime.path_revision += 1;
        runtime.grant.invalidate(reason);
    }

    fn expire(&self, runtime: &mut Runtime) -> bool {
        if self.expire_hold(runtime, Instant::now()) {
            return true;
        }
        if !runtime.grant.expire(now_millis()) {
            return false;
        }
        self.cancel_hold(runtime);
        runtime.apply_policy();
        self.journal(
            runtime,
            "grantExpired",
            runtime
                .grant
                .error
                .as_deref()
                .unwrap_or("Verification expired"),
            None,
        );
        true
    }

    fn revoke_drifted_route(&self, runtime: &mut Runtime, saved: Option<&NetworkRoute>) -> bool {
        if runtime.grant.mode != NetworkGateMode::Open
            || saved.is_some_and(|route| runtime.active_route.as_ref() == Some(route))
        {
            return false;
        }
        self.cancel_hold(runtime);
        runtime.path_revision += 1;
        let reason = "The saved network route changed or is unavailable. A fresh check is required.";
        runtime.grant.invalidate(reason);
        runtime.apply_policy();
        self.journal(runtime, "grantInvalidated", reason, None);
        true
    }

    pub fn gate_due(&self) -> (bool, bool) {
        let mut runtime = self.runtime();
        let expired = self.expire(&mut runtime);
        let drifted = runtime.grant.mode == NetworkGateMode::Open
            && self.revoke_drifted_route(&mut runtime, self.route().ok().as_ref());
        let due = !runtime.sleeping && runtime.connectivity.mode == NetworkConnectivity::Normal
            && runtime
                .gateway
                .as_ref()
                .is_some_and(Gateway::connected)
            && runtime.grant.due(now_millis());
        (due, expired || drifted)
    }

    #[cfg(any(target_os = "macos", test))]
    pub fn system_sleep_changed(&self, sleeping: bool) {
        let mut runtime = self.runtime();
        runtime.sleeping = sleeping;
        runtime.path_revision += 1;
        if runtime.active_route.as_ref().is_some_and(|route| route.bind_interface.is_some()) {
            runtime.clear_upstream();
        }
        runtime.connectivity.clear();
        runtime.verified_exit = None;
        runtime.verified_samples.clear();
        let reason = if sleeping {
            "The Mac is going to sleep"
        } else {
            "The Mac woke up. A fresh network check is required."
        };
        runtime.grant.invalidate(reason);
        runtime.apply_policy();
        if runtime.gateway.is_some() {
            self.journal(&runtime, "grantInvalidated", reason, None);
        }
        self.wake_health();
    }

    pub fn set_health_wake(&self, sender: SyncSender<()>) {
        *self.health_wake.locked() = Some(sender);
    }

    pub fn wake_health(&self) {
        if let Some(sender) = &*self.health_wake.locked() {
            let _ = sender.try_send(());
        }
    }

    pub fn poll_interface_paths(&self) -> bool {
        self.refresh_interface_paths(&mut self.runtime())
    }

    fn refresh_interface_paths(&self, runtime: &mut Runtime) -> bool {
        runtime.gateway.is_some()
            && self.observe_interface_paths(runtime, network_interfaces::paths().unwrap_or_default())
    }

    fn observe_interface_paths(&self, runtime: &mut Runtime, paths: BTreeMap<String, InterfacePath>) -> bool {
        let previous = std::mem::replace(&mut runtime.interface_paths, paths);
        if previous == runtime.interface_paths {
            return false;
        }
        let changed = |name: &str| previous.get(name) != runtime.interface_paths.get(name);
        let active_changed = runtime.active_route.as_ref().and_then(|route| route.bind_interface.as_deref()).is_some_and(changed);
        let saved_changed = self.route().ok().and_then(|route| route.bind_interface).as_deref().is_some_and(changed);
        if !active_changed && !saved_changed {
            return false;
        }
        self.invalidate_interface_path(runtime, active_changed);
        true
    }

    fn invalidate_interface_path(&self, runtime: &mut Runtime, clear_upstream: bool) {
        runtime.path_revision += 1;
        runtime.hard_block_revision += 1;
        if clear_upstream {
            runtime.installing = false;
            runtime.clear_upstream();
        }
        runtime.connectivity.clear();
        let reason = "The selected interface changed and needs a fresh network check";
        runtime.grant.invalidate(reason);
        runtime.apply_policy();
        self.journal(runtime, "interfaceChanged", reason, None);
    }

    /// Called by the macOS network-path watcher (host.rs).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn path_changed(&self) -> bool {
        let mut runtime = self.runtime();
        if runtime.active_route.as_ref().is_some_and(|route| route.bind_interface.is_some()) {
            self.invalidate_interface_path(&mut runtime, true);
            return true;
        }
        runtime.path_revision += 1;
        self.start_hold(&mut runtime, HoldCheck::Reachability, "The Mac network path changed".into())
    }

    pub fn hold_check(&self) -> Option<HoldCheck> {
        self.runtime().connectivity.check
    }

    pub fn health_target(&self) -> Option<HealthTarget> {
        let mut runtime = self.runtime();
        self.expire(&mut runtime);
        let route = self.route().ok();
        self.revoke_drifted_route(&mut runtime, route.as_ref());
        if runtime.sleeping || runtime.installing
            || runtime.grant.paused
            || (runtime.grant.mode != NetworkGateMode::Open
                && !(runtime.connectivity.mode == NetworkConnectivity::Blocked
                    && runtime.grant.auto_recover))
            || runtime.active_route != route
        {
            return None;
        }
        let gateway = runtime.gateway.as_ref()?;
        let proxy_url = if let Some(message) = gateway.error() {
            Err(format!("The network gateway stopped: {message}"))
        } else if !gateway.connected() {
            Err("The gateway lost its machine connection".into())
        } else {
            gateway
                .proxy_url()
                .ok_or_else(|| "The gateway listener is unavailable".into())
        };
        Some(HealthTarget {
            identity: runtime.identity(),
            generation: runtime.grant.generation,
            check: runtime.connectivity.check,
            proxy_url,
        })
    }

    pub fn observe_health(
        &self,
        target: &HealthTarget,
        result: Result<Option<Ipv4Addr>, String>,
    ) -> HealthChange {
        if matches!(target.check, Some(HoldCheck::Guest | HoldCheck::Verification)) {
            return HealthChange::default();
        }
        self.observe_reachability(target, result)
    }

    pub fn observe_guest_health(
        &self,
        target: &HealthTarget,
        result: Result<Option<Ipv4Addr>, String>,
    ) -> HealthChange {
        if target.check != Some(HoldCheck::Guest) {
            return HealthChange::default();
        }
        self.observe_reachability(target, result)
    }

    fn observe_reachability(
        &self,
        target: &HealthTarget,
        result: Result<Option<Ipv4Addr>, String>,
    ) -> HealthChange {
        let mut runtime = self.runtime();
        self.expire(&mut runtime);
        if runtime.grant.mode == NetworkGateMode::Open
            && self.revoke_drifted_route(&mut runtime, self.route().ok().as_ref())
        {
            return HealthChange { changed: true, hard_block: false };
        }
        if !target.is_current(&runtime) || runtime.gateway.is_none() || runtime.grant.paused {
            return HealthChange::default();
        }
        let result = result.and_then(|address| {
            if runtime.grant.mode == NetworkGateMode::Open {
                match (runtime.verified_exit, address) {
                    (Some(old), Some(new)) if old != new => {
                        return Err("The public exit no longer matches the verified route".into());
                    }
                    (None, _) | (_, None) if target.check.is_some() => {
                        return Err("Could not confirm the verified public exit".into());
                    }
                    _ => {}
                }
            }
            Ok(())
        });
        match result {
            Ok(()) if runtime.connectivity.clear() => {
                runtime.path_revision += 1;
                runtime.apply_policy();
                let message = if runtime.grant.mode == NetworkGateMode::Open {
                    "Connectivity recovered. Held traffic resumed."
                } else {
                    "Connectivity recovered. Waiting for a fresh environment check."
                };
                self.journal(&runtime, "recovered", message, None);
                HealthChange { changed: true, hard_block: false }
            }
            Err(reason) if target.check.is_some() => {
                let hard_block = self.fail_hold(&mut runtime, reason);
                HealthChange { changed: true, hard_block }
            }
            Err(reason) => HealthChange {
                changed: self.start_hold(&mut runtime, HoldCheck::Reachability, reason),
                hard_block: false,
            },
            Ok(()) => HealthChange::default(),
        }
    }

    /// A failed renewal must be retried by the verifier, which observes the
    /// guest path and required checks as well as the Mac's HTTPS connection.
    pub fn recheck_verification(&self, attempt: &CheckAttempt, check: HoldCheck, reason: String) -> bool {
        let mut runtime = self.runtime();
        self.expire(&mut runtime);
        if !attempt.refresh
            || runtime.identity() != attempt.path
            || !runtime.grant.matches(&attempt.token)
        {
            return false;
        }
        runtime.grant.defer_refresh(&attempt.token);
        if attempt.held {
            self.fail_hold(&mut runtime, reason)
        } else {
            if self.start_hold(&mut runtime, check, reason) {
                self.wake_health();
            }
            false
        }
    }

    pub fn held_target_is_current(&self, target: &HealthTarget) -> bool {
        target.is_current(&self.runtime())
    }

    /// Count a scheduled verification that failed before obtaining a check token.
    pub fn fail_held_preflight(&self, target: &HealthTarget, reason: String) -> bool {
        let mut runtime = self.runtime();
        self.expire(&mut runtime);
        target.check == Some(HoldCheck::Verification) && target.is_current(&runtime)
            && self.fail_hold(&mut runtime, reason)
    }

    fn start_hold(&self, runtime: &mut Runtime, check: HoldCheck, reason: String) -> bool {
        if runtime.grant.mode != NetworkGateMode::Open || runtime.installing
            || !runtime.connectivity.hold(check, reason.clone())
        {
            return false;
        }
        runtime.path_revision += 1;
        runtime.apply_policy();
        self.journal(runtime, "rechecking", &reason, None);
        true
    }

    fn fail_hold(&self, runtime: &mut Runtime, reason: String) -> bool {
        if !runtime.connectivity.fail(reason.clone()) {
            return false;
        }
        let blocked = runtime.connectivity.mode == NetworkConnectivity::Blocked;
        if blocked {
            self.block_hold(runtime, &format!("Three network rechecks failed: {reason}"));
        } else {
            runtime.path_revision += 1;
        }
        runtime.apply_policy();
        self.journal(runtime, if blocked { "blocked" } else { "rechecking" }, &reason, None);
        blocked
    }

    fn cancel_hold(&self, runtime: &mut Runtime) {
        if runtime.connectivity.check.is_some() {
            runtime.connectivity.clear();
            runtime.path_revision += 1;
        }
    }

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
        let node = proxy::subscription::parse_nodes(&text)?.remove(0);
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
            let via = runtime.gateway.as_ref().and_then(Gateway::proxy_url)
                .ok_or_else(|| error("The route is unavailable"))?;
            (source, via, runtime.identity(), runtime.grant.generation)
        };

        // A slow provider cannot hold up a route change or a catalog read.
        let nodes = proxy::fetch_refresh(&source, Some(&via))?;
        let _guard = self.transaction.locked();
        {
            let mut runtime = self.runtime();
            self.expire(&mut runtime);
            if !runtime.internet_allowed() || runtime.identity() != path || runtime.grant.generation != generation {
                return Err(error("The network changed while the subscription was being downloaded"));
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

    fn save_refreshed_source(&self, source: &StoredSource, nodes: &[Node]) -> Result<ProxyCatalog, AppError> {
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
            vm_core::network_interfaces::require_available(name).map_err(|e| error(e.to_string()))?;
        }
        let identity = self.runtime().identity();
        let node = self.with_store(|store| proxy::nodes::test_node(store, id, revision))?;
        let result = proxy::nodes::measure(&node, id, revision, kind, CHECK_TARGET, interface);
        if self.runtime().identity() != identity {
            return Err(error("The network changed while the node was being tested"));
        }
        if let Some(name) = interface {
            vm_core::network_interfaces::require_available(name).map_err(|e| error(e.to_string()))?;
        }
        Ok(result)
    }

    /// One node on its own.
    pub fn probe(&self, id: &str) -> Result<agent_core::model::ProbeResult, AppError> {
        let route = self.route()?;
        if let Some(name) = route.bind_interface.as_deref() {
            vm_core::network_interfaces::require_available(name).map_err(|e| error(e.to_string()))?;
        }
        let identity = self.runtime().identity();
        let node = self.nodes(&[id.to_string()])?.remove(0);
        let result = proxy::probe_chain_on_interface(&[node], CHECK_TARGET, route.bind_interface.as_deref())?;
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
            let result = proxy::probe_chain_on_interface(&nodes[..=index], CHECK_TARGET, route.bind_interface.as_deref()).unwrap_or_else(|e| {
                agent_core::model::ProbeResult {
                    ok: false,
                    latency_ms: None,
                    detail: e.to_string(),
                }
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
            return Err(error("The network changed while the route was being checked"));
        }
        if let Some(name) = route.bind_interface.as_deref() {
            vm_core::network_interfaces::require_available(name).map_err(|e| error(e.to_string()))?;
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
            return Err(error("The network changed while the exit was being looked up"));
        }
        let ip = exit.ip.parse().map_err(|_| error("Invalid exit address"))?;
        runtime.exit_observation = Some((identity, ip));
        // The fastest answer often only has a country. Reuse the verified
        // location only when it describes the very same public address.
        if let Some(reference) = runtime.verified_samples.iter().flatten().find(|sample| {
            sample.ip == exit.ip && exit.country_code.as_ref().is_none_or(|code| sample.country_code.as_ref() == Some(code))
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
    pub fn exit_type(&self, requested: &str, refresh: bool) -> Result<Option<IpClassification>, AppError> {
        let ip = requested.parse::<IpAddr>().map_err(|_| error("Invalid exit address"))?;
        let (identity, url) = {
            let runtime = self.runtime();
            let identity = runtime.identity();
            if runtime.exit_observation != Some((identity, ip)) || runtime.active_route.as_ref() != Some(&self.route()?) {
                return Err(error("The exit changed while its IP type was being looked up"));
            }
            (identity, Self::proxy_endpoint(&runtime)?)
        };
        let cached = {
            let mut cache = self.quality.locked();
            cache.retain(|_, (at, reply)| at.elapsed() < Duration::from_secs(if reply.is_some() { 3 * 24 * 3600 } else { 300 }));
            (!refresh).then(|| cache.get(&ip).map(|(_, reply)| reply.clone())).flatten()
        };
        if let Some(reply) = cached {
            let runtime = self.runtime();
            if runtime.identity() != identity || runtime.exit_observation != Some((identity, ip)) {
                return Err(error("The exit changed while its IP type was being looked up"));
            }
            return Ok(reply);
        }
        let reply = detector(Some(&url))?.classify(ip).map(|result| IpClassification {
            ip_type: ip_type(result.ip_type), inferred: result.inferred, organization: result.organization,
            asn: result.asn, source: result.source.name().into(),
        });
        let runtime = self.runtime();
        if runtime.exit_observation != Some((identity, ip)) || runtime.identity() != identity {
            return Err(error("The exit changed while its IP type was being looked up"));
        }
        let mut cache = self.quality.locked();
        if cache.len() >= 256 && let Some(old) = cache.iter().min_by_key(|(_, (at, _))| *at).map(|(ip, _)| *ip) {
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
                    scope.spawn(move || {
                        detector(Some(url))
                            .map_err(|error| error.message)?
                            .detect_service(service)
                            .map_err(|failure| failure.to_string())
                            .map(geo)
                            .and_then(|geo| {
                                if public_ipv4(&geo.ip) {
                                    Ok(geo)
                                } else {
                                    Err("The exit sample was not a public IPv4 address".to_string())
                                }
                            })
                    })
                })
                .collect::<Vec<_>>();
            Ok(handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|_| Err("The location sample thread stopped".into()))
                })
                .collect())
        })
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
        let mut info = detector.detect().map_err(|failure| error(failure.to_string()))?;
        // The map needs coordinates. Enrich the confirmed Mac address using
        // an explicit address lookup, retaining a fast result if that fails.
        if info.latitude.zip(info.longitude).is_none() && let Ok(located) = detector.lookup(info.ip) {
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
            std::thread::scope(|scope| {
                for _ in 0..LOCATE_WORKERS.min(missing.len()) {
                    scope.spawn(|| {
                        loop {
                            let Some(host) = next.locked()
                                .next()
                                .cloned()
                            else {
                                break;
                            };
                            let result = resolve(&host).and_then(|ip| {
                                detector
                                    .lookup(ip)
                                    .map(geo)
                                    .map_err(|e| format!("Could not locate {ip}: {e}"))
                            });
                            found.locked()
                                .push((host, result));
                        }
                    });
                }
            });
            let mut places = self.places.locked();
            for (host, result) in found.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
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
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use agent_core::model::{NetworkCheckCategory, NetworkCheckItem, NetworkCheckStatus};

    const TWO: &str = "proxies:\n  - {name: entry, type: socks5, server: 127.0.0.1, port: 1080}\n  - {name: exit, type: socks5, server: 127.0.0.2, port: 1081}\n";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cabin-network-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn passing_report(revision: &str) -> NetworkCheckReport {
        NetworkCheckReport {
            id: "test-check".into(),
            revision: revision.into(),
            started_at: now_millis(),
            finished_at: now_millis(),
            checks: vec![NetworkCheckItem {
                id: "test".into(),
                title: "Test".into(),
                category: NetworkCheckCategory::Route,
                status: NetworkCheckStatus::Pass,
                required: true,
                summary: "passed".into(),
                evidence: Vec::new(),
            }],
            passed_required: 1,
            total_required: 1,
            can_open: true,
        }
    }

    fn open_gate(network: &GlobalNetwork, route: &NetworkRoute) {
        let revision = route_revision(route);
        let token = network.begin_check(revision.clone(), false).unwrap();
        assert!(
            network
                .finish_check(&token, passing_report(&revision), None, &[], None)
                .unwrap()
        );
    }

    fn serve_once(body: &str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let body = body.to_string();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (format!("http://{address}/source"), handle)
    }

    #[test]
    fn connectivity_suspends_retries_and_rejects_stale_results() {
        let dir = temp("health");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        open_gate(&network, &NetworkRoute::default());
        let point = network.rollback_point();
        let target = || network.health_target().unwrap();
        let old = target();
        let first = network.observe_health(&old, Err("first failure".into()));
        assert!(first.changed && !first.hard_block);
        let status = network.status(true).unwrap();
        assert_eq!(status.connectivity, NetworkConnectivity::Rechecking);
        assert_eq!(status.failed_checks, 0);
        assert_eq!(status.gate, NetworkGateMode::Closed);
        assert!(!network.observe_health(&old, Ok(None)).changed);
        for failed in 1..=3 {
            let retry = target();
            let result = network.observe_health(&retry, Err(format!("retry {failed}")));
            assert!(result.changed);
            assert_eq!(result.hard_block, failed == 3);
            assert_eq!(network.status(true).unwrap().failed_checks, failed);
            assert!(!network.observe_health(&retry, Ok(None)).changed);
        }
        network.restore_gate(point);
        let status = network.status(true).unwrap();
        assert_eq!(status.connectivity, NetworkConnectivity::Blocked);
        assert_eq!(status.failed_checks, 3);
        assert_eq!(status.gate, NetworkGateMode::Closed);
        assert!(network.blocked_reason().is_some());

        let recovered = network.observe_health(&target(), Ok(None));
        assert!(recovered.changed);
        assert_eq!(
            network.status(true).unwrap().connectivity,
            NetworkConnectivity::Normal
        );
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        open_gate(&network, &NetworkRoute::default());
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Open);
        network.stop();
        drop(network);
        let reopened = GlobalNetwork::open(&dir).unwrap();
        let entries = reopened.history(None, 25).unwrap();
        assert!(
            entries
                .iter()
                .any(|entry| entry.kind == "blocked" && entry.failed_checks == 3)
        );
        assert!(entries.iter().any(|entry| entry.kind == "recovered"));
        drop(reopened);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn changed_mac_path_without_an_exit_identity_requires_a_fresh_grant() {
        let dir = temp("path-change");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        open_gate(&network, &NetworkRoute::default());
        assert!(network.path_changed());
        let target = network.health_target().unwrap();
        assert!(network.observe_health(&target, Ok(None)).changed);
        let status = network.status(true).unwrap();
        assert_eq!(status.connectivity, NetworkConnectivity::Rechecking);
        assert_eq!(status.failed_checks, 1);
        assert_eq!(status.gate, NetworkGateMode::Closed);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mac_path_change_can_resume_only_the_verified_exit() {
        let dir = temp("path-identity");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        open_gate(&network, &NetworkRoute::default());
        let exit = Ipv4Addr::new(203, 0, 113, 7);
        network.runtime().verified_exit = Some(exit);
        assert!(network.path_changed());
        let target = || network.health_target().unwrap();
        assert!(network.observe_health(&target(), Ok(Some(exit))).changed);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Open);
        assert!(network.path_changed());
        let wrong_exit = Ipv4Addr::new(203, 0, 113, 8);
        for failed in 1..=3 {
            let change = network.observe_health(&target(), Ok(Some(wrong_exit)));
            assert!(change.changed);
            assert_eq!(change.hard_block, failed == 3);
            assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        }
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn any_of_three_rechecks_can_resume_the_verified_route() {
        let dir = temp("connectivity-recovery");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        open_gate(&network, &NetworkRoute::default());
        let exit = Ipv4Addr::new(203, 0, 113, 7);
        network.runtime().verified_exit = Some(exit);
        let target = || network.health_target().unwrap();
        for failures in 0..3 {
            network.observe_health(&target(), Err("temporary failure".into()));
            for _ in 0..failures {
                assert!(!network.observe_health(&target(), Err("still unavailable".into())).hard_block);
            }
            assert!(network.observe_health(&target(), Ok(Some(exit))).changed);
            let status = network.status(true).unwrap();
            assert_eq!(status.connectivity, NetworkConnectivity::Normal);
            assert_eq!(status.failed_checks, 0);
            assert_eq!(status.gate, NetworkGateMode::Open);
        }
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn guest_dns_hold_ignores_mac_https_success_and_releases_only_after_guest_passes() {
        let dir = temp("guest-network-hold");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        let route = NetworkRoute::default();
        open_gate(&network, &route);
        let exit = Ipv4Addr::new(203, 0, 113, 7);
        network.runtime().verified_exit = Some(exit);
        let renewal = network.begin_check(route_revision(&route), true).unwrap();
        assert!(!network.recheck_verification(&renewal, HoldCheck::Guest, "Guest DNS failed".into()));
        let target = network.health_target().unwrap();
        assert_eq!(target.check, Some(HoldCheck::Guest));
        assert!(!network.observe_health(&target, Ok(Some(exit))).changed);
        assert!(!network.observe_guest_health(&target, Err("Guest DNS is still down".into())).hard_block);
        assert_eq!(network.status(true).unwrap().failed_checks, 1);
        assert!(!network.observe_guest_health(&target, Ok(Some(exit))).changed);
        assert!(network.observe_guest_health(&network.health_target().unwrap(), Ok(Some(exit))).changed);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Open);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stalled_hold_closes_access_and_rejects_late_success() {
        let dir = tempfile::tempdir().unwrap();
        let network = GlobalNetwork::open(dir.path()).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        let route = NetworkRoute::default();
        open_gate(&network, &route);
        let revision = route_revision(&route);
        let renewal = network.begin_check(revision.clone(), true).unwrap();
        network.recheck_verification(&renewal, HoldCheck::Verification, "Route stalled".into());
        let held = network.begin_check(revision.clone(), true).unwrap();
        assert!(network.expire_hold(&mut network.runtime(), Instant::now() + crate::network_health::HOLD_TIMEOUT));
        assert!(!network.finish_check(&held, passing_report(&revision), None, &[], None).unwrap());
        let status = network.status(true).unwrap();
        assert_eq!(status.gate, NetworkGateMode::Closed);
        assert_eq!(status.connectivity, NetworkConnectivity::Blocked);
        assert!(status.gate_error.unwrap().contains("timed out"));
        network.stop();
    }

    #[test]
    fn a_failed_renewal_requires_its_own_verification_to_resume() {
        let dir = temp("verification-hold");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        let route = NetworkRoute::default();
        open_gate(&network, &route);
        let exit = Ipv4Addr::new(203, 0, 113, 7);
        network.runtime().verified_exit = Some(exit);
        let revision = route_revision(&route);
        let renewal = network.begin_check(revision.clone(), true).unwrap();
        assert!(!network.recheck_verification(&renewal, HoldCheck::Verification, "Guest DNS failed".into()));
        let held = network.health_target().unwrap();
        assert_eq!(held.check, Some(HoldCheck::Verification));
        assert!(!network.observe_health(&held, Ok(Some(exit))).changed);
        assert_eq!(network.status(true).unwrap().failed_checks, 0);

        for failed in 1..=2 {
            let attempt = network.begin_check(revision.clone(), true).unwrap();
            assert!(attempt.held);
            assert!(!network.recheck_verification(&attempt, HoldCheck::Verification, "Guest DNS still failed".into()));
            assert!(!network.recheck_verification(&attempt, HoldCheck::Verification, "Duplicate result".into()));
            assert_eq!(network.status(true).unwrap().failed_checks, failed);
        }
        let attempt = network.begin_check(revision.clone(), true).unwrap();
        assert!(network.finish_check(&attempt, passing_report(&revision), Some(exit), &[], None).unwrap());
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Open);
        assert_eq!(network.hold_check(), None);
        assert!(!network.fail_held_preflight(&held, "Stale failure".into()));

        let renewal = network.begin_check(revision.clone(), true).unwrap();
        network.recheck_verification(&renewal, HoldCheck::Verification, "Guest DNS failed".into());
        let attempt = network.begin_check(revision.clone(), true).unwrap();
        network.set_paused(true).unwrap();
        assert!(!network.finish_check(&attempt, passing_report(&revision), Some(exit), &[], None).unwrap());
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        assert_eq!(network.hold_check(), None);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_chain_route_survives_restart_and_protects_its_subscription() {
        let dir = temp("route");
        let network = GlobalNetwork::open(&dir).unwrap();
        let catalog = network.import("test", None, Some(TWO)).unwrap();
        let route = NetworkRoute {
            enabled: true,
            chain: catalog.nodes.iter().map(|n| n.id.clone()).collect(),
            bind_interface: None,
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
            let route = NetworkRoute { enabled, chain: vec![catalog.nodes[0].id.clone()], bind_interface: Some("cabin-nonexistent-interface".into()) };
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
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
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
    fn off_default_interface_changes_discard_the_stale_upstream_and_schedule_recovery() {
        let dir = temp("non-default-adapter");
        let network = GlobalNetwork::open(&dir).unwrap();
        let system = NetworkRoute::default();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        let ssh = network.ssh_port().unwrap();
        // Model a selected adapter without changing the host network configuration.
        let name = "en-test";
        let path = || InterfacePath { index: 123, ipv4: vec![(Ipv4Addr::new(192, 0, 2, 2), None)] };
        let bound = NetworkRoute { bind_interface: Some(name.into()), ..system };
        network.save(&bound).unwrap();
        network.runtime().active_route = Some(bound.clone());
        open_gate(&network, &bound);
        let rollback = network.rollback_point();
        {
            let mut runtime = network.runtime();
            runtime.interface_paths.insert(name.into(), path());
            assert!(network.observe_interface_paths(&mut runtime, BTreeMap::new()));
            assert!(runtime.active_route.is_none());
            assert!(runtime.gateway.as_ref().unwrap().proxy_url().is_none());
        }
        network.restore_gate(rollback);
        assert_eq!(network.ssh_port(), Some(ssh));
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.fail_route_preflight("The selected interface is unavailable");
        {
            let mut runtime = network.runtime();
            runtime.connectivity.mode = NetworkConnectivity::Blocked;
            assert!(network.observe_interface_paths(&mut runtime, BTreeMap::from([(name.into(), path())])));
            assert_eq!(runtime.connectivity.mode, NetworkConnectivity::Normal);
            assert!(runtime.grant.due(now_millis()));
            assert!(runtime.active_route.is_none());
        }
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn path_change_during_system_candidate_prevents_restoring_bound_grant() {
        let dir = temp("br");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        let bound = NetworkRoute { bind_interface: Some("en-test".into()), ..NetworkRoute::default() };
        network.save(&bound).unwrap();
        network.runtime().active_route = Some(bound.clone());
        open_gate(&network, &bound);
        let rollback = network.rollback_point();
        network.invalidate_gate("Checking candidate");
        let system = NetworkRoute::default();
        network.switch(&system).unwrap();
        let revision = route_revision(&system);
        let attempt = network.begin_check(revision.clone(), false).unwrap();
        let hard_block_revision = network.runtime().hard_block_revision;
        // While the candidate is active, the system-path branch does not bump
        // hard_block_revision; path_revision must still invalidate the grant.
        network.path_changed();
        assert_eq!(network.runtime().hard_block_revision, hard_block_revision);
        assert!(!network.finish_check(&attempt, passing_report(&revision), None, &[], None).unwrap());
        // Model switching the upstream back without depending on a real en0.
        network.runtime().active_route = Some(bound);
        network.restore_gate(rollback);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn interface_path_change_invalidates_inflight_check_and_rollback() {
        let dir = temp("interface-path-change");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        let route = NetworkRoute { bind_interface: Some("en-test".into()), ..NetworkRoute::default() };
        network.save(&route).unwrap();
        network.runtime().active_route = Some(route.clone());
        open_gate(&network, &route);
        let rollback = network.rollback_point();
        let revision = route_revision(&route);
        let attempt = network.begin_check(revision.clone(), true).unwrap();
        assert!(network.path_changed());
        assert!(!network.finish_check(&attempt, passing_report(&revision), None, &[], None).unwrap());
        network.restore_gate(rollback);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.stop();
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
        };
        let missing = NetworkRoute {
            enabled: true,
            chain: vec!["gone::n1".into()],
            bind_interface: None,
        };
        assert!(network.upstream(&empty).is_err());
        assert!(network.upstream(&missing).is_err());
        let off = NetworkRoute {
            enabled: false,
            chain: vec!["gone::n1".into()],
            bind_interface: None,
        };
        assert!(matches!(network.upstream(&off).unwrap(), Upstream::Direct));
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_apply_keeps_the_previous_gateway_saved_route_and_grant() {
        let dir = temp("switch");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        open_gate(&network, &NetworkRoute::default());
        let invalid = NetworkRoute {
            enabled: true,
            chain: vec!["missing::n1".into()],
            bind_interface: None,
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
    fn sleep_and_wake_revoke_holds_and_inflight_authorization() {
        let dir = temp("sleep-gate");
        let network = GlobalNetwork::open(&dir).unwrap();
        let route = NetworkRoute::default();
        let revision = route_revision(&route);
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        open_gate(&network, &route);
        network.path_changed();
        let before_sleep = network.health_target().unwrap();
        network.system_sleep_changed(true);
        assert_eq!(network.hold_check(), None);
        assert!(network.begin_check(revision.clone(), false).is_none());
        assert!(network.health_target().is_none());
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.system_sleep_changed(false);
        assert!(!network.observe_health(&before_sleep, Ok(None)).changed);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);

        let check = network.begin_check(revision.clone(), false).unwrap();
        network.system_sleep_changed(false); // Missed sleep notification.
        assert!(!network.finish_check(&check, passing_report(&revision), None, &[], None).unwrap());
        open_gate(&network, &route);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_saved_route_change_revokes_existing_permission() {
        let dir = temp("route-drift");
        let network = GlobalNetwork::open(&dir).unwrap();
        let route = NetworkRoute::default();
        let revision = route_revision(&route);
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        open_gate(&network, &route);
        let check = network.begin_check(revision.clone(), true).unwrap();
        network.save(&NetworkRoute { bind_interface: Some("en-test".into()), ..route.clone() }).unwrap();
        assert!(!network.finish_check(&check, passing_report(&revision), None, &[], None).unwrap());
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_gateway_starts_closed_and_manual_pause_is_durable() {
        let dir = temp("gate");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
        let status = network.status(true).unwrap();
        assert_eq!(status.gate, NetworkGateMode::Closed);
        assert!(status.next_retry_at.is_some());
        network.set_paused(true).unwrap();
        assert!(network.status(true).unwrap().paused);
        drop(network);
        let network = GlobalNetwork::open(&dir).unwrap();
        assert!(network.status(false).unwrap().paused);
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
        let check = network.check(&NetworkRoute { enabled: true, chain, bind_interface: None }).unwrap();
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
        network.with_store(|store| store.upsert_proxy_source(&source)).unwrap();
        let bound = NetworkRoute { bind_interface: Some("en-test".into()), ..NetworkRoute::default() };
        network.save(&bound).unwrap();
        assert!(network.download_proxy().is_err(), "a missing gateway cannot fall back to direct");

        // Start a local gateway and model the active bound route without using
        // a real host interface or making external network requests.
        network.start_with(&vm_core::sockets::Endpoint::Tcp(0), 0, &NetworkRoute::default()).unwrap();
        network.runtime().active_route = Some(bound.clone());
        assert!(network.download_proxy().is_err(), "a closed gate blocks downloads");
        assert!(network.import("blocked", Some(&url), None).is_err());
        assert!(network.refresh(&catalog.sources[0].id, true).is_err());
        open_gate(&network, &bound);
        let gateway_url = network.runtime().gateway.as_ref().unwrap().proxy_url().unwrap();
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
        };
        network.save(&route).unwrap();
        network.start(&vm_core::sockets::Endpoint::Tcp(0), 0).unwrap();
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
