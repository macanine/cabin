use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

/// A Mac folder kept in step with a folder in the machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncProject {
    pub id: String,
    /// The workspace whose folder is the machine side.
    pub workspace_id: Option<String>,
    pub name: String,
    pub host_path: String,
    pub guest_path: String,
    pub mode: crate::sync::SyncMode,
    pub rules: crate::sync::SyncRules,
    pub paused: bool,
    /// `.git` was copied into the machine (GitHandling::CopyOnce).
    #[serde(default)]
    pub git_copied: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub created_at: i64,
}

/// What the New project dialog asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncProjectInput {
    pub workspace_id: Option<String>,
    pub name: String,
    pub host_path: String,
    pub guest_path: String,
    pub mode: crate::sync::SyncMode,
    pub rules: crate::sync::SyncRules,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Waiting for the machine to connect.
    Offline,
    /// The first full sync, or a forced one.
    Scanning,
    Syncing,
    /// In step and watching for changes.
    Watching,
    Paused,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub id: String,
    pub state: SyncState,
    pub error: Option<String>,
    /// Unix milliseconds of the last completed cycle.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub last_sync_at: Option<i64>,
    /// Paths in step (files, folders and links).
    #[cfg_attr(test, ts(type = "number"))]
    pub files: u64,
    /// Totals since the app started.
    #[cfg_attr(test, ts(type = "number"))]
    pub pushed: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub pulled: u64,
    /// Paths changed on both sides, waiting for a choice.
    pub conflicts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncView {
    pub project: SyncProject,
    pub status: SyncStatus,
}

/// One top-level item of a folder, for choosing what syncs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPreviewItem {
    pub name: String,
    pub dir: bool,
    /// Files that would sync under it (1 for an included file).
    #[cfg_attr(test, ts(type = "number"))]
    pub files: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub bytes: u64,
    pub included: bool,
    /// Why it is left out: `unticked`, `ignored` (by .gitignore,
    /// .cabinignore or a pattern) or `git` (see GitHandling).
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncPreview {
    pub host_path: String,
    pub items: Vec<SyncPreviewItem>,
    #[cfg_attr(test, ts(type = "number"))]
    pub files: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub bytes: u64,
    /// The folder is a git repository (has `.git`).
    pub git: bool,
    /// The folder has a `.gitignore` at its top.
    pub gitignore: bool,
}
