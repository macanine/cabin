use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

use super::AgentKind;

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
