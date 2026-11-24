use agent_core::model::{
    BrowserEnvironmentProbe, BrowserEnvironmentState, BrowserLive, GuestNetworkProbe, NetworkCheckCategory,
    NetworkCheckItem, NetworkCheckStatus, NetworkEvidence,
};

use super::exit::normalized_zone;
use super::report::{evidence, item};

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

pub(super) fn browser_revision_material(browser: Option<&BrowserEnvironmentProbe>) -> Option<String> {
    let browser = browser?;
    // Only what decides the gate: the live browser coming and going (closed,
    // asleep, reopened) must not invalidate a valid grant.
    serde_json::to_string(&(browser.enabled, &browser.expected, browser.installed.as_ref())).ok()
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
pub(super) fn browser_checks(
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

#[cfg(test)]
mod tests {
    use super::super::{CheckInputs, evaluate_report, revision_for};
    use super::super::test_support::*;
    use super::*;
    use agent_core::model::{NetworkCheckReport, NetworkRoute};

    fn browser_state(timezone: &str) -> BrowserEnvironmentState {
        BrowserEnvironmentState {
            timezone: Some(timezone.into()),
            language: Some("en-US".into()),
            languages: vec!["en-US".into(), "en".into()],
            platform: Some(vm_core::arch::GuestArch::current().ua_platform().into()),
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
                exit_policy_error: None,
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
