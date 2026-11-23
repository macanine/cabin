//! In-memory authorization for machine internet access.
//!
//! A route change invalidates the old grant immediately. Checks use a
//! generation token so a late result cannot open a different revision. The
//! state only opens after every required observation is current and passing.

mod browser;
mod exit;
mod report;
#[cfg(test)]
mod test_support;

use agent_core::model::{
    BrowserEnvironmentProbe, GuestNetworkProbe, NetworkCheckReport, NetworkCheckStatus, NetworkGateMode, NetworkRoute,
};
use agent_core::util::{now_millis, sha256_text};
use browser::browser_revision_material;

pub use browser::{browser_needs_follow, live_matches};
pub use exit::{region_follow_zone, same_exit};
pub use report::{CheckInputs, evaluate_report};

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

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

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
}
