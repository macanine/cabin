use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

use super::AgentKind;

// ---------------------------------------------------------------- workspaces

/// A workspace's model for one agent, overriding the agent's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ModelOverride {
    pub agent: AgentKind,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub path: String,
    pub model_overrides: Vec<ModelOverride>,
    pub archived: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub created_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
}

impl Workspace {
    pub fn model_for(&self, agent: AgentKind) -> Option<&str> {
        self.model_overrides
            .iter()
            .find(|entry| entry.agent == agent)
            .map(|entry| entry.model.as_str())
    }
}
