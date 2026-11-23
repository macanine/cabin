//! Connectivity holds, rechecks, expiry and sleep.
use agent_core::util::LockExt;
use std::net::Ipv4Addr;
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use agent_core::model::{NetworkConnectivity, NetworkGateMode};

use super::{CheckAttempt, GlobalNetwork, HealthChange, HealthTarget, Runtime};
use crate::network_health::HoldCheck;

impl GlobalNetwork {
    pub fn expire_hold_now(&self) -> bool {
        self.expire_hold(&mut self.runtime(), Instant::now())
    }

    pub(super) fn expire_hold(&self, runtime: &mut Runtime, now: Instant) -> bool {
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
                    (Some(old), Some(new)) if !crate::network_gate::same_exit(old, new) => {
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

    pub(super) fn start_hold(&self, runtime: &mut Runtime, check: HoldCheck, reason: String) -> bool {
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

    pub(super) fn cancel_hold(&self, runtime: &mut Runtime) {
        if runtime.connectivity.check.is_some() {
            runtime.connectivity.clear();
            runtime.path_revision += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_network::test_support::*;
    use agent_core::model::NetworkRoute;
    use crate::network_gate::route_revision;

    #[test]
    fn connectivity_suspends_retries_and_rejects_stale_results() {
        let dir = temp("health");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&dir.join("gateway.sock"), 0).unwrap();
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
        network.start(&dir.join("gateway.sock"), 0).unwrap();
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
        network.start(&dir.join("gateway.sock"), 0).unwrap();
        open_gate(&network, &NetworkRoute::default());
        let exit = Ipv4Addr::new(203, 0, 113, 7);
        network.runtime().verified_exit = Some(exit);
        assert!(network.path_changed());
        let target = || network.health_target().unwrap();
        assert!(network.observe_health(&target(), Ok(Some(exit))).changed);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Open);
        assert!(network.path_changed());
        // A neighbour from the same NAT pool is the same exit.
        assert!(network.observe_health(&target(), Ok(Some(Ipv4Addr::new(203, 0, 113, 142)))).changed);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Open);
        assert!(network.path_changed());
        let wrong_exit = Ipv4Addr::new(203, 0, 114, 8);
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
        network.start(&dir.join("gateway.sock"), 0).unwrap();
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
        network.start(&dir.join("gateway.sock"), 0).unwrap();
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
        network.start(&dir.path().join("gateway.sock"), 0).unwrap();
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
        network.start(&dir.join("gateway.sock"), 0).unwrap();
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
    fn sleep_and_wake_revoke_holds_and_inflight_authorization() {
        let dir = temp("sleep-gate");
        let network = GlobalNetwork::open(&dir).unwrap();
        let route = NetworkRoute::default();
        let revision = route_revision(&route);
        network.start(&dir.join("gateway.sock"), 0).unwrap();
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
}
