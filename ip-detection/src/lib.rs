//! Detect the current outbound IP address and its provider.
//!
//! This is a standalone, blocking library. See the package README for proxy
//! configuration and the FlClash source order.

mod mapping;
mod quality;
mod race;
mod resolver;

pub use quality::{Classification, QualityService};

use std::fmt;
use std::io::Read;
use std::net::IpAddr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// FlClash v0.9.4 public address and location probes, in dispatch order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Service {
    Cloudflare,
    IpSb,
    IpInfo,
    IpWhoIs,
    GeoJs,
    CountryIs,
    IpQuery,
    IdentMe,
}

impl Service {
    pub const ALL: [Self; 8] = [
        Self::Cloudflare,
        Self::IpSb,
        Self::IpInfo,
        Self::IpWhoIs,
        Self::GeoJs,
        Self::CountryIs,
        Self::IpQuery,
        Self::IdentMe,
    ];
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cloudflare => "Cloudflare",
            Self::IpSb => "IP.SB",
            Self::IpInfo => "IPinfo",
            Self::IpWhoIs => "IPWHOIS",
            Self::GeoJs => "GeoJS",
            Self::CountryIs => "country.is",
            Self::IpQuery => "IPQuery",
            Self::IdentMe => "ident.me",
        }
    }
    pub const fn url(self) -> &'static str {
        match self {
            Self::Cloudflare => "https://www.cloudflare.com/cdn-cgi/trace",
            Self::IpSb => "https://api.ip.sb/geoip",
            Self::IpInfo => "https://ipinfo.io/json",
            Self::IpWhoIs => "https://ipwho.is",
            Self::GeoJs => "https://get.geojs.io/v1/ip/geo.json",
            Self::CountryIs => "https://api.country.is",
            Self::IpQuery => "https://api.ipquery.io/?format=json",
            Self::IdentMe => "https://ident.me/json",
        }
    }

    /// The endpoint that describes a specific address, when supported.
    pub fn lookup_url(self, ip: IpAddr) -> Option<String> {
        match self {
            Self::IpSb => Some(format!("https://api.ip.sb/geoip/{ip}")),
            Self::IpInfo => Some(format!("https://ipinfo.io/{ip}/json")),
            Self::IpWhoIs => Some(format!("https://ipwho.is/{ip}")),
            Self::GeoJs => Some(format!("https://get.geojs.io/v1/ip/geo/{ip}.json")),
            Self::CountryIs => Some(format!("https://api.country.is/{ip}")),
            Self::IpQuery => Some(format!("https://api.ipquery.io/{ip}?format=json")),
            Self::Cloudflare | Self::IdentMe => None,
        }
    }
}

/// Classification explicitly reported by the provider, or inferred from its
/// fixed-line ISP classification when it also rules out mobile and hosting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IpType {
    Mobile,
    Residential,
    Commercial,
    Server,
}

/// Normalized information returned by one service. Missing fields remain
/// `None`, so missing coordinates cannot be mistaken for 0° latitude/longitude.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpInfo {
    pub ip: IpAddr,
    pub country_code: Option<String>,
    pub continent_code: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    /// The provider or organization as reported by the address source.
    pub organization: Option<String>,
    pub asn: Option<u32>,
    pub asn_organization: Option<String>,
    pub longitude: Option<f64>,
    pub latitude: Option<f64>,
    pub timezone: Option<String>,
    #[serde(default)]
    pub ip_type: Option<IpType>,
    pub source: Service,
    pub fetched_at_unix_ms: u64,
}

impl IpInfo {
    /// Whether the answer says where the address is (not just what it is).
    pub fn has_location(&self) -> bool {
        self.country_code.is_some()
            || self.country.is_some()
            || self.city.is_some()
            || (self.latitude.is_some() && self.longitude.is_some())
    }

    pub fn isp(&self) -> Option<&str> {
        self.organization.as_deref()
    }
}

/// A failure from an individual service, after any transport retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceFailure {
    pub service: Service,
    pub attempts: usize,
    pub kind: FailureKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureKind {
    Transport(String),
    HttpStatus(u16),
    InvalidResponse(String),
}

impl fmt::Display for ServiceFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: ", self.service.url())?;
        match &self.kind {
            FailureKind::Transport(message) => write!(f, "network error: {message}"),
            FailureKind::HttpStatus(status) => write!(f, "HTTP {status}"),
            FailureKind::InvalidResponse(message) => write!(f, "invalid response: {message}"),
        }
    }
}

/// All configured services failed; each individual reason is available.
#[derive(Debug)]
pub struct DetectionError {
    pub failures: Vec<ServiceFailure>,
}

impl fmt::Display for DetectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "all IP detection services failed")?;
        if let Some(last) = self.failures.last() {
            write!(f, "; last error: {last}")?;
        }
        Ok(())
    }
}

impl std::error::Error for DetectionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigurationError {
    NoServices,
    ZeroTimeout,
    TimeoutTooLarge,
    InvalidProxy,
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoServices => write!(f, "at least one IP detection service is required"),
            Self::ZeroTimeout => write!(f, "the request timeout must be greater than zero"),
            Self::TimeoutTooLarge => write!(f, "the request timeout is too large"),
            Self::InvalidProxy => write!(f, "invalid or unsupported proxy URL"),
        }
    }
}

impl std::error::Error for ConfigurationError {}

#[derive(Debug)]
enum ProxyMode {
    Environment,
    Direct,
    Explicit(String),
}

/// Builds a detector. By default requests use proxy environment variables, a
/// six-second address deadline, no retries and FlClash’s eight address sources.
pub struct DetectorBuilder {
    services: Vec<Service>,
    timeout: Duration,
    retries: usize,
    retry_delay: Duration,
    proxy: ProxyMode,
    ipv4_only: bool,
}

impl Default for DetectorBuilder {
    fn default() -> Self {
        Self {
            services: Service::ALL.to_vec(),
            timeout: Duration::from_secs(6),
            retries: 0,
            retry_delay: Duration::from_secs(1),
            proxy: ProxyMode::Environment,
            ipv4_only: false,
        }
    }
}

impl DetectorBuilder {
    pub fn services(mut self, services: impl IntoIterator<Item = Service>) -> Self {
        self.services = services.into_iter().collect();
        self
    }

    /// Maximum duration of a single HTTP request, including its response body.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Number of retries after a transport failure, per service.
    pub fn retries(mut self, retries: usize) -> Self {
        self.retries = retries;
        self
    }

    /// Initial retry delay. Later retries back off with jitter, capped at 4s.
    pub fn retry_delay(mut self, retry_delay: Duration) -> Self {
        self.retry_delay = retry_delay;
        self
    }

    /// Route all requests through an HTTP or SOCKS proxy.
    pub fn proxy_url(mut self, proxy_url: impl Into<String>) -> Self {
        self.proxy = ProxyMode::Explicit(proxy_url.into());
        self
    }

    /// Ignore proxy environment variables and connect directly.
    pub fn direct(mut self) -> Self {
        self.proxy = ProxyMode::Direct;
        self
    }

    /// Only connect to IPv4 addresses. The standalone library remains dual-stack by default.
    pub fn ipv4_only(mut self) -> Self {
        self.ipv4_only = true;
        self
    }

    pub fn build(self) -> Result<Detector, ConfigurationError> {
        if self.services.is_empty() {
            return Err(ConfigurationError::NoServices);
        }
        if self.timeout.is_zero() {
            return Err(ConfigurationError::ZeroTimeout);
        }
        if Instant::now().checked_add(self.timeout).is_none() {
            return Err(ConfigurationError::TimeoutTooLarge);
        }

        let ipv4_only = self.ipv4_only;
        let agent = ureq::AgentBuilder::new()
            .redirects(0)
            .timeout(self.timeout)
            .timeout_connect(self.timeout)
            .resolver(move |netloc: &str| resolver::resolve(netloc, ipv4_only))
            .user_agent(concat!("cabin-ip-detection/", env!("CARGO_PKG_VERSION")));
        let agent = match self.proxy {
            ProxyMode::Environment => agent.try_proxy_from_env(true),
            ProxyMode::Direct => agent.try_proxy_from_env(false),
            ProxyMode::Explicit(proxy) => {
                let proxy = validate_proxy(&proxy)?;
                agent.try_proxy_from_env(false).proxy(proxy)
            }
        };

        Ok(Detector {
            agent: agent.build(),
            services: self.services,
            retries: self.retries,
            retry_delay: self.retry_delay,
            timeout: self.timeout,
            ipv4_only: self.ipv4_only,
        })
    }
}

fn validate_proxy(proxy: &str) -> Result<ureq::Proxy, ConfigurationError> {
    let url = url::Url::parse(proxy).map_err(|_| ConfigurationError::InvalidProxy)?;
    if !matches!(url.scheme(), "http" | "socks4" | "socks4a" | "socks5")
        || url.host_str().is_none()
        || matches!(url.host(), Some(url::Host::Ipv6(_)))
        || url.port() == Some(0)
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigurationError::InvalidProxy);
    }
    // ureq 2's proxy parser does not handle IPv6 proxy hosts correctly.
    ureq::Proxy::new(proxy).map_err(|_| ConfigurationError::InvalidProxy)
}

/// A reusable blocking HTTP client for IP detection.
#[derive(Clone)]
pub struct Detector {
    agent: ureq::Agent,
    services: Vec<Service>,
    retries: usize,
    retry_delay: Duration,
    timeout: Duration,
    ipv4_only: bool,
}

#[derive(Clone, Copy)]
struct Endpoint<'a> {
    service: Service,
    url: &'a str,
}

impl Detector {
    pub fn builder() -> DetectorBuilder {
        DetectorBuilder::default()
    }

    /// First valid public address. Slow sources are hedged in FlClash order.
    pub fn detect(&self) -> Result<IpInfo, DetectionError> {
        let detector = self.clone();
        let outcomes = race::race(
            self.services.iter().copied(),
            race::Schedule::address(self.timeout),
            move |service, timeout| {
                detector
                    .try_endpoint_for(
                        Endpoint {
                            service,
                            url: service.url(),
                        },
                        Some(timeout),
                    )
                    .and_then(|info| detector.require_public(info))
            },
            Result::is_ok,
            |service, error| Err(ServiceFailure::spawn_failed(service, error)),
        );
        let mut failures = Vec::new();
        for answer in outcomes {
            match answer {
                Ok(info) => return Ok(info),
                Err(failure) => failures.push(failure),
            }
        }
        Err(DetectionError { failures })
    }

    fn require_public(&self, info: IpInfo) -> Result<IpInfo, ServiceFailure> {
        if public_ip(info.ip) && (!self.ipv4_only || info.ip.is_ipv4()) {
            return Ok(info);
        }
        Err(ServiceFailure {
            service: info.source,
            attempts: 1,
            kind: FailureKind::InvalidResponse("expected a public address".into()),
        })
    }

    /// Query one specific service without falling back to another.
    pub fn detect_service(&self, service: Service) -> Result<IpInfo, DetectionError> {
        self.try_endpoint_for(
            Endpoint {
                service,
                url: service.url(),
            },
            None,
        )
        .and_then(|info| self.require_public(info))
        .map_err(|error| DetectionError {
            failures: vec![error],
        })
    }

    /// Describe a specific address, preserving the same deadline as detection.
    pub fn lookup(&self, ip: IpAddr) -> Result<IpInfo, DetectionError> {
        let urls = self
            .services
            .iter()
            .copied()
            .filter_map(|service| service.lookup_url(ip).map(|url| (service, url)));
        let detector = self.clone();
        let outcomes = race::race(
            urls,
            race::Schedule::address(self.timeout),
            move |(service, url), timeout| {
                let result =
                    detector.try_endpoint_for(Endpoint { service, url: &url }, Some(timeout))?;
                if result.ip == ip {
                    Ok(result)
                } else {
                    Err(ServiceFailure {
                        service,
                        attempts: 1,
                        kind: FailureKind::InvalidResponse(
                            "answered for a different address".into(),
                        ),
                    })
                }
            },
            |answer| answer.as_ref().is_ok_and(IpInfo::has_location),
            |(service, _), error| Err(ServiceFailure::spawn_failed(service, error)),
        );
        let mut bare = None;
        let mut failures = Vec::new();
        for answer in outcomes {
            match answer {
                Ok(info) if info.has_location() => return Ok(info),
                Ok(info) => {
                    bare.get_or_insert(info);
                }
                Err(error) => failures.push(error),
            }
        }
        bare.ok_or(DetectionError { failures })
    }

    #[cfg(test)]
    fn detect_endpoints<'a>(
        &self,
        endpoints: impl IntoIterator<Item = Endpoint<'a>>,
    ) -> Result<IpInfo, DetectionError> {
        self.detect_matching(endpoints, None)
    }

    /// First valid answer; with `expected`, only an answer about that address.
    #[cfg(test)]
    fn detect_matching<'a>(
        &self,
        endpoints: impl IntoIterator<Item = Endpoint<'a>>,
        expected: Option<IpAddr>,
    ) -> Result<IpInfo, DetectionError> {
        let mut failures = Vec::new();
        // An answer with the address but no location is kept only as a last
        // resort; another service may know where it is.
        let mut bare = None;
        for endpoint in endpoints {
            match self.try_endpoint(endpoint) {
                Ok(info) if expected.is_some_and(|ip| ip != info.ip) => {
                    failures.push(ServiceFailure {
                        service: endpoint.service,
                        attempts: 1,
                        kind: FailureKind::InvalidResponse(format!(
                            "answered for {} instead",
                            info.ip
                        )),
                    })
                }
                Ok(info) if !info.has_location() => {
                    bare.get_or_insert(info);
                }
                Ok(info) => return Ok(info),
                Err(error) => failures.push(error),
            }
        }
        bare.ok_or(DetectionError { failures })
    }

    #[cfg(test)]
    fn try_endpoint(&self, endpoint: Endpoint<'_>) -> Result<IpInfo, ServiceFailure> {
        self.try_endpoint_for(endpoint, None)
    }

    fn try_endpoint_for(
        &self,
        endpoint: Endpoint<'_>,
        budget: Option<Duration>,
    ) -> Result<IpInfo, ServiceFailure> {
        let started = Instant::now();
        let remaining = || budget.map(|budget| budget.saturating_sub(started.elapsed()));
        let mut attempts = 0usize;
        loop {
            let timeout = remaining().unwrap_or(self.timeout);
            if timeout.is_zero() {
                return Err(ServiceFailure {
                    service: endpoint.service,
                    attempts,
                    kind: FailureKind::Transport("address detection deadline elapsed".into()),
                });
            }
            attempts = attempts.saturating_add(1);
            match request(&self.agent, endpoint.url, timeout) {
                Ok(response) if response.status() == 200 => {
                    let result = read_body(response)
                        .and_then(|bytes| mapping::parse_body(endpoint.service, &bytes));
                    return result.map_err(|message| ServiceFailure {
                        service: endpoint.service,
                        attempts,
                        kind: FailureKind::InvalidResponse(message),
                    });
                }
                Ok(response) => {
                    return Err(ServiceFailure {
                        service: endpoint.service,
                        attempts,
                        kind: FailureKind::HttpStatus(response.status()),
                    });
                }
                Err(kind) => {
                    if matches!(kind, FailureKind::Transport(_)) && attempts <= self.retries {
                        let exponent = (attempts - 1).min(2) as u32;
                        let base = self
                            .retry_delay
                            .saturating_mul(1 << exponent)
                            .min(Duration::from_secs(4));
                        let max = base.saturating_mul(2).min(Duration::from_secs(4));
                        let jitter =
                            Duration::from_secs_f64((max - base).as_secs_f64() * fastrand::f64());
                        let delay = base + jitter;
                        if remaining().is_none_or(|left| delay < left) {
                            std::thread::sleep(delay);
                            continue;
                        }
                    }
                    return Err(ServiceFailure {
                        service: endpoint.service,
                        attempts,
                        kind,
                    });
                }
            }
        }
    }
}

fn request(
    agent: &ureq::Agent,
    url: &str,
    timeout: Duration,
) -> Result<ureq::Response, FailureKind> {
    resolver::with_timeout(timeout, || {
        agent
            .get(url)
            .timeout(timeout)
            .call()
            .map_err(|error| match error {
                ureq::Error::Status(status, _) => FailureKind::HttpStatus(status),
                ureq::Error::Transport(error) => FailureKind::Transport(error.to_string()),
            })
    })
}

impl ServiceFailure {
    fn spawn_failed(service: Service, error: std::io::Error) -> Self {
        Self {
            service,
            attempts: 0,
            kind: FailureKind::Transport(format!("could not start request worker: {error}")),
        }
    }
}

fn read_body(response: ureq::Response) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err("response exceeded 1 MiB".into());
    }
    Ok(bytes)
}

/// Decode an IP and its country from a Cloudflare trace response.
pub fn parse_cloudflare_trace(body: &[u8]) -> Result<IpInfo, String> {
    mapping::parse_body(Service::Cloudflare, body)
}

pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(address) => {
            let [first, second, ..] = address.octets();
            !(first == 0
                || first >= 224
                || (first == 100 && (64..128).contains(&second))
                || (first == 198 && matches!(second, 18 | 19))
                || address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_documentation())
        }
        IpAddr::V6(address) => {
            let segments = address.segments();
            // Limit the result to currently allocated global-unicast space.
            segments[0] & 0xe000 == 0x2000 && !(segments[0] == 0x2001 && segments[1] == 0x0db8)
        }
    }
}

fn now_unix_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
