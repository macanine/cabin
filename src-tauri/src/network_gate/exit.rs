use agent_core::model::{GeoInfo, NetworkCheckReport, NetworkCheckStatus};

/// The time zone the machine should take when a check failed only because
/// the machine's region differs from a verified exit: the exit check passed,
/// the time zone check failed, and every other failing required check is one
/// that follows the machine's zone (the browser profile). The gate itself is
/// unchanged: the caller applies the zone in the guest and checks again.
pub fn region_follow_zone(
    report: &NetworkCheckReport,
    exits: &[Result<GeoInfo, String>],
) -> Option<String> {
    let status = |id: &str| report.checks.iter().find(|check| check.id == id).map(|check| check.status);
    if status("exit") != Some(NetworkCheckStatus::Pass) || status("timezone") != Some(NetworkCheckStatus::Fail) {
        return None;
    }
    let follows_machine = ["timezone", "browser", "browserLive"];
    if report.checks.iter().any(|check| {
        check.required && check.status != NetworkCheckStatus::Pass && !follows_machine.contains(&check.id.as_str())
    }) {
        return None;
    }
    exits
        .iter()
        .filter_map(|sample| sample.as_ref().ok())
        .find_map(|geo| geo.timezone.clone())
        .filter(|zone| !zone.trim().is_empty())
}

/// Two observations of one exit. Carrier-grade and campus NAT hand
/// consecutive connections addresses from a pool (`…120.14` then `…120.142`),
/// so IPv4 addresses in one /24 count as the same exit. Anything else must
/// match exactly.
pub(super) fn same_exit_network(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (a.parse::<std::net::Ipv4Addr>(), b.parse::<std::net::Ipv4Addr>()) {
        (Ok(a), Ok(b)) => same_exit(a, b),
        _ => false,
    }
}

/// [`same_exit_network`] for parsed addresses; the health monitor and the
/// grant refresh use it so every comparison of the verified exit agrees.
pub fn same_exit(a: std::net::Ipv4Addr, b: std::net::Ipv4Addr) -> bool {
    a.octets()[..3] == b.octets()[..3]
}

pub(super) fn normalized_zone(value: &str) -> &str {
    match value {
        "Etc/GMT" | "Etc/UTC" | "Etc/GMT+0" | "Etc/GMT-0" | "Etc/GMT0" | "Etc/UCT"
        | "Etc/Universal" | "Etc/Zulu" | "GMT" | "GMT0" | "Greenwich" | "UCT"
        | "Universal" | "Zulu" => "UTC",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;

    #[test]
    fn only_a_region_mismatch_on_a_verified_exit_is_followed() {
        let samples = vec![
            Ok(geo("one", "203.0.113.8", "America/New_York")),
            Ok(geo("two", "203.0.113.8", "America/New_York")),
        ];
        let utc = probe("203.0.113.8", "Etc/UTC");
        assert_eq!(
            region_follow_zone(&exit_report(&utc, &samples), &samples).as_deref(),
            Some("America/New_York")
        );
        // Already matching: nothing to follow.
        let matching = probe("203.0.113.8", "America/New_York");
        assert_eq!(region_follow_zone(&exit_report(&matching, &samples), &samples), None);
        // The exit itself is unverified (machine sees another address): never follow it.
        let other_exit = probe("198.51.100.9", "Etc/UTC");
        assert_eq!(region_follow_zone(&exit_report(&other_exit, &samples), &samples), None);
        // Another required failure (DNS) must be fixed first.
        let mut no_dns = probe("203.0.113.8", "Etc/UTC");
        no_dns.dns_ok = false;
        assert_eq!(region_follow_zone(&exit_report(&no_dns, &samples), &samples), None);
    }

    #[test]
    fn a_nat_pool_is_one_exit_but_another_network_is_not() {
        let guest = probe("222.205.120.14", "Asia/Shanghai");
        let pool = vec![
            Ok(geo("one", "222.205.120.142", "Asia/Shanghai")),
            Ok(geo("two", "222.205.120.14", "Asia/Shanghai")),
        ];
        assert!(exit_report(&guest, &pool).can_open);
        let elsewhere = vec![
            Ok(geo("one", "222.205.121.14", "Asia/Shanghai")),
            Ok(geo("two", "222.205.120.14", "Asia/Shanghai")),
        ];
        assert!(!exit_report(&guest, &elsewhere).can_open);
    }
}
