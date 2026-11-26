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
