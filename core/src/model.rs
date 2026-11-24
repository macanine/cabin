//! Wire types shared with the TypeScript UI (camelCase on the boundary).
//!
//! Every timestamp on the wire is Unix milliseconds. Guest timestamps carry the
//! guest's clock; server epochs (for example Codex quota) are normalized to
//! milliseconds at the agent boundary. TypeScript declarations are generated
//! from these types by the `ipc_bindings_are_current` test in `lib.rs`.

use serde::{Deserialize, Deserializer, Serialize};

#[cfg(test)]
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Opencode,
    Pi,
    Antigravity,
}

impl AgentKind {
    pub const ALL: [AgentKind; 5] = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Opencode,
        AgentKind::Pi,
        AgentKind::Antigravity,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Pi => "pi",
            Self::Antigravity => "antigravity",
        }
    }

    pub fn parse(value: &str) -> crate::Result<Self> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::Opencode),
            "pi" => Ok(Self::Pi),
            "antigravity" => Ok(Self::Antigravity),
            // Stored by earlier versions.
            "primary" => Ok(Self::Claude),
            other => Err(crate::Error::invalid(format!("unknown agent {other}"))),
        }
    }
}

impl<'de> Deserialize<'de> for AgentKind {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

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
pub struct ChatQuestion {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
    pub multiple: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachment {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachmentInput {
    pub name: String,
    pub mime_type: String,
    pub base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachmentData {
    pub mime_type: String,
    pub base64: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum ChatItemKind {
    UserMessage,
    AssistantMessage,
    Reasoning,
    ProposedPlan,
    CommandExecution,
    FileChange,
    Tool,
    ApprovalRequest,
    UserInputRequest,
    SystemNotice,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ChatItemStatus {
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ChatRunState {
    Starting,
    Ready,
    Running,
    Waiting,
    Switching,
    Stopped,
    Error,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "kebab-case")]
pub enum ChatPermissionMode {
    #[default]
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    Plan,
    FullAccess,
}

impl ChatPermissionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval-required",
            Self::AutoAcceptEdits => "auto-accept-edits",
            Self::Auto => "auto",
            Self::Plan => "plan",
            Self::FullAccess => "full-access",
        }
    }
}

/// One entry in an agent's native model catalog, normalized for the picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentModel {
    pub id: String,
    pub name: String,
    pub provider: Option<String>,
    pub description: Option<String>,
    pub is_default: bool,
}

// ---------------------------------------------------------- Claude account

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ClaudeAuthMethod {
    Subscription,
    ApiKey,
    OauthToken,
    External,
    Unknown,
}

/// The guest's current Claude login, safe to show in the account view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccount {
    /// Scope for subscription limits, when Claude identified the account.
    pub id: Option<String>,
    pub method: ClaudeAuthMethod,
    pub verified: bool,
    pub email: Option<String>,
    pub organization: Option<String>,
    pub plan: Option<String>,
    pub provider: Option<String>,
    pub usage_based: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ClaudeLogin {
    Active { account: ClaudeAccount },
    SignedOut,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsageWindow {
    pub label: String,
    pub used_percent: f64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub resets_at: Option<i64>,
    /// Claude's own text, when it did not provide an absolute timestamp.
    pub reset_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ClaudeUsage {
    pub windows: Vec<ClaudeUsageWindow>,
    pub no_limits: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub refreshed_at: i64,
}

// ---------------------------------------------------------- Codex accounts

/// A saved Codex login, without its credentials. Credentials are kept in a
/// separate 0600 file and cross IPC only for an explicit import or export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CodexAccount {
    pub id: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub auth_mode: String,
    pub subscription_until: Option<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub imported_at: i64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub last_used_at: Option<i64>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CodexQuotaWindow {
    /// `fiveHour`, `sevenDay`, or `other`.
    pub kind: String,
    pub label: String,
    pub used_percent: u8,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub window_minutes: Option<u64>,
    /// Unix milliseconds (normalized at the Codex boundary).
    #[cfg_attr(test, ts(type = "number | null"))]
    pub resets_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CodexCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CodexResetCard {
    pub id: String,
    pub status: String,
    pub reset_type: String,
    /// Unix milliseconds (normalized at the Codex boundary).
    #[cfg_attr(test, ts(type = "number | null"))]
    pub granted_at: Option<i64>,
    /// Unix milliseconds (normalized at the Codex boundary).
    #[cfg_attr(test, ts(type = "number | null"))]
    pub expires_at: Option<i64>,
    pub title: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CodexQuota {
    pub plan_type: Option<String>,
    pub ordinary_usage_allowed: Option<bool>,
    pub spend_control_reached: Option<bool>,
    pub rate_limit_reached_type: Option<String>,
    pub windows: Vec<CodexQuotaWindow>,
    pub credits: Option<CodexCredits>,
    pub reset_credit_count: u32,
    pub reset_credits: Vec<CodexResetCard>,
    #[cfg_attr(test, ts(type = "number"))]
    pub refreshed_at: i64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CodexAccounts {
    pub accounts: Vec<CodexAccount>,
    pub active_account_id: Option<String>,
    pub quota: Option<CodexQuota>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct CodexResetResult {
    pub outcome: String,
    pub message: String,
    pub quota: Option<CodexQuota>,
}

/// Provider-independent execution item projected from a provider's native stream.
/// An update replaces the previous revision of the same item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatItem {
    pub id: String,
    pub ordinal: u32,
    pub revision: u32,
    /// The user message that opened this turn. The UI groups a conversation by
    /// this stable id instead of re-splitting a flat item stream.
    #[serde(default)]
    pub turn_id: String,
    pub kind: ChatItemKind,
    pub status: ChatItemStatus,
    pub title: String,
    pub text: String,
    pub detail: String,
    pub request_id: Option<String>,
    pub questions: Vec<ChatQuestion>,
    #[serde(default)]
    pub attachments: Vec<ChatAttachment>,
    /// First and latest write, in Unix milliseconds. The store stamps these
    /// and `chat_view::present` fills `action`/`subject`/`files`; the bridge
    /// leaves all five out of the items it emits.
    #[serde(default)]
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at: i64,
    #[serde(default)]
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
    /// What a tool-like item did, for grouping ("Read 3 files"). Set by
    /// `chat_view::present`; `None` for messages and cards.
    #[serde(default)]
    pub action: Option<ToolAction>,
    /// One-line description: the command, the file, the query.
    #[serde(default)]
    pub subject: String,
    /// Files a change touched, workspace-relative when possible.
    #[serde(default)]
    pub files: Vec<String>,
    /// For approval items: the decisions the guest runtime allows (already
    /// filtered by the agent's capability descriptor). The UI renders exactly these.
    #[serde(default)]
    pub approval: Option<ApprovalView>,
    /// Structured file changes with their origin (provider-reported or workspace diff).
    #[serde(default)]
    pub changes: Vec<FileChangeView>,
    /// A subagent task this item launches or reports. Set by the ACP view when
    /// the provider's tool call names its child session.
    #[serde(default)]
    pub task_id: Option<String>,
}

/// The native lifecycle a plan preview follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewKind {
    /// The provider is stopped at a native request and waits for its reply.
    NativeRequest,
    /// The planning turn finished; approval starts a new execution turn.
    NextTurn,
}

impl PlanReviewKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeRequest => "native_request",
            Self::NextTurn => "next_turn",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "native_request" => Some(Self::NativeRequest),
            "next_turn" => Some(Self::NextTurn),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewStatus {
    Draft,
    AwaitingReview,
    Submitting,
    Approved,
    ChangesRequested,
    Deferred,
    Cancelled,
    Expired,
    Superseded,
}

impl PlanReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::AwaitingReview => "awaiting_review",
            Self::Submitting => "submitting",
            Self::Approved => "approved",
            Self::ChangesRequested => "changes_requested",
            Self::Deferred => "deferred",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "draft" => Self::Draft,
            "awaiting_review" => Self::AwaitingReview,
            "submitting" => Self::Submitting,
            "approved" => Self::Approved,
            "changes_requested" => Self::ChangesRequested,
            "deferred" => Self::Deferred,
            "cancelled" => Self::Cancelled,
            "expired" => Self::Expired,
            "superseded" => Self::Superseded,
            _ => return None,
        })
    }

    pub fn is_open(self) -> bool {
        matches!(
            self,
            Self::Draft | Self::AwaitingReview | Self::Submitting | Self::Deferred
        )
    }

    pub fn accepts_decision(self) -> bool {
        matches!(self, Self::AwaitingReview | Self::Deferred)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutionStatus {
    NotStarted,
    Starting,
    Running,
    Completed,
    Failed,
    Unknown,
}

impl PlanExecutionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotStarted => "not_started",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "not_started" => Self::NotStarted,
            "starting" => Self::Starting,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanDecision {
    Approve,
    RequestChanges,
    Defer,
}

impl PlanDecision {
    pub fn settled_status(self) -> PlanReviewStatus {
        match self {
            Self::Approve => PlanReviewStatus::Approved,
            Self::RequestChanges => PlanReviewStatus::ChangesRequested,
            Self::Defer => PlanReviewStatus::Deferred,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
            Self::Defer => "defer",
        }
    }
}

/// A persisted plan revision. `plan_id + revision` is the CAS identity; raw
/// provider reply contexts stay in the live provider session and never cross
/// IPC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct PlanReview {
    pub plan_id: String,
    pub agent: AgentKind,
    pub session_id: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub revision: i64,
    pub title: String,
    pub content: String,
    pub content_hash: String,
    pub complete: bool,
    pub review_kind: PlanReviewKind,
    pub native_plan_id: Option<String>,
    pub exec_permission: ChatPermissionMode,
    pub status: PlanReviewStatus,
    pub execution: PlanExecutionStatus,
    pub decision: Option<PlanDecision>,
    pub feedback: Option<String>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub decision_intent_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub applied_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number"))]
    pub created_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub superseded_by: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum PlanRespondOutcome {
    Applied,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct PlanRespondResult {
    pub outcome: PlanRespondOutcome,
    pub review: PlanReview,
}

/// One provider-dispatched subagent. Its transcript is stored beside the root
/// timeline so a Task row can no longer hide an entire delegated run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "snake_case")]
pub enum SubagentState {
    Active,
    Completed,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SubagentTask {
    pub id: String,
    pub label: String,
    pub subagent_type: Option<String>,
    pub detail: String,
    pub state: SubagentState,
    pub origin: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
    pub items: Vec<ChatItem>,
}

/// What a chat with this agent can do, so the UI hides actions before they are tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentChatCapabilities {
    pub agent: AgentKind,
    /// Empty: the permission picker is hidden for this agent.
    pub permission_modes: Vec<ChatPermissionMode>,
}

/// Prompt content the agent accepted in this live session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatPromptCapabilities {
    pub images: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

/// Session operations the live ACP adapter negotiated, plus the ACP surfaces
/// this runtime actually consumes. The UI uses this descriptor to show or hide
/// controls; it never derives behavior from the agent's name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatRuntimeCapabilities {
    pub prompt: ChatPromptCapabilities,
    pub steer: bool,
    pub resume: bool,
    pub load: bool,
    pub fork: bool,
    pub list: bool,
    pub plans: bool,
    pub subagents: bool,
    pub async_tasks: bool,
    pub notices: bool,
    pub compaction: bool,
    pub file_read: bool,
    pub file_write: bool,
    pub terminals: bool,
    /// The agent advertised the provider-neutral goal extension.
    pub goal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ApprovalView {
    /// `command`, `file-read`, `file-change` or another normalized request kind.
    pub kind: String,
    pub decisions: Vec<ApprovalDecisionView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecisionView {
    /// `accept`, `decline`, `cancel`, `acceptForSession` or `acceptAlways`.
    pub decision: String,
    pub label: String,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "kebab-case")]
pub enum ChangeOrigin {
    /// The agent reported this change itself.
    Provider,
    /// Found by diffing the workspace (shell commands, formatters, subagents).
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FileChangeView {
    /// Workspace-relative, validated to stay inside the workspace.
    pub path: String,
    pub origin: ChangeOrigin,
    pub kind: ChangeKind,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub additions: Option<u32>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub deletions: Option<u32>,
    /// Unified diff text, when known.
    pub patch: Option<String>,
}

/// The kind of work a tool call did, used to group it in the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "kebab-case")]
pub enum ToolAction {
    Read,
    Edit,
    Command,
    CodeSearch,
    WebSearch,
    Plan,
    Agent,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatState {
    pub state: ChatRunState,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatSnapshot {
    pub items: Vec<ChatItem>,
    pub plans: Vec<PlanReview>,
    pub subagents: Vec<SubagentTask>,
    /// In-flight background work reported by the agent's ACP extensions.
    /// Unlike transcript blocks this is live state; it is not persisted.
    #[serde(default)]
    pub tasks: Vec<ChatAsyncTask>,
    pub state: ChatState,
    pub model: Option<String>,
    pub permission: ChatPermissionMode,
    /// Token usage per turn, in turn order.
    pub usage: Vec<ChatTurnUsage>,
    /// What the live agent offers this session; empty when it is not running.
    #[serde(default)]
    pub controls: ChatControls,
}

/// The ACP runtime a chat with one agent runs on: an adapter package that
/// speaks ACP for the agent's own CLI (or, for OpenCode, the CLI itself),
/// installed per version under `<data>/acp/<agent>/<version>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AcpRuntime {
    pub agent: AgentKind,
    /// npm package, e.g. `@agentclientprotocol/claude-agent-acp`.
    pub package: String,
    pub license: String,
    /// The version this app was tested with; used until the user picks one.
    pub default_version: String,
    /// The version new chats run on.
    pub selected: String,
    /// Versions present on disk, newest first.
    pub installed: Vec<String>,
    /// Set when an environment variable overrides the managed install.
    pub override_path: Option<String>,
    /// The adapter drives the agent CLI from Setup (`false`: the package is the agent).
    pub uses_agent_cli: bool,
}

/// The live session controls an ACP agent reports (config options, slash
/// commands, its plan, context use, title). Held in memory for the running
/// session only; the UI renders exactly these and never infers them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatControls {
    pub agent_name: Option<String>,
    pub agent_version: Option<String>,
    /// Selectors the agent exposes (model, mode, thought level, …), in its order.
    pub options: Vec<ChatConfigOption>,
    /// Slash commands the agent accepts at the start of a message.
    pub commands: Vec<ChatCommand>,
    /// The agent's current execution plan (ACP `plan`), replaced whole on each update.
    pub plan: Vec<ChatPlanEntry>,
    pub context: Option<ChatContextUse>,
    pub title: Option<String>,
    /// Follow-up prompts the agent suggests after a turn.
    pub suggestions: Vec<String>,
    /// The agent accepts a message while a turn runs (`chat_steer`).
    pub can_steer: bool,
    /// The agent accepts image attachments.
    pub images: bool,
    /// Negotiated protocol capabilities for this live session.
    #[serde(default)]
    pub capabilities: ChatRuntimeCapabilities,
    /// The provider-neutral goal snapshot, when the live adapter advertises it.
    #[serde(default)]
    pub goal: Option<ChatGoal>,
}

/// One live goal owned by the agent. Goal state is reported by the adapter as a
/// complete snapshot; the UI neither derives nor mutates it locally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatGoal {
    pub objective: String,
    /// `active`, `paused`, `blocked`, `limited`, `complete`, …
    pub status: String,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub token_budget: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub tokens_used: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub time_used_seconds: Option<u64>,
    /// Goal-control actions this adapter actually accepts.
    pub actions: Vec<String>,
}

/// One provider-reported background task (ACP/JetBrains AIR async tasks).
/// Subagents live in `SubagentTask`; this is the agent's non-agent work such
/// as background shells, monitors and autonomous turns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatAsyncTask {
    pub id: String,
    pub name: String,
    pub task_type: String,
    pub description: String,
    /// `running`, `paused`, `completed`, `failed` or `stopped`.
    pub state: String,
    pub can_stop: bool,
    pub summary: Option<String>,
    pub last_tool_name: Option<String>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub total_tokens: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub tool_uses: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub duration_ms: Option<u64>,
    pub output_file_path: Option<String>,
    pub tool_call_id: Option<String>,
    /// The transcript item (tool card) that started this task, so the UI can
    /// mark that card and jump to it without knowing how ids are built.
    #[serde(default)]
    pub item_id: Option<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatConfigOption {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// `model`, `mode`, `thought_level` or another agent-defined category.
    pub category: Option<String>,
    /// `select` or `boolean`.
    pub kind: String,
    /// The current value id; `"true"`/`"false"` for a boolean.
    pub current: String,
    pub choices: Vec<ChatConfigChoice>,
    /// Owned by the permission picker (it sets this option), so not shown on its own.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatConfigChoice {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
    pub group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatCommand {
    pub name: String,
    pub description: String,
    /// Placeholder for the command's argument, when it takes one.
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatPlanEntry {
    pub content: String,
    /// `high`, `medium` or `low`.
    pub priority: String,
    /// `pending`, `in_progress` or `completed`.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatContextUse {
    #[cfg_attr(test, ts(type = "number"))]
    pub used: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
    /// Session cost so far in millionths of `currency`.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub cost_micros: Option<u64>,
    pub currency: Option<String>,
}

/// Token usage of one turn: `input` counts every
/// prompt token including cache reads and writes; `cached` and `cache_write`
/// are parts of it; `reasoning` is part of `output`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatUsage {
    #[cfg_attr(test, ts(type = "number"))]
    pub input: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub cached: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub cache_write: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub output: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub reasoning: u64,
    /// Time the model spent streaming text or reasoning (for tokens/s).
    #[cfg_attr(test, ts(type = "number"))]
    pub generation_ms: u64,
    /// Cost in millionths of a US dollar, when the agent reports it.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub cost_micros: Option<u64>,
    /// Prompt size of the latest request, and the model's window.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub context_tokens: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub context_window: Option<u64>,
}

/// Usage of the turn started by user message `turn_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ChatTurnUsage {
    pub turn_id: String,
    pub usage: ChatUsage,
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

// ---------------------------------------------------------------- operations

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum OperationState {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    RolledBack,
}

impl OperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::RolledBack => "rolledBack",
        }
    }

    pub fn is_finished(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    Pending,
    Running,
    Succeeded,
    Failed,
    RolledBack,
    RollbackFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StepRecord {
    pub name: String,
    pub state: StepState,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct OperationRecord {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub state: OperationState,
    pub steps: Vec<StepRecord>,
    pub error: Option<String>,
    pub retryable: bool,
    #[cfg_attr(test, ts(type = "number"))]
    pub created_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
}

// --------------------------------------------------------------- environment

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSpec {
    pub timezone: String,
    pub language: String,
}

impl Default for EnvironmentSpec {
    fn default() -> Self {
        Self {
            timezone: "UTC".into(),
            language: "en_US.UTF-8".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentState {
    /// The last successfully applied environment.
    pub committed: EnvironmentSpec,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub applied_at: Option<i64>,
    /// What the operating system reports right now.
    pub system_timezone: Option<String>,
    pub system_language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct FieldState {
    pub field: String,
    pub enabled: bool,
    pub reason: Option<String>,
    pub suggestion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentReview {
    pub fields: Vec<FieldState>,
    pub changes: Vec<String>,
    pub blocked: Option<String>,
    pub timezones: Vec<String>,
    pub languages: Vec<String>,
}

// --------------------------------------------------------------------- proxy

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxySource {
    pub id: String,
    pub name: String,
    pub has_url: bool,
    pub node_count: u32,
    #[cfg_attr(test, ts(type = "number"))]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyNode {
    pub id: String,
    pub name: String,
    pub source_id: String,
    pub source_name: String,
    pub protocol: String,
    /// Opaque identity of this node's current configuration. The route id can
    /// be reused when a subscription changes.
    pub config_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyGroup {
    pub id: String,
    pub name: String,
    pub node_ids: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyCatalog {
    pub sources: Vec<ProxySource>,
    pub nodes: Vec<ProxyNode>,
    #[serde(default)]
    pub groups: Vec<ProxyGroup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ProxyTestKind {
    Latency,
    Download,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProxyTestResult {
    pub node_id: String,
    pub config_revision: String,
    pub kind: ProxyTestKind,
    #[cfg_attr(test, ts(type = "number"))]
    pub tested_at: i64,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub bytes: Option<u32>,
    pub elapsed_ms: Option<u32>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub detail: String,
}

// ------------------------------------------------------------------ network

/// The VM's route. The VM's only NIC goes to shoes on the Mac; with
/// `enabled` off shoes sends it straight out (still cut off from the LAN and
/// the Mac), otherwise through `chain`: catalog node ids, entry first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkRoute {
    pub enabled: bool,
    pub chain: Vec<String>,
    /// Host interface for direct traffic or the first proxy hop; None uses the system route.
    #[serde(default)]
    pub bind_interface: Option<String>,
    #[serde(default)]
    pub exit_policy: ExitPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum ExitPolicy {
    #[default]
    Static,
    DynamicResidential,
}

/// How traffic from the machine is handled right now. `checking` is a short,
/// restricted window that only admits the fixed verification targets.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkGateMode {
    #[default]
    Closed,
    Checking,
    Open,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkCheckStatus {
    Pass,
    Fail,
    Unknown,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkCheckCategory {
    Route,
    Gateway,
    Dns,
    Transport,
    Boundary,
    Exit,
    Environment,
}

/// One observed fact behind a check result. Evidence is always shown with its
/// source and sampling time so missing or stale data is not mistaken for a
/// successful result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkEvidence {
    pub label: String,
    pub value: String,
    pub source: String,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub observed_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkCheckItem {
    pub id: String,
    pub title: String,
    pub category: NetworkCheckCategory,
    pub status: NetworkCheckStatus,
    pub required: bool,
    pub summary: String,
    pub evidence: Vec<NetworkEvidence>,
}

/// A complete evaluation for one immutable route revision. A report with any
/// required `fail` or `unknown` item cannot open the gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkCheckReport {
    pub id: String,
    pub revision: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub finished_at: i64,
    pub checks: Vec<NetworkCheckItem>,
    pub passed_required: u32,
    pub total_required: u32,
    pub can_open: bool,
}

/// Connectivity of the current network path, independent of the environment grant.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum NetworkConnectivity {
    #[default]
    Normal,
    Rechecking,
    Blocked,
}

/// Traffic delivered to or accepted from the VM by its internet gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkTraffic {
    #[cfg_attr(test, ts(type = "number"))]
    pub gateway_session: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub sampled_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub rx_bytes: u64,
    #[cfg_attr(test, ts(type = "number"))]
    pub tx_bytes: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub rx_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub tx_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkHistoryEntry {
    #[cfg_attr(test, ts(type = "number"))]
    pub id: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub kind: String,
    pub message: String,
    pub failed_checks: u32,
    pub report: Option<NetworkCheckReport>,
}

/// The gateway as it runs now. `available` is false where there is no
/// gateway (a local guest, or no machine yet).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatus {
    pub available: bool,
    pub route: NetworkRoute,
    pub running: bool,
    pub error: Option<String>,
    /// The latest gate transition or verification failure. Kept separate from
    /// `error`, which describes the gateway process itself.
    pub gate_error: Option<String>,
    /// A confirmed violation of the saved exit policy.
    pub exit_policy_error: Option<String>,
    pub gate: NetworkGateMode,
    #[serde(default)]
    pub connectivity: NetworkConnectivity,
    #[serde(default)]
    pub failed_checks: u32,
    pub connectivity_error: Option<String>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub gateway_session: Option<u64>,
    pub paused: bool,
    pub auto_recover: bool,
    /// When a check fails only because the machine's time zone differs from
    /// the verified exit, the Mac sets the machine to the exit's zone and
    /// checks again.
    #[serde(default = "network_status_true")]
    pub region_follows_exit: bool,
    /// Stable identity of the route and guest environment this grant covers.
    pub revision: String,
    /// Monotonic in-process token used to discard stale checks.
    #[cfg_attr(test, ts(type = "number"))]
    pub generation: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub checked_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub expires_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub next_retry_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number"))]
    pub retry_attempt: u32,
    pub report: Option<NetworkCheckReport>,
}

fn network_status_true() -> bool {
    true
}

impl Default for NetworkStatus {
    fn default() -> Self {
        Self {
            available: false,
            route: NetworkRoute::default(),
            running: false,
            error: None,
            gate_error: None,
            exit_policy_error: None,
            gate: NetworkGateMode::Closed,
            connectivity: NetworkConnectivity::Normal,
            failed_checks: 0,
            connectivity_error: None,
            gateway_session: None,
            paused: false,
            auto_recover: true,
            region_follows_exit: true,
            revision: String::new(),
            generation: 0,
            checked_at: None,
            expires_at: None,
            next_retry_at: None,
            retry_attempt: 0,
            report: None,
        }
    }
}

/// Facts read by the fixed guest-side probe. The probe never accepts a host,
/// port, command, path or proxy setting from its caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GuestNetworkProbe {
    #[cfg_attr(test, ts(type = "number"))]
    pub checked_at: i64,
    pub hostname: Option<String>,
    pub username: Option<String>,
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub dns_ok: bool,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub dns_ms: Option<u32>,
    pub connect_ok: bool,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub connect_ms: Option<u32>,
    pub https_ok: bool,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub https_ms: Option<u32>,
    pub public_ip: Option<String>,
    pub trace_location: Option<String>,
    pub ipv6_blocked: bool,
    pub private_blocked: bool,
    /// Only applicable while unrestricted public traffic is closed.
    pub unlisted_blocked: Option<bool>,
    pub udp_blocked: bool,
    pub error: Option<String>,
}

/// The browser-visible values compared with the installed browser settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserEnvironmentState {
    pub timezone: Option<String>,
    pub language: Option<String>,
    pub languages: Vec<String>,
    pub platform: Option<String>,
}

/// The managed browser's editable settings (the Mac's configuration). With
/// `followMachine` the region, hardware hints and platform come from the
/// machine and keep following it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserSettings {
    pub follow_machine: bool,
    pub timezone: String,
    pub locale: String,
    pub user_agent: String,
    pub platform: String,
    pub hardware_concurrency: u32,
    pub device_memory: f64,
}

/// Everything the browser environment panel shows: the settings, and (when
/// asked to check and the machine runs) the installed and live state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserEnvironmentView {
    pub enabled: bool,
    pub machine_running: bool,
    pub settings: Option<BrowserSettings>,
    pub probe: Option<BrowserEnvironmentProbe>,
    /// Whether the live pages use the installed settings; `None` when no
    /// live page was observed. Judged on the Mac, like the network check.
    pub live_matches: Option<bool>,
    /// Unix milliseconds of `probe`.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub checked_at: Option<i64>,
}

/// What the live Chrome could tell. Only `Observed` carries values; the
/// other states are normal (Chrome closed, no page, tabs asleep) and never
/// block the machine's internet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum BrowserLive {
    Observed,
    #[default]
    NotRunning,
    /// Running, but no web page answered (none open, or asleep).
    Idle,
    /// Could not be judged (pages disagree, the audit failed).
    Unavailable,
}

/// The managed browser's environment, as the network check sees it.
///
/// `installed` (the guest's profile file) against `expected` (the Mac's
/// configuration) decides whether the browser is set up correctly, and that
/// is what the gate requires. `observed` is the live Chrome: advisory only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BrowserEnvironmentProbe {
    pub enabled: bool,
    pub expected: BrowserEnvironmentState,
    /// The installed profile in the guest; `None` when it could not be read.
    pub installed: Option<BrowserEnvironmentState>,
    pub live: BrowserLive,
    pub observed: Option<BrowserEnvironmentState>,
    /// Why the live browser has no observation, in a few words.
    pub live_detail: Option<String>,
    /// A problem with the installed settings (blocks: they must be right).
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum IpType {
    Mobile,
    Residential,
    Commercial,
    Server,
    Unknown,
}

/// Network type information from a separate lookup about the verified address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct IpClassification {
    pub ip_type: IpType,
    pub inferred: bool,
    pub organization: Option<String>,
    pub asn: Option<u32>,
    pub source: String,
}

/// Where a public address is, from one public lookup service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GeoInfo {
    pub ip: String,
    pub country_code: Option<String>,
    pub continent_code: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    /// Provider (ISP or organisation), as the service names it.
    pub isp: Option<String>,
    pub ip_type: Option<IpType>,
    pub asn: Option<u32>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub timezone: Option<String>,
    /// The service that answered.
    pub source: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub checked_at: i64,
}

/// One prefix of a chain: the Mac through hops `0..=index`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct HopCheck {
    pub node_id: String,
    pub ok: bool,
    pub latency_ms: Option<u32>,
    pub detail: String,
}

/// A chain checked prefix by prefix. `broken_at` is the first hop that
/// could not be reached (the link into it is the broken one); later hops
/// were not tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct RouteCheck {
    pub hops: Vec<HopCheck>,
    pub broken_at: Option<u32>,
    #[cfg_attr(test, ts(type = "number"))]
    pub checked_at: i64,
}

/// Where a node's server is (its host resolved on the Mac, then looked up).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct NodePlace {
    pub node_id: String,
    pub host: String,
    pub geo: Option<GeoInfo>,
    pub error: Option<String>,
}

// -------------------------------------------------------------------- agents

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    pub kind: AgentKind,
    pub name: String,
    pub package: String,
    pub requires_node: bool,
    pub installed: bool,
    pub binary: Option<String>,
    /// Version and authentication checks ran for this installation.
    #[serde(default)]
    pub cli_checked: bool,
    pub version: Option<String>,
    pub config_path: String,
    pub config_exists: bool,
    pub config_error: Option<String>,
    pub model_key: String,
    pub model: Option<String>,
    pub authenticated: bool,
    pub auth_detail: String,
    #[serde(default)]
    pub claude_login: Option<ClaudeLogin>,
}

/// How an MCP server is reached. Each agent stores it in its own format
/// (see `agents/*.rs`); this is the common shape the UI edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum McpTransport {
    /// A local process speaking MCP over stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: std::collections::BTreeMap<String, String>,
    },
    /// A remote server (streamable HTTP, or SSE where the agent says so).
    Http {
        url: String,
        #[serde(default)]
        headers: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        sse: bool,
    },
}

/// One MCP server from an agent's native config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct McpServer {
    pub name: String,
    pub transport: McpTransport,
    pub enabled: bool,
}

/// A skill folder (`<dir>/<name>/SKILL.md`, the shared Agent Skills format).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    /// The skill's folder.
    pub path: String,
    /// `user` (the agent's home folder) or `project` (the workspace).
    pub scope: String,
}

/// A skill opened in the settings editor: its folder, front matter and body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillFile {
    pub path: String,
    pub name: String,
    pub description: String,
    pub body: String,
    pub scope: String,
}

/// MCP servers and skills an agent keeps on disk, for Settings → Agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentExtensions {
    pub kind: AgentKind,
    /// The file holding the agent's own MCP servers; `None` when the agent
    /// has no native MCP support.
    pub mcp_path: Option<String>,
    /// Whether a server can be switched off without removing it.
    pub mcp_can_disable: bool,
    pub mcp_servers: Vec<McpServer>,
    /// Why the MCP file could not be read (it is then left untouched).
    pub mcp_error: Option<String>,
    pub skills_path: String,
    pub skills: Vec<SkillEntry>,
}

/// Something the composer can insert: a slash command or a skill, with the
/// exact text the agent understands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentCommand {
    pub name: String,
    pub description: Option<String>,
    /// Argument hint, e.g. `<file>`.
    pub hint: Option<String>,
    /// `command` (typed with `/`) or `skill` (typed with `$`).
    pub kind: String,
    /// Where it comes from: `builtin`, `user`, `project`, `extension`, …
    pub source: Option<String>,
    /// Text the composer inserts, e.g. `/review ` or `$pdf `.
    pub insert: String,
}

/// Live state of one MCP server as the running agent reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct McpStatus {
    pub name: String,
    /// `connected`, `failed`, `disabled`, `needsAuth`, `pending` or the
    /// agent's own word.
    pub status: String,
    pub detail: Option<String>,
    pub tools: Option<u32>,
}

/// What an agent offers in one workspace, asked from the agent itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentCatalog {
    pub commands: Vec<AgentCommand>,
    pub mcp: Vec<McpStatus>,
    /// Set when the agent could not be asked; the lists are then empty.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AgentConfigText {
    pub kind: AgentKind,
    pub path: String,
    pub exists: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct BackupEntry {
    pub path: String,
    pub name: String,
    #[cfg_attr(test, ts(type = "number"))]
    pub created_at: i64,
    #[cfg_attr(test, ts(type = "number"))]
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub ok: bool,
    pub lines: Vec<String>,
}

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

// --------------------------------------------------------------------- clock

/// The guest's clock. Every guest timestamp is Unix milliseconds on this
/// clock; the UI compares it with the Mac's to place them on one time line.
/// `timezone` is informational: the UI always shows times in the Mac's zone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GuestClock {
    #[cfg_attr(test, ts(type = "number"))]
    pub now_ms: i64,
    pub timezone: Option<String>,
}

/// A read-only observation of the running guest, collected on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GuestMachineObservation {
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub kernel: Option<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub uptime_seconds: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub root_total_bytes: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub root_available_bytes: Option<u64>,
}

// ------------------------------------------------------------------- monitor

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MonitorSample {
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub cpu_percent: Option<f32>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub memory_used: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub memory_total: Option<u64>,
    pub latency_ms: Option<u32>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub download_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub upload_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub disk_read_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub disk_write_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub swap_used: Option<u64>,
    /// `None` when the machine has no swap.
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub swap_total: Option<u64>,
    /// Load averages over 1, 5 and 15 minutes.
    #[serde(default)]
    pub load: Option<LoadAverage>,
    #[serde(default)]
    pub cpu_count: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

/// One process in the machine, for the performance details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// The command line, shortened; empty for kernel threads.
    pub command: String,
    /// Share of the whole machine (all cores), 0–100.
    pub cpu_percent: f32,
    #[cfg_attr(test, ts(type = "number"))]
    pub memory: u64,
    /// Seconds since the process started.
    #[cfg_attr(test, ts(type = "number"))]
    pub run_time: u64,
}

/// The busiest processes, asked for only while the details are open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProcessList {
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub total: u32,
    pub by_cpu: Vec<ProcessInfo>,
    pub by_memory: Vec<ProcessInfo>,
}

// ------------------------------------------------------------------ settings

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    /// `host:port` used for TCP-connect latency samples.
    pub latency_target: String,
    /// `host:port` used to check whether the network works directly.
    pub check_target: String,
    pub default_agent: AgentKind,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            latency_target: "1.1.1.1:443".into(),
            check_target: "www.google.com:443".into(),
            default_agent: AgentKind::Claude,
        }
    }
}

// ------------------------------------------------------------- the machine

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum MachineState {
    NotInstalled,
    Stopped,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum GuestConnection {
    Offline,
    Connecting,
    /// Copying a matching cabind into the VM.
    Updating,
    Connected,
}

/// The VM as seen from the Mac.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MachineStatus {
    pub state: MachineState,
    pub connection: GuestConnection,
    /// What the Mac is doing to the VM right now ("Installing…", "Stopping…").
    pub busy: Option<String>,
    pub ssh_port: Option<u16>,
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_size: Option<String>,
    /// The in-app desktop (headless Sway over VNC) is installed.
    pub display: bool,
    /// Guest operating system id (`os::RELEASES`), once installed.
    pub os: Option<String>,
    pub browser: bool,
    pub device: bool,
    pub guest_version: Option<String>,
    /// The VM's Linux user and hostname (from the install).
    pub username: Option<String>,
    pub hostname: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase", default)]
pub struct MachineInstallOptions {
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_size: Option<String>,
    pub username: Option<String>,
    pub hostname: Option<String>,
    pub device: bool,
    /// Archive mirror for apt in the VM, matching the OS (`None`: the
    /// image's default archive).
    pub apt_mirror: Option<String>,
    /// Guest operating system id (`None`: the default, Ubuntu 24.04).
    pub os: Option<String>,
}

/// An operating system the installer offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct OsChoice {
    pub id: String,
    pub name: String,
    /// A development release with daily images.
    pub testing: bool,
    /// Archive mirrors offered for it, the default first.
    pub mirrors: Vec<String>,
}

/// Result of testing one ARM64 package mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MirrorResult {
    pub uri: String,
    pub host: String,
    /// The archive the image uses when no mirror is chosen.
    pub default: bool,
    /// ok | noArm64 | unreachable
    pub status: String,
    pub latency_ms: Option<u32>,
    pub speed_kbps: Option<u32>,
    pub detail: String,
}

/// One pipeline step during a machine install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct InstallStep {
    pub id: String,
    pub title: String,
    /// pending | running | done | cached | failed | rolledBack
    pub state: String,
    pub detail: Option<String>,
    pub log_tail: Vec<String>,
    pub log_path: Option<String>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub seconds: Option<u64>,
    /// Unix milliseconds.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub applied_at: Option<i64>,
    pub fingerprint: Option<String>,
    /// Bytes, total and speed while the step downloads (the cloud image).
    #[serde(default)]
    pub download: Option<DownloadProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    #[cfg_attr(test, ts(type = "number"))]
    pub done_bytes: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub total_bytes: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub bytes_per_sec: Option<u64>,
    /// All bytes are here and the checksum is being verified.
    pub verifying: bool,
}

/// What an install will do (the wizard's "automation hints").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    pub os: String,
    pub os_name: String,
    pub os_choices: Vec<OsChoice>,
    /// Where the newest build is found; it is pinned when the install starts.
    pub image_index: String,
    pub qemu_release: String,
    pub qemu_installed: bool,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_size: String,
    pub network: String,
    pub accel: String,
    pub firmware: Option<String>,
    pub disk_path: String,
    pub seed_path: String,
    pub username: String,
    pub hostname: String,
    pub ssh_port: u16,
    pub apt_mirror: Option<String>,
    pub steps: Vec<InstallStep>,
}

/// What `app_subscribe` returns on the Mac: the machine, plus the guest's
/// snapshot when cabind is connected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct HostSnapshot {
    pub machine: MachineStatus,
    pub guest: Option<AppSnapshot>,
    /// The running or most recent machine start.
    pub start: Option<StartReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StartFact {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum StartPhaseState {
    Pending,
    Running,
    Done,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StartPhase {
    pub id: String,
    pub title: String,
    pub state: StartPhaseState,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub started_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub finished_at: Option<i64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum StartState {
    Running,
    Succeeded,
    Failed,
}

/// Live report of a machine start, from the Mac app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StartReport {
    pub state: StartState,
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at: i64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub finished_at: Option<i64>,
    pub error: Option<String>,
    /// QEMU path and version, accelerator, firmware, resources, ports, pid…
    pub facts: Vec<StartFact>,
    pub phases: Vec<StartPhase>,
    pub log_path: Option<String>,
    pub log_tail: Vec<String>,
    /// The guest's serial console (only output from this boot).
    pub serial_path: Option<String>,
    pub serial_tail: Vec<String>,
}

// --------------------------------------------------------- snapshot + events

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct AppSnapshot {
    pub workspaces: Vec<Workspace>,
    pub sessions: Vec<Session>,
    pub agents: Vec<AgentStatus>,
    pub environment: EnvironmentState,
    /// The node catalog and the route are the Mac's; it fills these in.
    #[serde(default)]
    pub proxy: ProxyCatalog,
    #[serde(default)]
    pub network: NetworkStatus,
    pub operations: Vec<OperationRecord>,
    pub settings: AppSettings,
    pub data_dir: String,
    pub hostname: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AppEvent {
    Operation {
        record: OperationRecord,
    },
    SessionOutput {
        id: String,
        generation: u32,
        #[cfg_attr(test, ts(type = "number"))]
        offset: u64,
        data: String,
    },
    TerminalStatus {
        id: String,
        generation: u32,
        status: SessionStatus,
    },
    SessionStatus {
        id: String,
        status: SessionStatus,
    },
    ChatItem {
        id: String,
        item: ChatItem,
    },
    ChatPlan {
        id: String,
        plan: PlanReview,
    },
    ChatSubagent {
        id: String,
        task: SubagentTask,
    },
    ChatTask {
        id: String,
        task: ChatAsyncTask,
    },
    ChatState {
        id: String,
        state: ChatState,
    },
    ChatModel {
        id: String,
        model: Option<String>,
    },
    ChatPermission {
        id: String,
        permission: ChatPermissionMode,
    },
    ChatUsage {
        id: String,
        turn: ChatTurnUsage,
    },
    ChatControls {
        id: String,
        controls: ChatControls,
    },
    Monitor {
        sample: MonitorSample,
    },
    Agents {
        agents: Vec<AgentStatus>,
    },
    /// The Mac's node catalog and gateway route changed.
    Network {
        catalog: ProxyCatalog,
        status: NetworkStatus,
    },
    Environment {
        state: EnvironmentState,
    },
    Library {
        workspaces: Vec<Workspace>,
        sessions: Vec<Session>,
    },
    /// Sent by the Mac app: the machine changed.
    Machine {
        status: MachineStatus,
    },
    /// Sent by the Mac app after (re)connecting to the guest.
    Snapshot {
        snapshot: Box<AppSnapshot>,
    },
    /// Sent by the Mac app about once a second while installing the machine.
    Install {
        steps: Vec<InstallStep>,
    },
    /// Sent by the Mac app about twice a second while the machine starts,
    /// and as the guest link comes up afterwards.
    Start {
        report: Box<StartReport>,
    },
    /// A synced project's state changed (Mac app; see `sync`).
    Sync {
        status: SyncStatus,
    },
}

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
