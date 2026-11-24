//! Shared builders for the network gate tests.

use agent_core::model::{
    GeoInfo, GuestNetworkProbe, NetworkCheckCategory, NetworkCheckItem, NetworkCheckReport, NetworkCheckStatus,
    NetworkRoute,
};
use agent_core::util::now_millis;

use super::report::{CheckInputs, evaluate_report};

pub(super) fn report(revision: &str, pass: bool) -> NetworkCheckReport {
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

pub(super) fn probe(ip: &str, zone: &str) -> GuestNetworkProbe {
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

pub(super) fn geo(source: &str, ip: &str, zone: &str) -> GeoInfo {
    GeoInfo {
        isp: None,
        ip_type: None,
        ip: ip.into(),
        country_code: Some("US".into()),
        continent_code: Some("NA".into()),
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

pub(super) fn exit_report(guest: &GuestNetworkProbe, samples: &[Result<GeoInfo, String>]) -> NetworkCheckReport {
    evaluate_report(
        CheckInputs {
            route: &NetworkRoute::default(),
            profile_revision: None,
            route_check: None,
            gateway_running: true,
            gateway_connected: true,
            guest,
            require_closed_boundary: true,
            browser: None,
            exits: samples,
            exit_policy_error: None,
        },
        1,
    )
}
