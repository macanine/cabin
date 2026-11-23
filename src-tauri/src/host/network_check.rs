//! Network route checks: applying and verifying routes, following the exit
//! region, the guest probe and browser audit, rollback, and the automatic
//! recovery and health threads.

use agent_core::util::RwLockExt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::Duration;

use agent_core::model::{
    BrowserEnvironmentProbe, GuestConnection, GuestNetworkProbe, MachineState, NetworkCheckReport,
    NetworkConnectivity, NetworkGateMode, NetworkRoute, NetworkStatus,
};
use agent_core::util::now_millis;
use serde_json::json;

use super::{Host, start_gateway, unavailable};
use crate::app_error::AppError;
use crate::global_network::RollbackPoint;
use crate::network_gate::{CheckInputs, evaluate_report, route_revision};
use crate::network_health::{self, HoldCheck};

fn failure_summary(report: &NetworkCheckReport) -> String {
    let failed = report
        .checks
        .iter()
        .filter(|check| {
            check.required && check.status != agent_core::model::NetworkCheckStatus::Pass
        })
        .map(|check| check.title.as_str())
        .collect::<Vec<_>>();
    if failed.is_empty() {
        "The network did not pass its complete check".into()
    } else {
        format!("The network check did not pass: {}", failed.join(", "))
    }
}

impl Host {
    fn restore_network_transaction(
        &self,
        route: &NetworkRoute,
        gate: RollbackPoint,
    ) -> Result<(), AppError> {
        let saved = self.network.save(route);
        let switched = if self.network.is_up() {
            self.network.switch(route)
        } else {
            Ok(())
        };
        if saved.is_ok() && switched.is_ok() {
            self.network.restore_gate(gate);
            return Ok(());
        }

        // A half-restored transaction must never inherit an old grant.
        let reason = saved
            .err()
            .or_else(|| switched.err())
            .map(|error| error.message)
            .unwrap_or_else(|| "unknown rollback failure".into());
        let message = format!("The previous route could not be restored: {reason}");
        self.network.abandon_upstream(&message);
        Err(AppError { code: "network_rollback".into(), message, details: None })
    }

    fn candidate_error(&self, error: AppError, rollback: Result<(), AppError>) -> AppError {
        match rollback {
            Ok(()) => error,
            Err(rollback) => AppError {
                code: error.code,
                message: format!("{}; {}", error.message, rollback.message),
                details: error.details.or(rollback.details),
            },
        }
    }

    /// The browser's part of a network check. Reads the installed settings
    /// and looks at the live Chrome (bounded; a closed or sleeping Chrome is a
    /// normal answer). The guest display transaction that makes the browser
    /// follow the machine's time zone and language runs only when they
    /// actually differ, not on every check.
    fn browser_environment(&self, guest: &GuestNetworkProbe) -> Option<BrowserEnvironmentProbe> {
        let cfg = self.machine.config().ok()?;
        if !cfg.browser_enabled() {
            return None;
        }
        let mut probe = Self::audit_browser(&cfg);
        if let (Some(timezone), Some(language)) = (&guest.timezone, &guest.language)
            && crate::network_gate::browser_needs_follow(&probe, guest)
        {
            match vm_core::shell::browser_follow_environment(self.machine.home(), &cfg.name, timezone, language) {
                Ok(true) => {
                    if let Ok(cfg) = self.machine.config() {
                        probe = Self::audit_browser(&cfg);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    probe.error = Some(format!("The browser could not follow the machine environment: {error}"));
                }
            }
        }
        Some(probe)
    }

    pub(super) fn audit_browser(cfg: &vm_core::config::Config) -> BrowserEnvironmentProbe {
        let failed = |error: String| BrowserEnvironmentProbe {
            enabled: true,
            error: Some(error),
            ..BrowserEnvironmentProbe::default()
        };
        match vm_core::browser::audit_environment(cfg) {
            Ok(value) => serde_json::from_value(value).unwrap_or_else(|error| {
                failed(format!("The browser environment returned an invalid result: {error}"))
            }),
            Err(error) => failed(error.to_string()),
        }
    }

    pub(super) fn apply_network_route(&self, route: &NetworkRoute) -> Result<NetworkStatus, AppError> {
        self.require_gateway()?;
        let connected = {
            let machine = self.status.read_lock();
            machine.state == MachineState::Running && machine.connection == GuestConnection::Connected
        };
        if !connected || self.network.status(true)?.paused {
            self.network.save_interface(route)?;
            self.emit_network()?;
            return self.network.status(true);
        }
        self.verify_route(route, true, "candidateCheck")
    }

    /// Verify the saved route or a candidate. A valid existing grant can be
    /// renewed in place. A candidate stays closed until all checks pass.
    pub(super) fn verify_route(
        &self,
        route: &NetworkRoute,
        commit_candidate: bool,
        kind: &str,
    ) -> Result<NetworkStatus, AppError> {
        self.require_gateway()?;
        let previous_route = self.network.route()?;
        let mut previous_gate = None;
        let result = self.verify_route_attempt(route, commit_candidate, &previous_route, &mut previous_gate, kind);
        match result {
            Err(error) if commit_candidate => {
                let error = if let Some(point) = previous_gate {
                    let rollback = self.restore_network_transaction(&previous_route, point);
                    let message = if rollback.is_ok() { "Previous route restored" } else { "Previous route could not be restored" };
                    self.network.record_check("routeRollback", message, None);
                    self.candidate_error(error, rollback)
                } else {
                    error
                };
                let _ = self.emit_network();
                Err(error)
            }
            other => other,
        }
    }

    fn verify_route_attempt(
        &self,
        route: &NetworkRoute,
        commit_candidate: bool,
        previous_route: &NetworkRoute,
        previous_gate: &mut Option<RollbackPoint>,
        kind: &str,
    ) -> Result<NetworkStatus, AppError> {
        let cfg = self.machine.config().map_err(Self::vm_error)?;
        let profile_revision = cfg.environment_revision().map_err(Self::vm_error)?;
        self.network.observe_environment_revision(Some(profile_revision.clone()));
        if self.status.read_lock().state != MachineState::Running {
            return Err(unavailable("Start the machine before checking its network"));
        }
        let state = self.network.status(true)?;
        if state.paused {
            return Err(unavailable("Resume internet access before checking the route"));
        }
        let held = self.network.hold_check();
        let candidate_changed = commit_candidate && route != previous_route;
        if !candidate_changed && matches!(held, Some(HoldCheck::Reachability | HoldCheck::Guest)) {
            self.network.wake_health();
            return Ok(state);
        }
        if let Err(error) = self.network.upstream(route) {
            if !candidate_changed && held.is_none() {
                self.network.fail_route_preflight(&error.message);
                let _ = self.emit_network();
            }
            return Err(error);
        }
        let started_at = now_millis();
        // Diagnose a broken candidate before disturbing the working path.
        let candidate_check = if candidate_changed && route.enabled {
            let check = self.network.check(route).inspect_err(|e| {
                self.network.record_check(kind, &e.message, None);
            })?;
            if let Some(index) = check.broken_at {
                let message = format!("Route hop {} did not pass its transport probe", index + 1);
                self.network.record_check(kind, &message, None);
                return Err(AppError {
                    code: "network_check".into(),
                    message,
                    details: serde_json::to_string(&check).ok(),
                });
            }
            Some(check)
        } else {
            None
        };
        if commit_candidate {
            *previous_gate = Some(self.network.rollback_point());
        }
        if candidate_changed {
            self.network.invalidate_gate("Internet access is paused while the candidate route is checked");
        }
        let prepared = if self.network.is_up() {
            self.network.switch(route)
        } else {
            start_gateway(&self.network, &cfg, Some(route))
        };
        if let Err(error) = prepared {
            if !candidate_changed && held.is_none() {
                self.network.fail_route_preflight(&error.message);
                let _ = self.emit_network();
            }
            return Err(error);
        }
        let Some(attempt) = self.network.begin_check(route_revision(route), !candidate_changed) else {
            return Err(unavailable("The network changed before verification could start. Check again when the connection is ready."));
        };
        self.notify_hold_timeout();
        self.emit_network()?;

        let observed = std::thread::scope(|scope| {
            let exits = std::thread::Builder::new().name("network-exit-samples".into())
                .spawn_scoped(scope, || self.network.exit_samples());
            let route_check = std::thread::Builder::new().name("network-route-check".into())
                .spawn_scoped(scope, || {
                    if candidate_check.is_some() || !route.enabled {
                        Ok(candidate_check)
                    } else {
                        self.network.check(route).map(Some)
                    }
                });
            let probe = self.guest_network_probe(attempt.checks_closed_boundary(), Duration::from_secs(20));
            let browser = probe.as_ref().ok().and_then(|probe| self.browser_environment(probe));
            // Join both before propagating an error. An unjoined panic would
            // otherwise unwind the scope and leave the gate in checking mode.
            let exits = exits
                .map_err(|error| unavailable(format!("Could not start exit sampling: {error}")))
                .and_then(|worker| worker.join().map_err(|_| unavailable("Exit sampling stopped"))?);
            let route_check = route_check
                .map_err(|error| unavailable(format!("Could not start route checking: {error}")))
                .and_then(|worker| worker.join().map_err(|_| unavailable("Route checking stopped"))?);
            Ok::<_, AppError>((probe?, browser, exits?, route_check?))
        });
        let (mut probe, browser, mut exits, route_check) = match observed {
            Ok(observed) => observed,
            Err(error) => {
                if attempt.refresh {
                    if self.network.recheck_verification(&attempt, HoldCheck::Verification, error.message.clone()) {
                        self.stop_chats_after_hold();
                    }
                } else {
                    self.network.fail_check(&attempt, &error.message);
                }
                self.network.record_check(kind, &error.message, None);
                self.emit_network()?;
                return Err(error);
            }
        };
        self.network.reuse_exit_samples(&attempt, &probe, &mut exits);
        let evaluate = |probe: &GuestNetworkProbe, browser: Option<&BrowserEnvironmentProbe>| {
            evaluate_report(
                CheckInputs {
                    route,
                    profile_revision: Some(&profile_revision),
                    route_check: route_check.as_ref(),
                    gateway_running: self.network.is_up(),
                    gateway_connected: self.network.gateway_connected(),
                    guest: probe,
                    require_closed_boundary: attempt.checks_closed_boundary(),
                    browser,
                    exits: &exits,
                },
                started_at,
            )
        };
        let mut report = evaluate(&probe, browser.as_ref());
        if !report.can_open
            && let Some(followed) = self.follow_exit_region(&report, &exits, &probe, attempt.checks_closed_boundary())
        {
            let browser = self.browser_environment(&followed);
            report = evaluate(&followed, browser.as_ref());
            probe = followed;
        }
        let verification = if report.can_open { "Network and environment checks passed".into() } else { failure_summary(&report) };
        self.network.record_check(kind, &verification, Some(&report));
        self.sync_environment_revision();
        if attempt.refresh && !report.can_open {
            let check = if report.checks.iter().filter(|check| {
                check.required && check.status != agent_core::model::NetworkCheckStatus::Pass
            }).all(|check| matches!(check.id.as_str(), "dns" | "transport" | "boundary" | "gateway")) {
                HoldCheck::Guest
            } else {
                HoldCheck::Verification
            };
            if self.network.recheck_verification(&attempt, check, verification.clone()) {
                self.stop_chats_after_hold();
            }
            self.emit_network()?;
            return self.network.status(true);
        }
        let opened = match self.network.finish_check(
            &attempt,
            report.clone(),
            probe.public_ip.as_deref().and_then(|ip| ip.parse().ok()),
            &exits,
            commit_candidate.then_some(route),
        ) {
            Ok(opened) => opened,
            Err(error) if attempt.refresh => {
                if self.network.recheck_verification(&attempt, HoldCheck::Verification, error.message) {
                    self.stop_chats_after_hold();
                }
                false
            }
            Err(error) => return Err(error),
        };
        if commit_candidate && !opened {
            return Err(AppError {
                code: "network_check".into(),
                message: if report.can_open { "The network changed before the candidate could be committed".into() } else { verification },
                details: serde_json::to_string(&report).ok(),
            });
        }
        if commit_candidate {
            let message = if route.enabled {
                format!("Applied a proxy route with {} hops", route.chain.len())
            } else {
                "Applied the direct route".into()
            };
            self.network.record_check("routeApplied", &message, None);
        }
        self.emit_network()?;
        let status = self.network.status(true)?;
        if state.gate == NetworkGateMode::Open && status.gate == NetworkGateMode::Closed
            && status.connectivity == NetworkConnectivity::Normal && !opened
        {
            self.fail_guest_chats(format!("The machine's internet access was blocked. {verification}"));
        }
        Ok(status)
    }

    /// When `report` failed only because the machine's time zone differs from
    /// the verified exit (see `network_gate::region_follow_zone`) and the user
    /// lets the region follow the exit, set the guest to the exit's zone and
    /// return a fresh probe to evaluate again. The language is kept. Every
    /// outcome is recorded in the network history.
    fn follow_exit_region(
        &self,
        report: &agent_core::model::NetworkCheckReport,
        exits: &[Result<agent_core::model::GeoInfo, String>],
        probe: &GuestNetworkProbe,
        check_boundary: bool,
    ) -> Option<GuestNetworkProbe> {
        if !self.network.region_follows_exit() {
            return None;
        }
        let zone = crate::network_gate::region_follow_zone(report, exits)?;
        let language = probe.language.clone()?;
        let applied = self
            .call_guest_with_timeout(
                "environment_apply",
                json!({ "spec": { "timezone": zone, "language": language } }),
                Some(Duration::from_secs(120)),
            )
            .and_then(|record| {
                let record: agent_core::model::OperationRecord = serde_json::from_value(record)
                    .map_err(|error| unavailable(format!("The machine returned an invalid operation: {error}")))?;
                match record.state {
                    agent_core::model::OperationState::Succeeded => Ok(()),
                    _ => Err(unavailable(record.error.unwrap_or_else(|| "The time zone change did not finish".into()))),
                }
            });
        match applied {
            Ok(()) => {
                self.network.record_check("regionFollowed", &format!("The machine's time zone now follows the exit: {zone}"), None);
                self.guest_network_probe(check_boundary, Duration::from_secs(20)).ok()
            }
            Err(error) => {
                self.network.record_check(
                    "regionFollowFailed",
                    &format!("The machine's time zone could not follow the exit ({zone}): {}", error.message),
                    None,
                );
                None
            }
        }
    }

    fn guest_network_probe(&self, check_boundary: bool, timeout: Duration) -> Result<GuestNetworkProbe, AppError> {
        let probe = self.call_guest_with_timeout(
            "network_probe", json!({ "checkClosedBoundary": check_boundary }), Some(timeout),
        )?;
        serde_json::from_value(probe).map_err(|error| {
            unavailable(format!("The machine returned an invalid probe result: {error}"))
        })
    }

    fn retry_guest_network(&self) -> Result<Option<std::net::Ipv4Addr>, String> {
        let probe = self.guest_network_probe(true, Duration::from_secs(7)).map_err(|error| error.message)?;
        if !probe.dns_ok {
            return Err("The machine could not resolve the check hostname".into());
        }
        if !(probe.connect_ok && probe.https_ok) {
            return Err(probe.error.unwrap_or_else(|| "The machine could not complete the HTTPS check".into()));
        }
        if !(probe.private_blocked && probe.ipv6_blocked && probe.udp_blocked && probe.unlisted_blocked == Some(true)) {
            return Err("The machine's traffic boundary did not pass".into());
        }
        Ok(probe.public_ip.and_then(|ip| ip.parse().ok()))
    }

    pub(super) fn verify_current_network(&self) -> Result<NetworkStatus, AppError> {
        self.verify_route(&self.network.route()?, false, "startupCheck")
    }

    fn recover_network_if_due(&self) {
        // Detect changes to non-default adapters even during a route transaction.
        if self.network.poll_interface_paths() {
            let _ = self.emit_network();
        }
        // Expiry also runs while a different verification owns the transaction.
        let expired_here = self.network.expire_hold_now();
        if expired_here {
            let _ = self.emit_network();
        }
        if self.status.read_lock().connection != GuestConnection::Connected {
            return;
        }
        let Ok(_transaction) = self.network.transaction.try_lock() else { return; };
        // Keep delivery ordered with verification so a late notice cannot stop
        // chats that started after a fresh grant opened the route.
        if self.notify_hold_timeout() && !expired_here {
            let _ = self.emit_network();
        }
        self.sync_environment_revision();
        let (due, expired) = self.network.gate_due();
        if expired {
            let _ = self.emit_network();
        }
        if due && let Err(error) = self.network.route().and_then(|route| self.verify_route(&route, false, "automaticCheck")) {
            cabin_diagnostics::event(cabin_diagnostics::Level::Warn, "network", "automatic_check_failed", format_args!("{}", error.code));
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn install_network_power_watch(self: &Arc<Self>) {
        let host = Arc::downgrade(self);
        network_health::install_power_watch(move |sleeping| {
            if let Some(host) = host.upgrade() {
                host.network.system_sleep_changed(sleeping);
                if !sleeping {
                    host.wake();
                }
            }
        });
    }

    pub(super) fn start_network_recovery(self: &Arc<Self>) -> Result<(), AppError> {
        self.start_network_health()?;
        let weak = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("network-authorization".into())
            .spawn(move || {
                while let Some(me) = weak.upgrade() {
                    if me.stopping.load(Ordering::Relaxed) { break; }
                    me.recover_network_if_due();
                    drop(me);
                    std::thread::sleep(Duration::from_secs(2));
                }
            })
            .map_err(|error| unavailable(format!("Could not start network authorization: {error}")))?;
        Ok(())
    }

    fn start_network_health(self: &Arc<Self>) -> Result<(), AppError> {
        let weak = Arc::downgrade(self);
        let (sender, signal) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("network-connectivity".into())
            .spawn(move || {
                #[cfg(target_os = "macos")]
                let _watch = {
                    let weak = weak.clone();
                    network_health::PathWatch::new(move || {
                        if let Some(host) = weak.upgrade() {
                            host.network.path_changed();
                            host.network.wake_health();
                        }
                    })
                };
                let mut delay = network_health::CHECK_INTERVAL;
                loop {
                    if matches!(signal.recv_timeout(delay), Ok(()))
                        && let Some(me) = weak.upgrade()
                    {
                        let _ = me.emit_network();
                        me.recover_network_if_due();
                    }
                    let Some(me) = weak.upgrade() else { break; };
                    if me.stopping.load(Ordering::Relaxed) { break; }
                    delay = network_health::CHECK_INTERVAL;
                    if let Some(target) = me.network.health_target() {
                        if target.check == Some(HoldCheck::Verification) {
                            // Updates, manual checks and this retry share the same transaction.
                            if let Ok(_transaction) = me.network.transaction.try_lock()
                                && me.network.held_target_is_current(&target)
                            {
                                let result = match &target.proxy_url {
                                    Ok(_) => me.network.route().and_then(|route| {
                                        me.verify_route(&route, false, "holdCheck")
                                    }).map(|_| ()).map_err(|error| error.message),
                                    Err(reason) => Err(reason.clone()),
                                };
                                if let Err(reason) = result {
                                    if me.network.fail_held_preflight(&target, reason) {
                                        me.stop_chats_after_hold();
                                    }
                                    let _ = me.emit_network();
                                }
                            }
                        } else {
                            let proxy = target.proxy_url.as_deref().map_err(String::clone);
                            let change = if target.check == Some(HoldCheck::Guest) {
                                let result = proxy.and_then(|_| me.retry_guest_network());
                                me.network.observe_guest_health(&target, result)
                            } else {
                                let result = proxy.and_then(network_health::probe);
                                me.network.observe_health(&target, result)
                            };
                            if change.changed {
                                let _ = me.emit_network();
                            }
                            if change.hard_block {
                                me.stop_chats_after_hold();
                            }
                        }
                        if me.network.hold_check().is_some() {
                            delay = network_health::RETRY_INTERVAL;
                        }
                    }
                }
            })
            .map_err(|error| unavailable(format!("Could not start network monitoring: {error}")))?;
        self.network.set_health_wake(sender);
        Ok(())
    }

    fn notify_hold_timeout(&self) -> bool {
        if !self.network.take_hold_timeout_notice() {
            return false;
        }
        self.fail_guest_chats("The network recheck timed out. Internet access is blocked.");
        true
    }

    fn stop_chats_after_hold(&self) {
        self.fail_guest_chats("Three network rechecks failed. Internet access is blocked and active agent requests were stopped.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use vm_core::machine::{InstallOptions, Machine};

    #[test]
    fn network_apply_defers_only_interface_changes_without_live_verification() {
        for (running, paused) in [(false, false), (true, false), (true, true)] {
            let home = tempfile::tempdir().unwrap();
            let machine = Machine::new(home.path(), concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
            machine.install_config(&InstallOptions::default()).unwrap().save_instance_meta().unwrap();
            let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
            if running {
                host.network.start(&home.path().join("gateway.sock"), 0).unwrap();
                host.network.set_paused(paused).unwrap();
                host.update(|status| {
                    status.state = MachineState::Running;
                    status.connection = if paused { GuestConnection::Connected } else { GuestConnection::Offline };
                });
            }
            let bound = NetworkRoute { bind_interface: Some("en-missing".into()), ..NetworkRoute::default() };
            host.network.save(&bound).unwrap();
            let proxy = NetworkRoute { enabled: true, chain: vec!["test".into()], ..bound.clone() };
            assert!(host.call("network_apply", json!({ "route": proxy })).is_err());
            assert_eq!(host.network.route().unwrap(), bound);

            let ssh = host.network.ssh_port();
            let system = NetworkRoute::default();
            let reply = host.call("network_apply", json!({ "route": system })).unwrap();
            let status: NetworkStatus = serde_json::from_value(reply).unwrap();
            assert_eq!(status.route, system);
            assert_eq!(status.gate, NetworkGateMode::Closed);
            assert_eq!(status.paused, paused);
            assert_eq!(host.network.ssh_port(), ssh);
            assert_eq!(host.network.route().unwrap(), system);
            host.shutdown();
        }
    }

    #[test]
    fn an_unreadable_machine_config_closes_a_running_gateway() {
        let home = tempfile::tempdir().unwrap();
        let machine = Machine::new(home.path(), concat!(env!("CARGO_MANIFEST_DIR"), "/../vm"));
        let host = Host::new(machine, PathBuf::from("/missing/agentd")).unwrap();
        let route = NetworkRoute::default();
        host.network.start(&home.path().join("gateway.sock"), 0).unwrap();
        host.network.begin_check(route_revision(&route), false).unwrap();
        assert_eq!(host.network.status(true).unwrap().gate, NetworkGateMode::Checking);
        host.sync_environment_revision();
        let status = host.network.status(true).unwrap();
        assert_eq!(status.gate, NetworkGateMode::Closed);
        assert!(status.gate_error.unwrap().contains("cannot be verified"));
    }
}
