use std::net::Ipv4Addr;

use agent_core::model::{ExitPolicy, GeoInfo, NetworkRoute};
use serde::{Deserialize, Serialize};

use super::{CheckAttempt, GlobalNetwork, Runtime, error, route_revision};
use crate::app_error::AppError;

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum ExitBaseline {
    Static(Ipv4Addr),
    Region { country: String, continent: String },
}

impl ExitBaseline {
    pub(super) fn violation(
        &self,
        ip: Option<Ipv4Addr>,
        country: Option<&str>,
        continent: Option<&str>,
    ) -> Option<String> {
        match self {
            Self::Static(approved) => ip.filter(|ip| ip != approved).map(|ip| {
                format!("Static exit changed from {approved} to {ip}. Internet access was blocked.")
            }),
            Self::Region {
                country: approved,
                continent: approved_continent,
            } if country.is_some_and(|code| !code.eq_ignore_ascii_case(approved))
                || continent.is_some_and(|code| code != approved_continent) =>
            {
                Some(format!(
                    "The exit moved outside the approved country or continent ({approved}, {approved_continent}). Internet access was blocked."
                ))
            }
            _ => None,
        }
    }
}

fn baseline_key(route: &NetworkRoute) -> String {
    let exit = route.enabled.then(|| route.chain.last()).flatten();
    let material =
        serde_json::to_string(&(exit, route.exit_policy)).expect("exit identity serializes");
    format!(
        "network_exit_baseline_{}",
        agent_core::util::sha256_text(&material)
    )
}

impl GlobalNetwork {
    pub(super) fn exit_baseline(
        &self,
        route: &NetworkRoute,
    ) -> Result<Option<ExitBaseline>, AppError> {
        self.with_store(|store| store.setting(&baseline_key(route)))
    }

    pub(super) fn save_exit_baseline(
        &self,
        route: &NetworkRoute,
        baseline: &ExitBaseline,
    ) -> Result<(), AppError> {
        self.with_store(|store| store.set_setting(&baseline_key(route), baseline))
    }

    pub(super) fn block_exit(&self, runtime: &mut Runtime, reason: String) {
        let Some(route) = runtime.active_route.as_ref() else {
            return;
        };
        runtime.exit_policy_error = Some((route_revision(route), reason.clone()));
        runtime.connectivity.clear();
        self.block_hold(runtime, &reason);
        runtime.grant.next_retry_at = None;
        runtime.apply_policy();
        self.journal(runtime, "exitPolicyBlocked", &reason, None);
    }

    /// Compare fresh observations with the persisted baseline before region-follow can change the guest.
    pub(crate) fn check_exit_policy(
        &self,
        attempt: &CheckAttempt,
        route: &NetworkRoute,
        ip: Option<Ipv4Addr>,
        trace_country: Option<&str>,
        samples: &[Result<GeoInfo, String>],
        guest_geo: Option<&GeoInfo>,
        rebind: bool,
        candidate_changed: bool,
    ) -> Result<ExitBaseline, AppError> {
        let mut runtime = self.runtime();
        if runtime.identity() != attempt.path || !runtime.grant.matches(&attempt.token) {
            return Err(error("The route changed while checking its exit policy"));
        }
        let reference = if rebind {
            None
        } else {
            self.exit_baseline(route)?
        };
        let ip = ip
            .filter(|ip| cabin_ip_detection::public_ip((*ip).into()))
            .ok_or_else(|| error("Could not confirm the public exit address"))?;
        let observed = samples
            .iter()
            .filter_map(|sample| sample.as_ref().ok())
            .chain(guest_geo);
        let reason = reference.as_ref().and_then(|reference| {
            reference
                .violation(Some(ip), trace_country, None)
                .or_else(|| {
                    observed.clone().find_map(|geo| {
                        reference.violation(
                            geo.ip.parse().ok(),
                            geo.country_code.as_deref(),
                            geo.continent_code.as_deref(),
                        )
                    })
                })
        });
        if let Some(reason) = reason {
            if !candidate_changed {
                self.block_exit(&mut runtime, reason.clone());
            }
            return Err(AppError {
                code: "exit_policy".into(),
                message: reason,
                details: None,
            });
        }
        if route.exit_policy == ExitPolicy::Static {
            return Ok(ExitBaseline::Static(ip));
        }
        let country = trace_country
            .filter(|code| code.len() == 2 && code.bytes().all(|byte| byte.is_ascii_alphabetic()))
            .ok_or_else(|| error("Could not confirm the public exit country"))?;
        if observed
            .clone()
            .filter_map(|geo| geo.country_code.as_deref())
            .any(|code| !code.eq_ignore_ascii_case(country))
        {
            return Err(error("Exit observers disagree on the country"));
        }
        let continent = observed
            .clone()
            .find_map(|geo| {
                (geo.ip.parse::<Ipv4Addr>() == Ok(ip)
                    && geo
                        .country_code
                        .as_deref()
                        .is_some_and(|code| code.eq_ignore_ascii_case(country)))
                .then_some(geo.continent_code.as_deref())
                .flatten()
            })
            .ok_or_else(|| error("Could not confirm the public exit continent"))?;
        if observed
            .filter_map(|geo| geo.continent_code.as_deref())
            .any(|code| code != continent)
        {
            return Err(error("Exit observers disagree on the continent"));
        }
        Ok(ExitBaseline::Region {
            country: country.to_ascii_uppercase(),
            continent: continent.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::global_network::test_support::{open_gate, passing_report};
    use crate::network_health::HoldCheck;
    use agent_core::model::NetworkGateMode;

    const OLD: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 8);
    const NEW: Ipv4Addr = Ipv4Addr::new(8, 8, 8, 9);

    fn network(route: &NetworkRoute) -> (tempfile::TempDir, GlobalNetwork) {
        let dir = tempfile::tempdir().unwrap();
        let network = GlobalNetwork::open(dir.path()).unwrap();
        network.save(route).unwrap();
        network.start(&dir.path().join("network.sock"), 0).unwrap();
        open_gate(&network, route);
        network.runtime().verified_exit = Some(OLD);
        (dir, network)
    }

    fn sample(ip: Ipv4Addr, country: &str, continent: Option<&str>) -> Result<GeoInfo, String> {
        Ok(GeoInfo {
            ip: ip.to_string(),
            country_code: Some(country.into()),
            continent_code: continent.map(str::to_owned),
            country: None,
            region: None,
            city: None,
            isp: None,
            ip_type: None,
            asn: None,
            latitude: None,
            longitude: None,
            timezone: None,
            source: "fixture".into(),
            checked_at: 0,
        })
    }

    #[test]
    fn static_drift_closes_immediately_and_persists_the_original_baseline() {
        let route = NetworkRoute::default();
        let (dir, network) = network(&route);
        network
            .save_exit_baseline(&route, &ExitBaseline::Static(OLD))
            .unwrap();
        let target = network.health_target().unwrap();
        assert!(
            network
                .observe_exit_health(&target, Ok(Some((NEW, Some("US".into())))))
                .hard_block
        );
        let blocked = network.status(true).unwrap();
        assert_eq!(blocked.gate, NetworkGateMode::Closed);
        assert!(blocked.exit_policy_error.unwrap().contains("8.8.8.9"));
        assert!(!network.gate_due().0);
        assert!(
            !network
                .observe_exit_health(&target, Ok(Some((OLD, None))))
                .changed
        );
        drop(network);
        let reopened = GlobalNetwork::open(dir.path()).unwrap();
        assert_eq!(
            reopened.exit_baseline(&route).unwrap(),
            Some(ExitBaseline::Static(OLD))
        );
    }

    #[test]
    fn dynamic_ip_changes_hold_traffic_until_the_exact_guest_exit_is_verified() {
        let route = NetworkRoute {
            exit_policy: ExitPolicy::DynamicResidential,
            ..Default::default()
        };
        let (_dir, network) = network(&route);
        let approved = ExitBaseline::Region {
            country: "US".into(),
            continent: "NA".into(),
        };
        network.save_exit_baseline(&route, &approved).unwrap();
        let target = network.health_target().unwrap();
        let change = network.observe_exit_health(&target, Ok(Some((NEW, Some("US".into())))));
        assert!(change.changed && !change.hard_block);
        assert_eq!(network.hold_check(), Some(HoldCheck::Verification));
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
        let revision = route_revision(&route);
        let check = network.begin_check(revision.clone(), true).unwrap();
        let wrong_ip = [sample(OLD, "US", Some("NA")), sample(NEW, "US", None)];
        assert!(
            network
                .check_exit_policy(
                    &check,
                    &route,
                    Some(NEW),
                    Some("US"),
                    &wrong_ip,
                    None,
                    false,
                    false
                )
                .is_err()
        );
        let fresh = [sample(NEW, "US", Some("NA"))];
        let baseline = network
            .check_exit_policy(
                &check,
                &route,
                Some(NEW),
                Some("US"),
                &fresh,
                None,
                false,
                false,
            )
            .unwrap();
        assert_eq!(baseline, approved);
        assert!(
            network
                .finish_check(
                    &check,
                    passing_report(&revision),
                    Some(NEW),
                    &fresh,
                    None,
                    Some(&baseline)
                )
                .unwrap()
        );
        assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Open);
    }

    #[test]
    fn either_country_or_continent_drift_blocks_and_requires_explicit_reapproval() {
        for (country, continent) in [("CA", "NA"), ("US", "EU")] {
            let route = NetworkRoute {
                exit_policy: ExitPolicy::DynamicResidential,
                ..Default::default()
            };
            let (_dir, network) = network(&route);
            network
                .save_exit_baseline(
                    &route,
                    &ExitBaseline::Region {
                        country: "US".into(),
                        continent: "NA".into(),
                    },
                )
                .unwrap();
            let revision = route_revision(&route);
            let check = network.begin_check(revision.clone(), true).unwrap();
            let samples = [sample(NEW, country, Some(continent))];
            assert_eq!(
                network
                    .check_exit_policy(
                        &check,
                        &route,
                        Some(NEW),
                        Some(country),
                        &samples,
                        None,
                        false,
                        false
                    )
                    .unwrap_err()
                    .code,
                "exit_policy"
            );
            assert_eq!(network.status(true).unwrap().gate, NetworkGateMode::Closed);
            let check = network.begin_check(revision.clone(), false).unwrap();
            let baseline = network
                .check_exit_policy(
                    &check,
                    &route,
                    Some(NEW),
                    Some(country),
                    &samples,
                    None,
                    true,
                    false,
                )
                .unwrap();
            assert!(
                network
                    .finish_check(
                        &check,
                        passing_report(&revision),
                        Some(NEW),
                        &samples,
                        None,
                        Some(&baseline)
                    )
                    .unwrap()
            );
            let reopened = network.status(true).unwrap();
            assert_eq!(reopened.gate, NetworkGateMode::Open);
            assert!(reopened.exit_policy_error.is_none());
        }
    }
}
