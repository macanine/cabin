//! In-memory authorization for machine internet access.
//!
//! A route change invalidates the old grant immediately. Checks use a
//! generation token so a late result cannot open a different revision. The
//! state only opens after every required observation is current and passing.

use agent_core::model::{
    BrowserEnvironmentProbe, BrowserEnvironmentState, BrowserLive, GeoInfo, GuestNetworkProbe, NetworkCheckCategory, NetworkCheckItem,
    NetworkCheckReport, NetworkCheckStatus, NetworkEvidence, NetworkGateMode, NetworkRoute,
    RouteCheck,
};
use agent_core::util::{new_id, now_millis, sha256_text};

pub const GRANT_TTL_MS: i64 = 5 * 60 * 1000;
pub const CHECK_TTL_MS: i64 = 3 * 60 * 1000;
const RENEW_BEFORE_MS: i64 = 60 * 1000;
const RETRY_BASE_MS: i64 = 15 * 1000;
const RETRY_MAX_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateToken {
    pub revision: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateState {
    pub mode: NetworkGateMode,
    pub paused: bool,
    pub auto_recover: bool,
    pub revision: String,
    environment_revision: Option<String>,
    pub generation: u64,
    pub checked_at: Option<i64>,
    check_deadline: Option<i64>,
    pub expires_at: Option<i64>,
    pub next_retry_at: Option<i64>,
    pub retry_attempt: u32,
    pub report: Option<NetworkCheckReport>,
    pub error: Option<String>,
}

impl Default for GateState {
    fn default() -> Self {
        Self {
            mode: NetworkGateMode::Closed,
            paused: false,
            auto_recover: true,
            revision: String::new(),
            environment_revision: None,
            generation: 0,
            checked_at: None,
            check_deadline: None,
            expires_at: None,
            next_retry_at: None,
            retry_attempt: 0,
            report: None,
            error: None,
        }
    }
}

impl GateState {
    pub fn with_preferences(paused: bool, auto_recover: bool) -> Self {
        let now = now_millis();
        Self {
            paused,
            auto_recover,
            next_retry_at: (!paused).then_some(now),
            error: paused.then(|| "Internet access is paused".into()),
            ..Self::default()
        }
    }

    pub fn set_revision(&mut self, revision: String) -> bool {
        if self.revision == revision {
            return false;
        }
        self.revision = revision;
        self.generation = self.generation.saturating_add(1);
        self.mode = NetworkGateMode::Closed;
        self.checked_at = None;
        self.check_deadline = None;
        self.expires_at = None;
        self.retry_attempt = 0;
        self.next_retry_at = (!self.paused).then_some(now_millis());
        self.report = None;
        self.error = None;
        true
    }

    pub fn set_environment_revision(&mut self, revision: Option<String>) -> bool {
        if self.environment_revision == revision {
            return false;
        }
        self.environment_revision = revision;
        if self.paused {
            self.invalidate("Internet access is paused");
        } else {
            self.invalidate("Environment settings changed and need a full check");
        }
        true
    }

    pub fn invalidate(&mut self, reason: impl Into<String>) {
        self.generation = self.generation.saturating_add(1);
        self.mode = NetworkGateMode::Closed;
        self.checked_at = None;
        self.check_deadline = None;
        self.expires_at = None;
        self.retry_attempt = 0;
        self.next_retry_at = (!self.paused).then_some(now_millis());
        self.report = None;
        self.error = Some(reason.into());
    }

    pub fn begin(&mut self, revision: String) -> Option<GateToken> {
        self.set_revision(revision);
        if self.paused {
            self.mode = NetworkGateMode::Closed;
            self.next_retry_at = None;
            self.error = Some("Internet access is paused".into());
            return None;
        }
        self.generation = self.generation.saturating_add(1);
        self.mode = NetworkGateMode::Checking;
        self.checked_at = None;
        self.check_deadline = Some(now_millis().saturating_add(CHECK_TTL_MS));
        self.expires_at = None;
        self.next_retry_at = None;
        self.report = None;
        self.error = None;
        Some(GateToken {
            revision: self.revision.clone(),
            generation: self.generation,
        })
    }

    /// Revalidate an unexpired grant without disturbing live connections.
    pub fn begin_refresh(&mut self) -> GateToken {
        self.generation = self.generation.saturating_add(1);
        self.check_deadline = Some(now_millis().saturating_add(CHECK_TTL_MS));
        GateToken {
            revision: self.revision.clone(),
            generation: self.generation,
        }
    }

    pub fn matches(&self, token: &GateToken) -> bool {
        self.revision == token.revision && self.generation == token.generation
    }

    pub fn finish_observation(&mut self, token: &GateToken, report: NetworkCheckReport) -> bool {
        if !self.matches(token) {
            return false;
        }
        self.revision.clone_from(&report.revision);
        self.finish(
            &GateToken {
                revision: report.revision.clone(),
                generation: token.generation,
            },
            report,
        )
    }

    pub fn restore_if_current(&mut self, mut previous: Self) {
        if self.environment_revision != previous.environment_revision || self.paused != previous.paused {
            return;
        }
        previous.generation = self.generation.saturating_add(1);
        previous.expire(now_millis());
        *self = previous;
    }

    pub fn defer_refresh(&mut self, token: &GateToken) {
        if self.matches(token) && self.mode == NetworkGateMode::Open {
            self.check_deadline = None;
            self.next_retry_at = Some(now_millis().saturating_add(RETRY_BASE_MS));
        }
    }

    pub fn finish(&mut self, token: &GateToken, report: NetworkCheckReport) -> bool {
        if token.revision != self.revision
            || token.generation != self.generation
            || report.revision != self.revision
        {
            return false;
        }
        self.generation = self.generation.saturating_add(1);
        let now = now_millis();
        let covered_required = report.total_required > 0
            && report.total_required == report.passed_required
            && report
                .checks
                .iter()
                .filter(|check| check.required)
                .all(|check| check.status == NetworkCheckStatus::Pass);
        let open = report.can_open && covered_required && !self.paused;
        self.mode = if open {
            NetworkGateMode::Open
        } else {
            NetworkGateMode::Closed
        };
        self.checked_at = Some(report.finished_at);
        self.check_deadline = None;
        self.expires_at = open.then_some(report.finished_at.saturating_add(GRANT_TTL_MS));
        self.error = if open {
            None
        } else {
            Some("Required observations did not pass".into())
        };
        self.retry_attempt = if open {
            0
        } else {
            self.retry_attempt.saturating_add(1)
        };
        self.next_retry_at = (!open && self.auto_recover && !self.paused)
            .then_some(now.saturating_add(retry_delay(self.retry_attempt)));
        self.report = Some(report);
        open
    }

    pub fn fail(&mut self, token: &GateToken, message: impl Into<String>) {
        if self.matches(token) {
            self.record_failure(message);
        }
    }

    pub fn record_failure(&mut self, message: impl Into<String>) {
        self.generation = self.generation.saturating_add(1);
        let now = now_millis();
        self.mode = NetworkGateMode::Closed;
        self.checked_at = Some(now);
        self.check_deadline = None;
        self.expires_at = None;
        self.retry_attempt = self.retry_attempt.saturating_add(1);
        self.next_retry_at = (self.auto_recover && !self.paused)
            .then_some(now.saturating_add(retry_delay(self.retry_attempt)));
        self.error = Some(message.into());
    }

    pub fn pause(&mut self, paused: bool) {
        if self.paused == paused {
            return;
        }
        self.paused = paused;
        self.generation = self.generation.saturating_add(1);
        self.mode = NetworkGateMode::Closed;
        self.checked_at = None;
        self.check_deadline = None;
        self.expires_at = None;
        self.retry_attempt = 0;
        self.report = None;
        if paused {
            self.next_retry_at = None;
            self.error = Some("Internet access is paused".into());
        } else {
            self.next_retry_at = Some(now_millis());
            self.error = None;
        }
    }

    pub fn set_auto_recover(&mut self, enabled: bool) {
        self.auto_recover = enabled;
        if !enabled {
            self.next_retry_at = None;
        } else if self.mode == NetworkGateMode::Closed && !self.paused {
            self.next_retry_at = Some(now_millis());
        }
    }

    pub fn expire(&mut self, now: i64) -> bool {
        let expired = match self.mode {
            NetworkGateMode::Checking => {
                self.check_deadline.is_some_and(|deadline| now >= deadline)
            }
            NetworkGateMode::Open => self.expires_at.is_some_and(|expires| now >= expires),
            NetworkGateMode::Closed => false,
        };
        if !expired {
            return false;
        }
        let was_checking = self.mode == NetworkGateMode::Checking;
        self.generation = self.generation.saturating_add(1);
        self.mode = NetworkGateMode::Closed;
        self.check_deadline = None;
        self.expires_at = None;
        self.retry_attempt = 0;
        self.next_retry_at = (self.auto_recover && !self.paused).then_some(now);
        self.error = Some(if was_checking {
            "The environment check timed out".into()
        } else {
            "The previous verification has expired".into()
        });
        true
    }

    pub fn due(&self, now: i64) -> bool {
        if self.paused {
            return false;
        }
        match self.mode {
            NetworkGateMode::Closed => self.auto_recover && self.next_retry_at.is_some_and(|next| now >= next),
            NetworkGateMode::Checking => false,
            NetworkGateMode::Open => {
                self.check_deadline.is_none()
                    && self.next_retry_at.is_none_or(|next| now >= next)
                    && self.expires_at.is_some_and(|expires| now >= expires - RENEW_BEFORE_MS)
            }
        }
    }
}

pub fn route_revision(route: &NetworkRoute) -> String {
    let material = serde_json::to_vec(route).unwrap_or_default();
    sha256_text(&String::from_utf8_lossy(&material))
}

pub fn report_revision(route: &NetworkRoute, environment: &[(&str, Option<&str>)]) -> String {
    let mut material = route_revision(route);
    for (key, value) in environment {
        material.push('|');
        material.push_str(key);
        material.push('=');
        material.push_str(value.unwrap_or("-"));
    }
    sha256_text(&material)
}

fn retry_delay(attempt: u32) -> i64 {
    RETRY_BASE_MS
        .saturating_mul(1i64 << attempt.min(5))
        .min(RETRY_MAX_MS)
}

pub struct CheckInputs<'a> {
    pub route: &'a NetworkRoute,
    pub profile_revision: Option<&'a str>,
    pub route_check: Option<&'a RouteCheck>,
    pub gateway_running: bool,
    pub gateway_connected: bool,
    pub guest: &'a GuestNetworkProbe,
    pub require_closed_boundary: bool,
    pub browser: Option<&'a BrowserEnvironmentProbe>,
    pub exits: &'a [Result<GeoInfo, String>],
}

fn evidence(
    label: &str,
    value: impl Into<String>,
    source: &str,
    observed_at: i64,
) -> NetworkEvidence {
    NetworkEvidence {
        label: label.into(),
        value: value.into(),
        source: source.into(),
        observed_at: Some(observed_at),
    }
}

fn item(
    id: &str,
    title: &str,
    category: NetworkCheckCategory,
    status: NetworkCheckStatus,
    required: bool,
    summary: impl Into<String>,
    evidence: Vec<NetworkEvidence>,
) -> NetworkCheckItem {
    NetworkCheckItem {
        id: id.into(),
        title: title.into(),
        category,
        status,
        required,
        summary: summary.into(),
        evidence,
    }
}

fn normalized_zone(value: &str) -> &str {
    match value {
        "Etc/GMT" | "Etc/UTC" | "Etc/GMT+0" | "Etc/GMT-0" | "Etc/GMT0" | "Etc/UCT"
        | "Etc/Universal" | "Etc/Zulu" | "GMT" | "GMT0" | "Greenwich" | "UCT"
        | "Universal" | "Zulu" => "UTC",
        other => other,
    }
}

fn normalized_language(value: &str) -> String {
    let language = value
        .split('.')
        .next()
        .unwrap_or(value)
        .replace('_', "-")
        .to_ascii_lowercase();
    if matches!(language.as_str(), "c" | "posix") {
        "en-us".into()
    } else {
        language
    }
}

fn browser_revision_material(browser: Option<&BrowserEnvironmentProbe>) -> Option<String> {
    let browser = browser?;
    // Only what decides the gate: the live browser coming and going (closed,
    // asleep, reopened) must not invalidate a valid grant.
    serde_json::to_string(&(browser.enabled, &browser.expected, browser.installed.as_ref())).ok()
}

pub fn revision_for(
    route: &NetworkRoute,
    guest: &GuestNetworkProbe,
    browser: Option<&BrowserEnvironmentProbe>,
    profile_revision: Option<&str>,
) -> String {
    let browser = browser_revision_material(browser);
    report_revision(
        route,
        &[
            ("timezone", guest.timezone.as_deref()),
            ("language", guest.language.as_deref()),
            ("hostname", guest.hostname.as_deref()),
            ("username", guest.username.as_deref()),
            ("browser", browser.as_deref()),
            ("profile", profile_revision),
        ],
    )
}

fn same_browser_environment(left: &BrowserEnvironmentState, right: &BrowserEnvironmentState) -> bool {
    normalized_zone(left.timezone.as_deref().unwrap_or(""))
        == normalized_zone(right.timezone.as_deref().unwrap_or(""))
        && normalized_language(left.language.as_deref().unwrap_or(""))
            == normalized_language(right.language.as_deref().unwrap_or(""))
        && left.platform == right.platform
        && left.languages == right.languages
}

fn state_evidence(
    out: &mut Vec<NetworkEvidence>,
    prefix: &str,
    state: &BrowserEnvironmentState,
    source: &str,
    at: i64,
) {
    let or_unknown = |value: Option<&str>| value.unwrap_or("unknown").to_string();
    out.push(evidence(&format!("{prefix} time zone"), or_unknown(state.timezone.as_deref()), source, at));
    out.push(evidence(&format!("{prefix} language"), or_unknown(state.language.as_deref()), source, at));
    out.push(evidence(&format!("{prefix} platform"), or_unknown(state.platform.as_deref()), source, at));
    out.push(evidence(
        &format!("{prefix} languages"),
        if state.languages.is_empty() { "unknown".into() } else { state.languages.join(", ") },
        source,
        at,
    ));
}

/// Whether the live pages use the installed settings (the configured ones
/// when the installed profile could not be read); `None` without a live
/// observation.
pub fn live_matches(browser: &BrowserEnvironmentProbe) -> Option<bool> {
    let observed = browser.observed.as_ref().filter(|_| browser.live == BrowserLive::Observed)?;
    Some(same_browser_environment(browser.installed.as_ref().unwrap_or(&browser.expected), observed))
}

/// Whether the browser should be brought in line with the machine: its
/// configured time zone or language differs from the machine's, or the
/// installed profile differs from the configuration. Aliases (`Etc/UTC` and
/// `UTC`, `en_US.UTF-8` and `en-US`) count as equal.
pub fn browser_needs_follow(browser: &BrowserEnvironmentProbe, guest: &GuestNetworkProbe) -> bool {
    if !browser.enabled {
        return false;
    }
    let expected = &browser.expected;
    let machine_differs = normalized_zone(expected.timezone.as_deref().unwrap_or(""))
        != normalized_zone(guest.timezone.as_deref().unwrap_or(""))
        || normalized_language(expected.language.as_deref().unwrap_or(""))
            != normalized_language(guest.language.as_deref().unwrap_or(""));
    let installed_differs = browser
        .installed
        .as_ref()
        .is_none_or(|installed| !same_browser_environment(expected, installed));
    machine_differs || installed_differs
}

/// The browser's two checks.
///
/// `browser` (required): the installed profile matches the configuration and
/// the machine's time zone and language. It reads files, not Chrome, so a
/// closed or sleeping browser can never close the gate.
///
/// `browserLive`: what an open page reports. Only an open Chrome is checked:
/// when a page was read it is required (Chrome on other values than its
/// settings blocks, fixed by restarting Chrome); Chrome closed, asleep or
/// without a page is skipped and passes.
fn browser_checks(
    browser: Option<&BrowserEnvironmentProbe>,
    guest: &GuestNetworkProbe,
) -> Vec<NetworkCheckItem> {
    let at = guest.checked_at;
    let Some(browser) = browser.filter(|browser| browser.enabled) else {
        return vec![item(
            "browser",
            "Browser environment",
            NetworkCheckCategory::Environment,
            NetworkCheckStatus::Skipped,
            false,
            "No browser profile is configured",
            Vec::new(),
        )];
    };
    let expected = &browser.expected;
    let machine_match = normalized_zone(expected.timezone.as_deref().unwrap_or(""))
        == normalized_zone(guest.timezone.as_deref().unwrap_or(""))
        && normalized_language(expected.language.as_deref().unwrap_or(""))
            == normalized_language(guest.language.as_deref().unwrap_or(""));
    let installed_match = browser
        .installed
        .as_ref()
        .is_some_and(|installed| same_browser_environment(expected, installed));
    let ok = browser.error.is_none() && installed_match && machine_match;
    let summary = if ok {
        "Chrome's installed settings match the machine environment".to_string()
    } else if let Some(error) = browser.error.as_deref() {
        error.to_string()
    } else if !installed_match {
        "Chrome's installed settings differ from its configuration".into()
    } else {
        "Chrome's time zone or language differs from the machine's".into()
    };
    let mut settings = Vec::new();
    state_evidence(&mut settings, "Configured", expected, "browser settings", at);
    if let Some(installed) = browser.installed.as_ref() {
        state_evidence(&mut settings, "Installed", installed, "guest profile", at);
    }
    settings.push(evidence("Machine time zone", guest.timezone.as_deref().unwrap_or("unknown"), "machine environment", at));
    settings.push(evidence("Machine language", guest.language.as_deref().unwrap_or("unknown"), "machine environment", at));
    let installed_check = item(
        "browser",
        "Browser environment",
        NetworkCheckCategory::Environment,
        if ok { NetworkCheckStatus::Pass } else { NetworkCheckStatus::Fail },
        true,
        summary,
        settings,
    );

    let detail = browser.live_detail.clone();
    let (status, summary, mut live_evidence) = match (browser.live, browser.observed.as_ref()) {
        (BrowserLive::Observed, Some(observed)) => {
            let mut values = Vec::new();
            state_evidence(&mut values, "Live", observed, "browser page", at);
            if live_matches(browser) == Some(true) {
                (NetworkCheckStatus::Pass, "The open pages use the installed settings".to_string(), values)
            } else {
                (
                    NetworkCheckStatus::Fail,
                    "Chrome is running with older settings; restart it to apply them".to_string(),
                    values,
                )
            }
        }
        (BrowserLive::NotRunning, _) => (
            NetworkCheckStatus::Skipped,
            detail.clone().unwrap_or_else(|| "Chrome is not running; it starts with the installed settings".into()),
            Vec::new(),
        ),
        (BrowserLive::Idle, _) | (BrowserLive::Observed, None) => (
            NetworkCheckStatus::Skipped,
            detail.clone().unwrap_or_else(|| "No open page to inspect".into()),
            Vec::new(),
        ),
        (BrowserLive::Unavailable, _) => (
            NetworkCheckStatus::Unknown,
            detail.clone().unwrap_or_else(|| "The live browser could not be inspected".into()),
            Vec::new(),
        ),
    };
    if let (Some(detail), NetworkCheckStatus::Pass | NetworkCheckStatus::Fail) = (detail, status) {
        live_evidence.push(evidence("Note", detail, "browser", at));
    }
    let live_check = item(
        "browserLive",
        "Live browser",
        NetworkCheckCategory::Environment,
        status,
        browser.live == BrowserLive::Observed && browser.observed.is_some(),
        summary,
        live_evidence,
    );
    vec![installed_check, live_check]
}

/// Turn bounded observations into the single report that decides whether the
/// gate may open. Missing, partial and conflicting evidence never passes.
pub fn evaluate_report(inputs: CheckInputs<'_>, started_at: i64) -> NetworkCheckReport {
    let guest = inputs.guest;
    let revision = revision_for(inputs.route, guest, inputs.browser, inputs.profile_revision);
    let mut checks = Vec::new();

    let route_status = if !inputs.route.enabled
        || inputs
            .route_check
            .is_some_and(|check| check.broken_at.is_none())
    {
        NetworkCheckStatus::Pass
    } else {
        NetworkCheckStatus::Fail
    };
    let route_evidence = inputs
        .route_check
        .map(|check| {
            check
                .hops
                .iter()
                .map(|hop| {
                    evidence(
                        &format!("Hop {}", hop.node_id),
                        format!(
                            "{}{}",
                            hop.detail,
                            hop.latency_ms
                                .map(|latency| format!(" · {latency} ms"))
                                .unwrap_or_default()
                        ),
                        "route probe",
                        check.checked_at,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    checks.push(item(
        "route",
        "Route",
        NetworkCheckCategory::Route,
        route_status,
        true,
        if inputs.route.enabled {
            "Every route prefix must complete a current transport probe"
        } else {
            "Direct route selected"
        },
        route_evidence,
    ));

    checks.push(item(
        "gateway",
        "Gateway",
        NetworkCheckCategory::Gateway,
        if inputs.gateway_running && inputs.gateway_connected {
            NetworkCheckStatus::Pass
        } else {
            NetworkCheckStatus::Unknown
        },
        true,
        if inputs.gateway_running && inputs.gateway_connected {
            "The gateway is running and the machine link is connected"
        } else if inputs.gateway_running {
            "The gateway is running but its machine link was not observed"
        } else {
            "The gateway is not running"
        },
        vec![evidence(
            "Gateway state",
            if inputs.gateway_running {
                "running"
            } else {
                "stopped"
            },
            "gateway",
            guest.checked_at,
        )],
    ));
    checks.push(item(
        "dns",
        "Name resolution",
        NetworkCheckCategory::Dns,
        if guest.dns_ok {
            NetworkCheckStatus::Pass
        } else {
            NetworkCheckStatus::Fail
        },
        true,
        if guest.dns_ok {
            "A fixed name resolved through the machine path"
        } else {
            "Name resolution did not complete"
        },
        vec![evidence(
            "Resolution time",
            guest
                .dns_ms
                .map(|ms| format!("{ms} ms"))
                .unwrap_or_else(|| "not measured".into()),
            "machine probe",
            guest.checked_at,
        )],
    ));

    let transport_ok = guest.connect_ok && guest.https_ok && guest.public_ip.is_some();
    checks.push(item(
        "transport",
        "Outbound transport",
        NetworkCheckCategory::Transport,
        if transport_ok {
            NetworkCheckStatus::Pass
        } else {
            NetworkCheckStatus::Fail
        },
        true,
        if transport_ok {
            "TCP, TLS and HTTP completed through the machine path".into()
        } else {
            guest
                .error
                .clone()
                .unwrap_or_else(|| "The fixed outbound request did not complete".into())
        },
        vec![
            evidence(
                "TCP",
                guest
                    .connect_ms
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "not measured".into()),
                "machine probe",
                guest.checked_at,
            ),
            evidence(
                "TLS and HTTP",
                guest
                    .https_ms
                    .map(|ms| format!("{ms} ms"))
                    .unwrap_or_else(|| "not measured".into()),
                "machine probe",
                guest.checked_at,
            ),
            evidence(
                "Observed address",
                guest.public_ip.clone().unwrap_or_else(|| "unknown".into()),
                "fixed HTTPS response",
                guest.checked_at,
            ),
        ],
    ));

    let boundary_ok =
        guest.ipv6_blocked && guest.private_blocked && (!inputs.require_closed_boundary || guest.unlisted_blocked == Some(true)) && guest.udp_blocked;
    checks.push(item(
        "boundary",
        "Traffic boundary",
        NetworkCheckCategory::Boundary,
        if boundary_ok {
            NetworkCheckStatus::Pass
        } else {
            NetworkCheckStatus::Fail
        },
        true,
        "Unlisted IPv6, private-network and direct UDP probes must stay closed",
        vec![
            evidence(
                "IPv6",
                if guest.ipv6_blocked {
                    "closed"
                } else {
                    "reachable"
                },
                "machine probe",
                guest.checked_at,
            ),
            evidence(
                "Private networks",
                if guest.private_blocked {
                    "closed"
                } else {
                    "reachable"
                },
                "machine probe",
                guest.checked_at,
            ),
            evidence(
                "Unlisted public port",
                match guest.unlisted_blocked {
                    Some(true) => "closed",
                    Some(false) => "reachable",
                    None => "checked when granting access",
                },
                "machine probe",
                guest.checked_at,
            ),
            evidence(
                "Direct UDP",
                if guest.udp_blocked {
                    "closed"
                } else {
                    "answered"
                },
                "machine probe",
                guest.checked_at,
            ),
        ],
    ));

    let successful: Vec<(&Result<GeoInfo, String>, &GeoInfo)> = inputs
        .exits
        .iter()
        .filter_map(|result| result.as_ref().ok().map(|geo| (result, geo)))
        .collect();
    let ips: Vec<&str> = successful.iter().map(|(_, geo)| geo.ip.as_str()).collect();
    let countries: Vec<&str> = successful
        .iter()
        .filter_map(|(_, geo)| geo.country_code.as_deref())
        .collect();
    let zones: Vec<&str> = successful
        .iter()
        .filter_map(|(_, geo)| geo.timezone.as_deref())
        .collect();
    let same_ip = successful.len() >= 2 && ips.windows(2).all(|pair| pair[0] == pair[1]);
    let same_country = countries.len() >= 2
        && countries
            .windows(2)
            .all(|pair| pair[0].eq_ignore_ascii_case(pair[1]));
    let same_zone = zones.len() >= 2
        && zones
            .windows(2)
            .all(|pair| normalized_zone(pair[0]) == normalized_zone(pair[1]));
    let guest_matches = guest
        .public_ip
        .as_deref()
        .is_some_and(|ip| ips.iter().all(|sample| *sample == ip))
        && guest
            .trace_location
            .as_deref()
            .zip(countries.first().copied())
            .is_some_and(|(trace, country)| trace.eq_ignore_ascii_case(country));
    let exit_ok = same_ip && same_country && same_zone && guest_matches;
    let mut exit_evidence: Vec<NetworkEvidence> = successful
        .iter()
        .map(|(_, geo)| {
            evidence(
                &geo.source,
                format!(
                    "{} · {} · {}",
                    geo.ip,
                    geo.country_code.as_deref().unwrap_or("country unknown"),
                    geo.timezone.as_deref().unwrap_or("time zone unknown")
                ),
                "route exit sample",
                geo.checked_at,
            )
        })
        .collect();
    exit_evidence.extend(
        inputs
            .exits
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(|message| {
                evidence(
                    "Source failed",
                    message.clone(),
                    "route exit sample",
                    guest.checked_at,
                )
            }),
    );
    exit_evidence.push(evidence(
        "Machine observation",
        format!(
            "{} · {}",
            guest.public_ip.as_deref().unwrap_or("address unknown"),
            guest.trace_location.as_deref().unwrap_or("country unknown")
        ),
        "machine probe",
        guest.checked_at,
    ));
    checks.push(item(
        "exit",
        "Exit consistency",
        NetworkCheckCategory::Exit,
        if exit_ok {
            NetworkCheckStatus::Pass
        } else {
            NetworkCheckStatus::Unknown
        },
        true,
        if exit_ok {
            "Two independent samples, the machine observation, country and time zone agree"
        } else if successful.len() < 2 {
            "At least two independent exit samples are required"
        } else if !same_ip || !guest_matches {
            "The observed addresses or countries do not agree"
        } else if !same_country || !same_zone {
            "Location providers disagree on country or time zone"
        } else {
            "The exit location is incomplete"
        },
        exit_evidence,
    ));

    let exit_zone = zones.first().copied();
    let zone_pair = guest.timezone.as_deref().zip(exit_zone);
    let timezone_ok =
        zone_pair.is_some_and(|(machine, exit)| normalized_zone(machine) == normalized_zone(exit));
    // Both zones known and different is a real failure with a known fix,
    // not an unconfirmed observation.
    let timezone_status = match zone_pair {
        _ if timezone_ok => NetworkCheckStatus::Pass,
        Some(_) => NetworkCheckStatus::Fail,
        None => NetworkCheckStatus::Unknown,
    };
    checks.push(item(
        "timezone",
        "Time zone",
        NetworkCheckCategory::Environment,
        timezone_status,
        true,
        match zone_pair {
            _ if timezone_ok => "The machine time zone matches the verified exit".to_string(),
            Some((machine, exit)) => {
                format!("The VM uses {machine} but the exit is in {exit}; set the VM's time zone to {exit}")
            }
            None => "The machine or exit time zone could not be observed".to_string(),
        },
        vec![
            evidence(
                "Machine",
                guest.timezone.as_deref().unwrap_or("unknown"),
                "machine environment",
                guest.checked_at,
            ),
            evidence(
                "Exit",
                exit_zone.unwrap_or("unknown"),
                "route exit sample",
                guest.checked_at,
            ),
        ],
    ));

    let identity_ok = guest
        .hostname
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && guest
            .username
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty());
    checks.push(item(
        "identity",
        "Machine identity",
        NetworkCheckCategory::Environment,
        if identity_ok {
            NetworkCheckStatus::Pass
        } else {
            NetworkCheckStatus::Unknown
        },
        true,
        if identity_ok {
            "The machine reports a hostname and login account"
        } else {
            "The hostname or login account could not be observed"
        },
        vec![
            evidence(
                "Hostname",
                guest.hostname.as_deref().unwrap_or("unknown"),
                "machine environment",
                guest.checked_at,
            ),
            evidence(
                "Login account",
                guest.username.as_deref().unwrap_or("unknown"),
                "machine environment",
                guest.checked_at,
            ),
        ],
    ));

    let language_ok = guest
        .language
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    checks.push(item(
        "language",
        "Language",
        NetworkCheckCategory::Environment,
        if language_ok {
            NetworkCheckStatus::Pass
        } else {
            NetworkCheckStatus::Unknown
        },
        true,
        if language_ok {
            "The machine reports an active language"
        } else {
            "The machine language could not be observed"
        },
        vec![evidence(
            "Machine",
            guest.language.as_deref().unwrap_or("unknown"),
            "machine environment",
            guest.checked_at,
        )],
    ));

    checks.extend(browser_checks(inputs.browser, guest));

    let required = checks
        .iter()
        .filter(|check| check.required)
        .collect::<Vec<_>>();
    let passed_required = required
        .iter()
        .filter(|check| check.status == NetworkCheckStatus::Pass)
        .count() as u32;
    let total_required = required.len() as u32;
    NetworkCheckReport {
        id: new_id("network_check"),
        revision,
        started_at,
        finished_at: now_millis(),
        can_open: passed_required == total_required,
        checks,
        passed_required,
        total_required,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_core::model::{
        GeoInfo, GuestNetworkProbe, NetworkCheckCategory, NetworkCheckItem, NetworkCheckStatus,
        NetworkGateMode,
    };

    fn report(revision: &str, pass: bool) -> NetworkCheckReport {
        NetworkCheckReport {
            id: "check".into(),
            revision: revision.into(),
            started_at: 1,
            finished_at: now_millis(),
            checks: vec![NetworkCheckItem {
                id: "route".into(),
                title: "Route".into(),
                category: NetworkCheckCategory::Route,
                status: if pass {
                    NetworkCheckStatus::Pass
                } else {
                    NetworkCheckStatus::Unknown
                },
                required: true,
                summary: String::new(),
                evidence: Vec::new(),
            }],
            passed_required: u32::from(pass),
            total_required: 1,
            can_open: pass,
        }
    }

    #[test]
    fn a_stale_result_cannot_open_a_new_revision() {
        let mut state = GateState::default();
        let old = state.begin("old".into()).unwrap();
        let new = state.begin("new".into()).unwrap();
        assert!(!state.finish(&old, report("old", true)));
        assert_eq!(state.mode, NetworkGateMode::Checking);
        assert!(state.finish(&new, report("new", true)));
        assert_eq!(state.mode, NetworkGateMode::Open);
        assert!(state.error.is_none());
    }

    #[test]
    fn unknown_keeps_the_gate_closed_and_schedules_retry() {
        let mut state = GateState::default();
        let token = state.begin("route".into()).unwrap();
        assert!(!state.finish(&token, report("route", false)));
        assert_eq!(state.mode, NetworkGateMode::Closed);
        assert!(state.next_retry_at.is_some());
        assert!(state.error.is_some());
    }

    #[test]
    fn manual_pause_wins_and_resume_makes_a_check_due() {
        let mut state = GateState::default();
        let token = state.begin("route".into()).unwrap();
        state.finish(&token, report("route", true));
        state.pause(true);
        assert_eq!(state.mode, NetworkGateMode::Closed);
        assert!(state.next_retry_at.is_none());
        state.pause(false);
        assert!(state.due(now_millis()));
    }

    #[test]
    fn a_passing_check_clears_the_previous_error() {
        let mut state = GateState::default();
        let failed = state.begin("route".into()).unwrap();
        state.fail(&failed, "temporary failure");
        assert!(state.error.is_some());
        assert_eq!(state.mode, NetworkGateMode::Closed);

        let passing = state.begin("route".into()).unwrap();
        assert!(state.finish(&passing, report("route", true)));
        assert_eq!(state.mode, NetworkGateMode::Open);
        assert!(state.error.is_none());
        assert!(state.next_retry_at.is_none());
    }

    #[test]
    fn a_new_environment_revision_revokes_the_previous_grant() {
        let mut state = GateState::default();
        let token = state.begin("old".into()).unwrap();
        assert!(state.finish(&token, report("old", true)));
        let generation = state.generation;

        assert!(state.set_revision("new".into()));
        assert_eq!(state.mode, NetworkGateMode::Closed);
        assert_eq!(state.generation, generation + 1);
        assert!(state.checked_at.is_none());
        assert!(state.expires_at.is_none());
        assert!(state.report.is_none());
        assert!(state.due(now_millis()));
    }

    #[test]
    fn a_live_environment_change_revokes_the_previous_grant() {
        let mut state = GateState::default();
        assert!(state.set_environment_revision(Some("old".into())));
        let token = state.begin("route+old".into()).unwrap();
        assert!(state.finish(&token, report("route+old", true)));
        let generation = state.generation;

        assert!(state.set_environment_revision(Some("new".into())));
        assert_eq!(state.mode, NetworkGateMode::Closed);
        assert_eq!(state.generation, generation + 1);
        assert!(state.checked_at.is_none());
        assert!(state.expires_at.is_none());
        assert!(state.report.is_none());
        assert!(state.due(now_millis()));
    }

    #[test]
    fn a_manual_pause_prevents_a_check_from_starting() {
        let mut state = GateState::with_preferences(true, true);
        assert!(state.begin("route".into()).is_none());
        assert_eq!(state.mode, NetworkGateMode::Closed);
        assert!(!state.due(i64::MAX));
        state.pause(false);
        assert!(state.begin("route".into()).is_some());
        assert_eq!(state.mode, NetworkGateMode::Checking);
    }

    #[test]
    fn disabling_auto_recovery_does_not_stop_grant_renewal() {
        let mut state = GateState::default();
        let token = state.begin("route".into()).unwrap();
        assert!(state.finish(&token, report("route", true)));
        state.set_auto_recover(false);
        assert!(state.due(state.expires_at.unwrap() - RENEW_BEFORE_MS));
        state.invalidate("failed");
        assert!(!state.due(i64::MAX));
    }

    #[test]
    fn expiry_revokes_the_open_state() {
        let mut state = GateState::default();
        let token = state.begin("route".into()).unwrap();
        state.finish(&token, report("route", true));
        assert!(state.expire(i64::MAX));
        assert_eq!(state.mode, NetworkGateMode::Closed);
        assert!(state.due(i64::MAX));
    }

    #[test]
    fn a_check_window_expires_and_rejects_late_evidence() {
        let mut state = GateState::default();
        let token = state.begin("route".into()).unwrap();
        assert!(state.expire(i64::MAX));
        assert_eq!(state.mode, NetworkGateMode::Closed);
        assert!(state.due(i64::MAX));
        assert!(!state.finish(&token, report("route", true)));
        assert_eq!(state.mode, NetworkGateMode::Closed);
    }

    fn probe(ip: &str, zone: &str) -> GuestNetworkProbe {
        GuestNetworkProbe {
            checked_at: 1,
            hostname: Some("test".into()),
            username: Some("user".into()),
            timezone: Some(zone.into()),
            language: Some("en_US.UTF-8".into()),
            dns_ok: true,
            dns_ms: Some(2),
            connect_ok: true,
            connect_ms: Some(3),
            https_ok: true,
            https_ms: Some(4),
            public_ip: Some(ip.into()),
            trace_location: Some("US".into()),
            ipv6_blocked: true,
            private_blocked: true,
            unlisted_blocked: Some(true),
            udp_blocked: true,
            error: None,
        }
    }

    fn geo(source: &str, ip: &str, zone: &str) -> GeoInfo {
        GeoInfo {
            isp: None,
            ip_type: None,
            ip: ip.into(),
            country_code: Some("US".into()),
            country: Some("United States".into()),
            region: None,
            city: None,
            asn: None,
            latitude: None,
            longitude: None,
            timezone: Some(zone.into()),
            source: source.into(),
            checked_at: 1,
        }
    }

    #[test]
    fn independent_exit_samples_are_required_for_a_pass() {
        let route = NetworkRoute::default();
        let guest = probe("203.0.113.8", "America/New_York");
        let samples = vec![
            Ok(geo("one", "203.0.113.8", "America/New_York")),
            Ok(geo("two", "203.0.113.8", "America/New_York")),
        ];
        let report = evaluate_report(
            CheckInputs {
                route: &route,
                profile_revision: None,
                route_check: None,
                gateway_running: true,
                gateway_connected: true,
                guest: &guest,
                require_closed_boundary: true,
                browser: None,
                exits: &samples,
            },
            1,
        );
        assert!(report.can_open);
        assert_eq!(report.passed_required, report.total_required);

        let partial = evaluate_report(
            CheckInputs {
                route: &route,
                profile_revision: None,
                route_check: None,
                gateway_running: true,
                gateway_connected: true,
                guest: &guest,
                require_closed_boundary: true,
                browser: None,
                exits: &samples[..1],
            },
            1,
        );
        assert!(!partial.can_open);
        assert_eq!(
            partial
                .checks
                .iter()
                .find(|check| check.id == "exit")
                .unwrap()
                .status,
            NetworkCheckStatus::Unknown
        );
    }

    #[test]
    fn a_time_zone_mismatch_keeps_the_gate_closed() {
        let route = NetworkRoute::default();
        let guest = probe("203.0.113.8", "Asia/Tokyo");
        let samples = vec![
            Ok(geo("one", "203.0.113.8", "America/New_York")),
            Ok(geo("two", "203.0.113.8", "America/New_York")),
        ];
        let report = evaluate_report(
            CheckInputs {
                route: &route,
                profile_revision: None,
                route_check: None,
                gateway_running: true,
                gateway_connected: true,
                guest: &guest,
                require_closed_boundary: true,
                browser: None,
                exits: &samples,
            },
            1,
        );
        assert!(!report.can_open);
        assert_eq!(
            report
                .checks
                .iter()
                .find(|check| check.id == "timezone")
                .unwrap()
                .status,
            NetworkCheckStatus::Fail
        );
    }

    fn browser_state(timezone: &str) -> BrowserEnvironmentState {
        BrowserEnvironmentState {
            timezone: Some(timezone.into()),
            language: Some("en-US".into()),
            languages: vec!["en-US".into(), "en".into()],
            platform: Some("Linux aarch64".into()),
        }
    }

    fn browser_report(guest: &GuestNetworkProbe, browser: &BrowserEnvironmentProbe) -> NetworkCheckReport {
        let route = NetworkRoute::default();
        let samples = vec![
            Ok(geo("one", "203.0.113.8", "America/New_York")),
            Ok(geo("two", "203.0.113.8", "America/New_York")),
        ];
        evaluate_report(
            CheckInputs {
                route: &route,
                profile_revision: None,
                route_check: None,
                gateway_running: true,
                gateway_connected: true,
                guest,
                require_closed_boundary: true,
                browser: Some(browser),
                exits: &samples,
            },
            1,
        )
    }

    fn check<'a>(report: &'a NetworkCheckReport, id: &str) -> &'a NetworkCheckItem {
        report.checks.iter().find(|check| check.id == id).unwrap()
    }

    #[test]
    fn a_closed_or_sleeping_browser_never_blocks_the_machine() {
        let guest = probe("203.0.113.8", "America/New_York");
        for (live, detail) in [
            (BrowserLive::NotRunning, "Chrome is not running"),
            (BrowserLive::Idle, "Chrome's pages are asleep"),
            (BrowserLive::Unavailable, "No page answered in time"),
        ] {
            let browser = BrowserEnvironmentProbe {
                enabled: true,
                expected: browser_state("America/New_York"),
                installed: Some(browser_state("America/New_York")),
                live,
                observed: None,
                live_detail: Some(detail.into()),
                error: None,
            };
            let report = browser_report(&guest, &browser);
            assert!(report.can_open, "{live:?} must not block");
            assert_eq!(check(&report, "browser").status, NetworkCheckStatus::Pass);
            let live_check = check(&report, "browserLive");
            assert!(!live_check.required);
            assert_eq!(live_check.summary, detail);
        }
    }

    #[test]
    fn an_open_browser_on_old_values_blocks() {
        let guest = probe("203.0.113.8", "America/New_York");
        let browser = BrowserEnvironmentProbe {
            enabled: true,
            expected: browser_state("America/New_York"),
            installed: Some(browser_state("America/New_York")),
            live: BrowserLive::Observed,
            observed: Some(browser_state("Asia/Tokyo")),
            live_detail: None,
            error: None,
        };
        let report = browser_report(&guest, &browser);
        assert!(!report.can_open);
        let live = check(&report, "browserLive");
        assert!(live.required);
        assert_eq!(live.status, NetworkCheckStatus::Fail);
        assert!(live.summary.contains("restart"));

        // The same Chrome on the right values passes.
        let matching = BrowserEnvironmentProbe { observed: Some(browser_state("America/New_York")), ..browser };
        assert!(browser_report(&guest, &matching).can_open);
    }

    #[test]
    fn installed_settings_that_differ_still_block() {
        let guest = probe("203.0.113.8", "America/New_York");
        let browser = BrowserEnvironmentProbe {
            enabled: true,
            expected: browser_state("America/New_York"),
            installed: Some(browser_state("Asia/Tokyo")),
            live: BrowserLive::NotRunning,
            observed: None,
            live_detail: None,
            error: None,
        };
        let report = browser_report(&guest, &browser);
        assert!(!report.can_open);
        assert_eq!(check(&report, "browser").status, NetworkCheckStatus::Fail);
    }

    #[test]
    fn browser_evidence_explains_a_hidden_machine_mismatch() {
        let mut guest = probe("203.0.113.8", "America/New_York");
        guest.language = Some("de_DE.UTF-8".into());
        let browser = BrowserEnvironmentProbe {
            enabled: true,
            expected: browser_state("America/New_York"),
            installed: Some(browser_state("America/New_York")),
            live: BrowserLive::NotRunning,
            observed: None,
            live_detail: None,
            error: None,
        };
        let report = browser_report(&guest, &browser);
        let item = check(&report, "browser");
        assert_eq!(item.status, NetworkCheckStatus::Fail);
        assert!(item.summary.contains("differs from the machine's"));
        assert!(item.evidence.iter().any(|entry| entry.label == "Machine language" && entry.value == "de_DE.UTF-8"));
        assert!(item.evidence.iter().any(|entry| entry.label == "Installed languages"));
    }

    #[test]
    fn the_live_browser_is_not_part_of_the_gate_revision() {
        let guest = probe("203.0.113.8", "America/New_York");
        let asleep = BrowserEnvironmentProbe {
            enabled: true,
            expected: browser_state("America/New_York"),
            installed: Some(browser_state("America/New_York")),
            live: BrowserLive::Idle,
            observed: None,
            live_detail: Some("Chrome's pages are asleep".into()),
            error: None,
        };
        let awake = BrowserEnvironmentProbe {
            live: BrowserLive::Observed,
            observed: Some(browser_state("America/New_York")),
            live_detail: None,
            ..asleep.clone()
        };
        let route = NetworkRoute::default();
        assert_eq!(
            revision_for(&route, &guest, Some(&asleep), None),
            revision_for(&route, &guest, Some(&awake), None)
        );
    }

    #[test]
    fn the_default_c_locale_matches_the_browser_default_language() {
        assert_eq!(normalized_language("C.UTF-8"), "en-us");
        assert_eq!(normalized_language("C"), "en-us");
        assert_eq!(normalized_language("POSIX"), "en-us");
        assert_eq!(normalized_language("en_US.UTF-8"), "en-us");
    }
}
