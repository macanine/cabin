use agent_core::model::{
    BrowserEnvironmentProbe, GeoInfo, GuestNetworkProbe, NetworkCheckCategory, NetworkCheckItem, NetworkCheckReport,
    NetworkCheckStatus, NetworkEvidence, NetworkRoute, RouteCheck,
};
use agent_core::util::{new_id, now_millis};

use super::browser::browser_checks;
use super::exit::{normalized_zone, same_exit_network};
use super::revision_for;

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

pub(super) fn evidence(
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

pub(super) fn item(
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
    let same_ip = successful.len() >= 2 && ips.windows(2).all(|pair| same_exit_network(pair[0], pair[1]));
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
        .is_some_and(|ip| ips.iter().all(|sample| same_exit_network(sample, ip)))
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
    use super::super::test_support::*;
    use super::*;

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
        let guest = probe("203.0.113.8", "Asia/Tokyo");
        let samples = vec![
            Ok(geo("one", "203.0.113.8", "America/New_York")),
            Ok(geo("two", "203.0.113.8", "America/New_York")),
        ];
        let report = exit_report(&guest, &samples);
        assert!(!report.can_open);
        let timezone = report.checks.iter().find(|check| check.id == "timezone").unwrap();
        assert_eq!(timezone.status, NetworkCheckStatus::Fail);
        assert!(timezone.required);
    }
}
