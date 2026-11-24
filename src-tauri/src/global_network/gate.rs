//! The internet gate: checks, grants, rollback points and preferences.
use std::net::Ipv4Addr;
use std::time::Instant;

use agent_core::model::{
    GeoInfo, GuestNetworkProbe, NetworkCheckReport, NetworkConnectivity, NetworkGateMode,
    NetworkRoute,
};
use agent_core::util::now_millis;
use vm_core::netgw::Gateway;

use super::{AUTO_RECOVER, CheckAttempt, GlobalNetwork, PAUSED, REGION_FOLLOWS_EXIT, RollbackPoint, Runtime, error};
use crate::app_error::AppError;
#[cfg(test)]
use crate::network_gate::GateState;
use crate::network_health::HoldCheck;

impl GlobalNetwork {
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
    pub(super) fn gate_snapshot(&self) -> GateState {
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
        baseline: Option<&super::ExitBaseline>,
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
        if attempt.held && public_ip.zip(runtime.verified_exit).is_none() {
            return Err(error("Could not confirm the verified public exit"));
        }
        if report.can_open && let (Some(route), Some(baseline)) = (runtime.active_route.as_ref(), baseline) {
            if self.exit_baseline(route)?.as_ref() != Some(baseline) {
                self.save_exit_baseline(route, baseline)?;
            }
        }
        if let Some(route) = candidate.filter(|_| report.can_open) {
            self.save(route)?;
        }
        let opened = runtime.grant.finish_observation(&attempt.token, report);
        if opened {
            runtime.connectivity.clear();
            runtime.hold_timeout_pending = false;
            runtime.verified_exit = public_ip;
            runtime.exit_policy_error = None;
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

    /// Whether a check that failed only on the machine's time zone may set
    /// the machine to the verified exit's zone (on unless turned off).
    pub fn region_follows_exit(&self) -> bool {
        self.with_store(|store| store.setting::<bool>(REGION_FOLLOWS_EXIT))
            .ok()
            .flatten()
            .unwrap_or(true)
    }

    pub fn set_region_follows_exit(&self, enabled: bool) -> Result<(), AppError> {
        let runtime = self.runtime();
        self.with_store(|store| store.set_setting(REGION_FOLLOWS_EXIT, &enabled))?;
        self.journal(
            &runtime,
            "regionPreference",
            if enabled { "The machine's time zone follows the exit" } else { "The machine's time zone is set by hand" },
            None,
        );
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

    pub(super) fn expire(&self, runtime: &mut Runtime) -> bool {
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

    pub(super) fn revoke_drifted_route(&self, runtime: &mut Runtime, saved: Option<&NetworkRoute>) -> bool {
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
        let exit_blocked = runtime.exit_policy_error.as_ref().is_some_and(|(revision, _)|
            self.route().is_ok_and(|route| super::route_revision(&route) == *revision));
        let due = !exit_blocked && !runtime.sleeping && runtime.connectivity.mode == NetworkConnectivity::Normal
            && runtime
                .gateway
                .as_ref()
                .is_some_and(Gateway::connected)
            && runtime.grant.due(now_millis());
        (due, expired || drifted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_network::test_support::*;
    use crate::network_gate::route_revision;

    #[test]
    fn a_saved_route_change_revokes_existing_permission() {
        let dir = temp("route-drift");
        let network = GlobalNetwork::open(&dir).unwrap();
        let route = NetworkRoute::default();
        let revision = route_revision(&route);
        network.start(&dir.join("gateway.sock"), 0).unwrap();
        open_gate(&network, &route);
        let check = network.begin_check(revision.clone(), true).unwrap();
        network.save(&NetworkRoute { bind_interface: Some("en-test".into()), ..route.clone() }).unwrap();
        assert!(!network.finish_check(&check, passing_report(&revision), None, &[], None, None).unwrap());
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_new_gateway_starts_closed_and_manual_pause_is_durable() {
        let dir = temp("gate");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&dir.join("gateway.sock"), 0).unwrap();
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
}
