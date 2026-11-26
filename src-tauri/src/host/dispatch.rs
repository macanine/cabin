//! `Host::call`: host-owned `network_*` and `proxy_*` methods are answered
//! on the Mac; everything else is forwarded to the guest.

use super::network_check::VerificationKind;
use agent_core::util::LockExt;

use agent_core::model::NetworkRoute;
use serde_json::Value;

use super::{Host, unavailable};
use crate::app_error::AppError;

impl Host {
    /// Forward one UI command to cabind and wait for its result. Network
    /// and catalog methods are answered here: they belong to the Mac.
    pub fn call(&self, method: &str, params: Value) -> Result<Value, AppError> {
        let text = |key: &str| params.get(key).and_then(Value::as_str);
        let required = |key: &str| text(key).ok_or_else(|| unavailable(format!("Missing {key}")));
        let ids = |key: &str| -> Result<Vec<String>, AppError> {
            serde_json::from_value(params.get(key).cloned().unwrap_or(Value::Null))
                .map_err(|_| unavailable(format!("Missing {key}")))
        };
        let to_value =
            |value: Result<Value, serde_json::Error>| value.map_err(|e| unavailable(e.to_string()));
        // Changes are serialized; checks and lookups run alongside them.
        let _guard = matches!(
            method,
            "proxy_import"
                | "proxy_add_url"
                | "proxy_refresh_from_host"
                | "proxy_remove"
                | "proxy_group_save"
                | "proxy_group_remove"
                | "network_apply"
                | "network_verify"
                | "network_pause"
                | "network_auto_recover"
        )
        .then(|| self.network.transaction.locked());
        let changed = match method {
            "network_state" => {
                self.sync_environment_revision();
                return to_value(serde_json::to_value(
                    self.network.status(self.uses_gateway())?,
                ));
            }
            "network_interfaces" => {
                let interfaces =
                    vm_core::network_interfaces::list().map_err(|e| unavailable(e.to_string()))?;
                return to_value(serde_json::to_value(interfaces));
            }
            "network_traffic" => return to_value(serde_json::to_value(self.network.traffic())),
            "network_history" => {
                let before = params.get("beforeId").and_then(Value::as_i64);
                let max = u64::from(agent_core::store::NETWORK_HISTORY_LIMIT);
                let limit = params
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(max)
                    .clamp(1, max) as u32;
                return to_value(serde_json::to_value(self.network.history(before, limit)?));
            }
            "network_apply" => {
                let mut route: NetworkRoute =
                    serde_json::from_value(params.get("route").cloned().unwrap_or(Value::Null))
                        .map_err(|e| unavailable(format!("Invalid route: {e}")))?;
                route.bind_interface = None;
                return to_value(serde_json::to_value(self.apply_network_route(&route)?));
            }
            "network_verify" => {
                let rebind = params.get("acceptExit").and_then(Value::as_bool) == Some(true);
                if rebind && self.network.status(true)?.exit_policy_error.is_none() {
                    return Err(unavailable("There is no blocked exit to replace"));
                }
                let kind = if rebind {
                    VerificationKind::ReplaceExit
                } else {
                    VerificationKind::Manual
                };
                let status = self.verify_route(&self.network.route()?, kind)?;
                return to_value(serde_json::to_value(status));
            }
            "network_pause" => {
                let paused = params
                    .get("paused")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| unavailable("Missing paused"))?;
                self.require_gateway()?;
                self.network.set_paused(paused)?;
                if paused {
                    self.fail_guest_chats(
                        "Internet access was paused. Active agent requests were stopped; resume the network and try again.",
                    );
                }
                self.emit_network()?;
                return to_value(serde_json::to_value(self.network.status(true)?));
            }
            "network_region_follow" => {
                let enabled = params
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| unavailable("Missing enabled"))?;
                self.network.set_region_follows_exit(enabled)?;
                self.emit_network()?;
                return to_value(serde_json::to_value(
                    self.network.status(self.uses_gateway())?,
                ));
            }
            "network_system_route" => {
                return to_value(serde_json::to_value(crate::system_route::detect()));
            }
            "network_auto_recover" => {
                let enabled = params
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| unavailable("Missing enabled"))?;
                self.require_gateway()?;
                self.network.set_auto_recover(enabled)?;
                self.emit_network()?;
                return to_value(serde_json::to_value(self.network.status(true)?));
            }
            "network_check" => {
                let mut route: NetworkRoute = if let Some(value) = params.get("route") {
                    serde_json::from_value(value.clone())
                        .map_err(|e| unavailable(format!("Invalid route: {e}")))?
                } else {
                    // Older clients sent only the candidate chain.
                    NetworkRoute {
                        enabled: true,
                        chain: ids("chain")?,
                        ..self.network.route()?
                    }
                };
                route.bind_interface = None;
                return to_value(serde_json::to_value(self.network.check(&route)?));
            }
            "network_exit" => {
                self.require_gateway()?;
                return to_value(serde_json::to_value(self.network.exit()?));
            }
            "network_exit_type" => {
                self.require_gateway()?;
                let refresh = params
                    .get("refresh")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                return to_value(serde_json::to_value(
                    self.network.exit_type(required("ip")?, refresh)?,
                ));
            }
            "network_origin" => return to_value(serde_json::to_value(self.network.origin()?)),
            "network_locate" => {
                return to_value(serde_json::to_value(self.network.locate(&ids("nodeIds")?)?));
            }
            "proxy_probe" => {
                return to_value(serde_json::to_value(
                    self.network.probe(required("nodeId")?)?,
                ));
            }
            "proxy_catalog" => return to_value(serde_json::to_value(self.network.catalog()?)),
            "proxy_node_test" => {
                let kind =
                    serde_json::from_value(params.get("kind").cloned().unwrap_or(Value::Null))
                        .map_err(|_| unavailable("Choose a latency or download test"))?;
                return to_value(serde_json::to_value(self.network.test_node(
                    required("nodeId")?,
                    required("configRevision")?,
                    kind,
                    None,
                )?));
            }
            "proxy_group_save" => serde_json::to_value(self.network.save_node_group(
                text("id"),
                required("name")?,
                &ids("nodeIds")?,
            )?),
            "proxy_group_remove" => {
                serde_json::to_value(self.network.remove_node_group(required("id")?)?)
            }
            "proxy_import" => serde_json::to_value(self.network.import(
                required("name")?,
                text("url"),
                text("contents"),
            )?),
            "proxy_add_url" => serde_json::to_value(self.network.add_manual(required("url")?)?),
            "proxy_refresh" => serde_json::to_value(
                self.network.refresh(
                    required("id")?,
                    params
                        .get("automatic")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                )?,
            ),
            "proxy_refresh_from_host" => {
                serde_json::to_value(self.network.refresh_from_host(required("id")?)?)
            }
            "proxy_remove" => serde_json::to_value(self.network.remove(required("id")?)?),
            _ => {
                if matches!(method, "chat_send" | "chat_steer" | "chat_retry")
                    && self.uses_gateway()
                    && let Some(reason) = self.network.blocked_reason()
                {
                    return Err(AppError {
                        code: "network_check".into(),
                        message: reason,
                        details: None,
                    });
                }
                return self.call_guest(method, params);
            }
        };
        let message = match method {
            "proxy_import" => "Imported a subscription",
            "proxy_add_url" => "Added a proxy node",
            "proxy_refresh" | "proxy_refresh_from_host" => "Refreshed a subscription",
            "proxy_remove" => "Removed a subscription",
            "proxy_group_save" => "Saved a node group",
            "proxy_group_remove" => "Removed a node group",
            _ => "Updated the network catalog",
        };
        self.network.record_check("catalogChanged", message, None);
        self.emit_network()?;
        to_value(changed)
    }
}
