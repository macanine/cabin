use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

use super::AgentKind;

// --------------------------------------------------------------------- setup

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum CheckStatus {
    Ok,
    Warning,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SetupAction {
    InstallBasePackages,
    /// Install the latest Node.js LTS from the official builds and make it
    /// the default.
    InstallNode,
    /// Python's venv/pip/dev packages plus uv and uvx (see `python`).
    InstallPython,
    InstallAgent {
        agent: AgentKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SetupCheck {
    pub id: String,
    pub label: String,
    pub status: CheckStatus,
    pub detail: String,
    pub action: Option<SetupAction>,
}

// ---------------------------------------------------------------------- node

/// Where the `node` that sessions use comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NodeSource {
    /// An official build installed and switched by cabin.
    Managed,
    /// Ubuntu's apt package (`/usr/bin/node`).
    System,
    /// Anything else on PATH (nvm, a manual install, …).
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NodeRuntime {
    pub version: String,
    pub npm: Option<String>,
    pub path: String,
    pub source: NodeSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NodeInstalled {
    pub version: String,
    /// True when this is the version Cabin links as `node`.
    pub current: bool,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NodeState {
    /// The `node` sessions and installs actually run.
    pub active: Option<NodeRuntime>,
    /// Versions Cabin installed, newest first.
    pub installed: Vec<NodeInstalled>,
    /// Ubuntu's apt Node.js, if present.
    pub system: Option<NodeRuntime>,
    pub minimum_major: u32,
    pub recommended_major: u32,
    /// Download mirror used last (or the default).
    pub mirror: String,
    pub install_dir: String,
    /// Architecture tag of the builds this machine needs (`linux-arm64`).
    pub platform: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NodeRelease {
    pub version: String,
    pub major: u32,
    /// LTS codename, or null for a Current release.
    pub lts: Option<String>,
    pub date: String,
    pub npm: Option<String>,
    pub security: bool,
    pub installed: bool,
}
