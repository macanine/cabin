use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

use super::AgentKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum SessionStatus {
    Stopped,
    Starting,
    Running { pid: u32 },
    Exited { code: Option<i32> },
    Failed { message: String },
}

impl SessionStatus {
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Starting | Self::Running { .. })
    }
}

/// Terminal output and process state at one point in the PTY stream. Offsets
/// count UTF-8 bytes, and a generation identifies one launch attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub generation: u32,
    pub status: SessionStatus,
    #[cfg_attr(test, ts(type = "number"))]
    pub start_offset: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub end_offset: u64,
    pub data: String,
}

/// Existing sessions are terminals. Chat sessions use a separate structured
/// provider conversation; the two do not pretend to share a native thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum SessionMode {
    #[default]
    Terminal,
    Ui,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SessionWorktree {
    pub path: String,
    pub root: String,
    pub repository: String,
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub worktree: Option<SessionWorktree>,
    /// The agent CLI this session runs; `None` for a plain terminal (the
    /// user's login shell in the workspace folder).
    pub agent: Option<AgentKind>,
    #[serde(default)]
    pub mode: SessionMode,
    pub archived: bool,
    pub starred: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub created_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub last_started_at: Option<i64>,
    pub status: SessionStatus,
}
