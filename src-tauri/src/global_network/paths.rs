//! The Mac interface paths behind a bound route.
use std::collections::BTreeMap;

use vm_core::network_interfaces::{self, InterfacePath};

use super::{GlobalNetwork, Runtime};
use crate::network_health::HoldCheck;

impl GlobalNetwork {
    pub fn poll_interface_paths(&self) -> bool {
        self.refresh_interface_paths(&mut self.runtime())
    }

    pub(super) fn refresh_interface_paths(&self, runtime: &mut Runtime) -> bool {
        runtime.gateway.is_some()
            && self
                .observe_interface_paths(runtime, network_interfaces::paths().unwrap_or_default())
    }

    fn observe_interface_paths(
        &self,
        runtime: &mut Runtime,
        paths: BTreeMap<String, InterfacePath>,
    ) -> bool {
        let previous = std::mem::replace(&mut runtime.interface_paths, paths);
        if previous == runtime.interface_paths {
            return false;
        }
        let changed = |name: &str| previous.get(name) != runtime.interface_paths.get(name);
        let active_changed = runtime
            .active_route
            .as_ref()
            .and_then(|route| route.bind_interface.as_deref())
            .is_some_and(changed);
        let saved_changed = self
            .route()
            .ok()
            .and_then(|route| route.bind_interface)
            .as_deref()
            .is_some_and(changed);
        if !active_changed && !saved_changed {
            return false;
        }
        self.invalidate_interface_path(runtime, active_changed);
        true
    }

    fn invalidate_interface_path(&self, runtime: &mut Runtime, clear_upstream: bool) {
        runtime.path_revision += 1;
        runtime.hard_block_revision += 1;
        if clear_upstream {
            runtime.installing = false;
            runtime.clear_upstream();
        }
        runtime.connectivity.clear();
        let reason = "The selected interface changed and needs a fresh network check";
        runtime.grant.invalidate(reason);
        runtime.apply_policy();
        self.journal(runtime, "interfaceChanged", reason, None);
    }

    /// Called by the macOS network-path watcher (`host/network_check.rs`).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn path_changed(&self) -> bool {
        self.path_changed_with(network_interfaces::paths().unwrap_or_default())
    }

    /// A default-path event says nothing about a bound adapter: tunnels such
    /// as a Clash TUN emit them constantly. A bound route is invalidated only
    /// when its own adapter's index or addresses changed in `paths`.
    fn path_changed_with(&self, paths: BTreeMap<String, InterfacePath>) -> bool {
        let mut runtime = self.runtime();
        if runtime
            .active_route
            .as_ref()
            .is_some_and(|route| route.bind_interface.is_some())
        {
            return runtime.gateway.is_some() && self.observe_interface_paths(&mut runtime, paths);
        }
        runtime.path_revision += 1;
        self.start_hold(
            &mut runtime,
            HoldCheck::Reachability,
            "The Mac network path changed".into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_network::test_support::*;
    use std::net::Ipv4Addr;

    use crate::network_gate::route_revision;
    use agent_core::model::{NetworkConnectivity, NetworkGateMode, NetworkRoute};
    use agent_core::util::now_millis;

    #[test]
    fn off_default_interface_changes_discard_the_stale_upstream_and_schedule_recovery() {
        let dir = temp("non-default-adapter");
        let network = GlobalNetwork::open(&dir).unwrap();
        let system = NetworkRoute::default();
        network.start(&dir.join("gateway.sock"), 0).unwrap();
        let ssh = network.ssh_port().unwrap();
        // Model a selected adapter without changing the host network configuration.
        let name = "en-test";
        let path = || InterfacePath {
            index: 123,
            ipv4: vec![(Ipv4Addr::new(192, 0, 2, 2), None)],
        };
        let bound = NetworkRoute {
            bind_interface: Some(name.into()),
            ..system
        };
        network.save(&bound).unwrap();
        network.runtime().active_route = Some(bound.clone());
        open_gate(&network, &bound);
        let rollback = network.rollback_point();
        {
            let mut runtime = network.runtime();
            runtime.interface_paths.insert(name.into(), path());
            assert!(network.observe_interface_paths(&mut runtime, BTreeMap::new()));
            assert!(runtime.active_route.is_none());
            assert!(runtime.gateway.as_ref().unwrap().proxy_url().is_none());
        }
        network.restore_gate(rollback);
        assert_eq!(network.ssh_port(), Some(ssh));
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.fail_route_preflight("The selected interface is unavailable");
        {
            let mut runtime = network.runtime();
            runtime.connectivity.mode = NetworkConnectivity::Blocked;
            assert!(
                network
                    .observe_interface_paths(&mut runtime, BTreeMap::from([(name.into(), path())]))
            );
            assert_eq!(runtime.connectivity.mode, NetworkConnectivity::Normal);
            assert!(runtime.grant.due(now_millis()));
            assert!(runtime.active_route.is_none());
        }
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn path_change_during_system_candidate_prevents_restoring_bound_grant() {
        let dir = temp("br");
        let network = GlobalNetwork::open(&dir).unwrap();
        network.start(&dir.join("gateway.sock"), 0).unwrap();
        let bound = NetworkRoute {
            bind_interface: Some("en-test".into()),
            ..NetworkRoute::default()
        };
        network.save(&bound).unwrap();
        network.runtime().active_route = Some(bound.clone());
        open_gate(&network, &bound);
        let rollback = network.rollback_point();
        network.invalidate_gate("Checking candidate");
        let system = NetworkRoute::default();
        network.switch(&system).unwrap();
        let revision = route_revision(&system);
        let attempt = network.begin_check(revision.clone(), false).unwrap();
        let hard_block_revision = network.runtime().hard_block_revision;
        // While the candidate is active, the system-path branch does not bump
        // hard_block_revision; path_revision must still invalidate the grant.
        network.path_changed();
        assert_eq!(network.runtime().hard_block_revision, hard_block_revision);
        assert!(
            !network
                .finish_check(&attempt, passing_report(&revision), None, &[], None, None)
                .unwrap()
        );
        // Model switching the upstream back without depending on a real en0.
        network.runtime().active_route = Some(bound);
        network.restore_gate(rollback);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn interface_path_change_invalidates_inflight_check_and_rollback() {
        let (dir, network, route) = bound_network("interface-path-change");
        let rollback = network.rollback_point();
        let revision = route_revision(&route);
        let attempt = network.begin_check(revision.clone(), true).unwrap();
        // The bound adapter appears with a new address.
        assert!(network.path_changed_with(test_path(7)));
        assert!(
            !network
                .finish_check(&attempt, passing_report(&revision), None, &[], None, None)
                .unwrap()
        );
        network.restore_gate(rollback);
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_default_path_event_keeps_an_unchanged_bound_adapter() {
        let (dir, network, _) = bound_network("interface-path-steady");
        network.runtime().interface_paths = test_path(142);
        let path_revision = network.runtime().path_revision;
        // Tunnels such as a Clash TUN emit these constantly.
        assert!(!network.path_changed_with(test_path(142)));
        assert_eq!(network.runtime().path_revision, path_revision);
        assert_eq!(network.runtime().grant.mode, NetworkGateMode::Open);
        network.stop();
        drop(network);
        let _ = std::fs::remove_dir_all(dir);
    }
}
