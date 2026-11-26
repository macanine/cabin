use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

// ---------------------------------------------------------------------------
// Workbench panel (files, git changes, browser preview). Paths are relative to
// the session's root (its worktree, else the workspace folder), "/"-separated.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum FsEntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub kind: FsEntryKind,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
    /// Matched by `.gitignore` (shown dimmed, like an editor's explorer).
    pub ignored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FsListing {
    pub path: String,
    pub entries: Vec<FsEntry>,
    /// More entries existed than the listing returns.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum FsContentKind {
    Text,
    Image,
    Binary,
    TooLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FsFile {
    pub path: String,
    pub kind: FsContentKind,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
    /// Unix milliseconds on the guest's clock.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub modified_at: Option<i64>,
    pub text: Option<String>,
    pub base64: Option<String>,
    pub mime_type: Option<String>,
}

/// One path in `git status`, in one group. `status` is git's letter for that
/// side: M A D R C T U, or `?` for untracked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GitFileStatus {
    pub path: String,
    pub orig_path: Option<String>,
    pub status: String,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub additions: Option<u32>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub deletions: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    /// False when the root is not inside a git work tree.
    pub repository: bool,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub ahead: u32,
    #[cfg_attr(test, ts(type = "number"))]
    pub behind: u32,
    /// Short id of HEAD; none on an unborn branch.
    pub head: Option<String>,
    pub conflicted: Vec<GitFileStatus>,
    pub staged: Vec<GitFileStatus>,
    pub unstaged: Vec<GitFileStatus>,
    pub untracked: Vec<GitFileStatus>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GitFileDiff {
    pub path: String,
    pub staged: bool,
    /// Unified diff; empty when the change has no text (e.g. mode only).
    pub patch: String,
    pub binary: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GitCommitResult {
    pub head: String,
    pub summary: String,
}

/// A TCP port a user process listens on in the VM (a dev server to preview).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ListeningPort {
    pub port: u16,
    /// "127.0.0.1", "::1", "0.0.0.0" or "::".
    pub address: String,
    pub process: Option<String>,
    pub command: Option<String>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub pid: Option<u32>,
}
