use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

// --------------------------------------------------------------------- proxy

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxySource {
    pub id: String,
    pub name: String,
    pub has_url: bool,
    pub node_count: u32,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyNode {
    pub id: String,
    pub name: String,
    pub source_id: String,
    pub source_name: String,
    pub protocol: String,
    /// Opaque identity of this node's current configuration. The route id can
    /// be reused when a subscription changes.
    pub config_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyGroup {
    pub id: String,
    pub name: String,
    pub node_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyCatalog {
    pub sources: Vec<ProxySource>,
    pub nodes: Vec<ProxyNode>,
    #[serde(default)]
    pub groups: Vec<ProxyGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ProxyTestKind {
    Latency,
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    pub node_id: String,
    pub config_revision: String,
    pub kind: ProxyTestKind,
    #[cfg_attr(test, ts(type = "number"))]
    pub tested_at: i64,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub bytes: Option<u32>,
    pub elapsed_ms: Option<u32>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub detail: String,
}

// ------------------------------------------------------------------ network

/// The VM's route. The VM's only NIC goes to shoes on the Mac; with
/// `enabled` off shoes sends it straight out (still cut off from the LAN and
/// the Mac), otherwise through `chain`: catalog node ids, entry first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkRoute {
    pub enabled: bool,
    pub chain: Vec<String>,
    /// Host interface for direct traffic or the first proxy hop; None uses the system route.
    #[serde(default)]
    pub bind_interface: Option<String>,
    #[serde(default)]
    pub exit_policy: ExitPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ExitPolicy {
    #[default]
    Static,
    DynamicResidential,
}

/// How traffic from the machine is handled right now. `checking` is a short,
/// restricted window that only admits the fixed verification targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkGateMode {
    #[default]
    Closed,
    Checking,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkCheckStatus {
    Pass,
    Fail,
    Unknown,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkCheckCategory {
    Route,
    Gateway,
    Dns,
    Transport,
    Boundary,
    Exit,
    Environment,
}

/// One observed fact behind a check result. Evidence is always shown with its
/// source and sampling time so missing or stale data is not mistaken for a
/// successful result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkEvidence {
    pub label: String,
    pub value: String,
    pub source: String,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub observed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkCheckItem {
    pub id: String,
    pub title: String,
    pub category: NetworkCheckCategory,
    pub status: NetworkCheckStatus,
    pub required: bool,
    pub summary: String,
    pub evidence: Vec<NetworkEvidence>,
}

/// A complete evaluation for one immutable route revision. A report with any
/// required `fail` or `unknown` item cannot open the gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkCheckReport {
    pub id: String,
    pub revision: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub finished_at: i64,
    pub checks: Vec<NetworkCheckItem>,
    pub passed_required: u32,
    pub total_required: u32,
    pub can_open: bool,
}

/// Connectivity of the current network path, independent of the environment grant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkConnectivity {
    #[default]
    Normal,
    Rechecking,
    Blocked,
}

/// Traffic delivered to or accepted from the VM by its internet gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkTraffic {
    #[cfg_attr(test, ts(type = "number"))]
    pub gateway_session: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub sampled_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub rx_bytes: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub tx_bytes: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub rx_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub tx_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkHistoryEntry {
    #[cfg_attr(test, ts(type = "number"))]
    pub id: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub kind: String,
    pub message: String,
    pub failed_checks: u32,
    pub report: Option<NetworkCheckReport>,
}

/// The gateway as it runs now. `available` is false where there is no
/// gateway (a local guest, or no machine yet).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    pub available: bool,
    pub route: NetworkRoute,
    pub running: bool,
    pub error: Option<String>,
    /// The latest gate transition or verification failure. Kept separate from
    /// `error`, which describes the gateway process itself.
    pub gate_error: Option<String>,
    /// A confirmed violation of the saved exit policy.
    pub exit_policy_error: Option<String>,
    pub gate: NetworkGateMode,
    #[serde(default)]
    pub connectivity: NetworkConnectivity,
    #[serde(default)]
    pub failed_checks: u32,
    pub connectivity_error: Option<String>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub gateway_session: Option<u64>,
    pub paused: bool,
    pub auto_recover: bool,
    /// When a check fails only because the machine's time zone differs from
    /// the verified exit, the Mac sets the machine to the exit's zone and
    /// checks again.
    #[serde(default = "network_status_true")]
    pub region_follows_exit: bool,
    /// Stable identity of the route and guest environment this grant covers.
    pub revision: String,
    /// Monotonic in-process token used to discard stale checks.
    #[cfg_attr(test, ts(type = "number"))]
    pub generation: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub checked_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub expires_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub next_retry_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number"))]
    pub retry_attempt: u32,
    pub report: Option<NetworkCheckReport>,
}

fn network_status_true() -> bool {
    true
}

impl Default for NetworkStatus {
    fn default() -> Self {
        Self {
            available: false,
            route: NetworkRoute::default(),
            running: false,
            error: None,
            gate_error: None,
            exit_policy_error: None,
            gate: NetworkGateMode::Closed,
            connectivity: NetworkConnectivity::Normal,
            failed_checks: 0,
            connectivity_error: None,
            gateway_session: None,
            paused: false,
            auto_recover: true,
            region_follows_exit: true,
            revision: String::new(),
            generation: 0,
            checked_at: None,
            expires_at: None,
            next_retry_at: None,
            retry_attempt: 0,
            report: None,
        }
    }
}

/// Facts read by the fixed guest-side probe. The probe never accepts a host,
/// port, command, path or proxy setting from its caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GuestNetworkProbe {
    #[cfg_attr(test, ts(type = "number"))]
    pub checked_at: i64,
    pub hostname: Option<String>,
    pub username: Option<String>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub dns_ok: bool,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub dns_ms: Option<u32>,
    pub connect_ok: bool,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub connect_ms: Option<u32>,
    pub https_ok: bool,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub https_ms: Option<u32>,
    pub public_ip: Option<String>,
    pub trace_location: Option<String>,
    pub ipv6_blocked: bool,
    pub private_blocked: bool,
    /// Only applicable while unrestricted public traffic is closed.
    pub unlisted_blocked: Option<bool>,
    pub udp_blocked: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum IpType {
    Mobile,
    Residential,
    Commercial,
    Server,
    Unknown,
}

/// Network type information from a separate lookup about the verified address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IpClassification {
    pub ip_type: IpType,
    pub inferred: bool,
    pub organization: Option<String>,
    pub asn: Option<u32>,
    pub source: String,
}

/// Where a public address is, from one public lookup service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GeoInfo {
    pub ip: String,
    pub country_code: Option<String>,
    pub continent_code: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    /// Provider (ISP or organisation), as the service names it.
    pub isp: Option<String>,
    pub ip_type: Option<IpType>,
    pub asn: Option<u32>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub timezone: Option<String>,
    /// The service that answered.
    pub source: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub checked_at: i64,
}

/// One prefix of a chain: the Mac through hops `0..=index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct HopCheck {
    pub node_id: String,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub detail: String,
}

/// A chain checked prefix by prefix. `broken_at` is the first hop that
/// could not be reached (the link into it is the broken one); later hops
/// were not tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct RouteCheck {
    pub hops: Vec<HopCheck>,
    pub broken_at: Option<u32>,
    #[cfg_attr(test, ts(type = "number"))]
    pub checked_at: i64,
}

/// Where a node's server is (its host resolved on the Mac, then looked up).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NodePlace {
    pub node_id: String,
    pub host: String,
    pub geo: Option<GeoInfo>,
    pub error: Option<String>,
}
