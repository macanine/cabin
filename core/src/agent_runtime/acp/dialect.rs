//! Provider-specific ACP dialect implementations.
//!
//! Generic ACP protocol mechanics (`mod.rs`, `view.rs`) delegate provider quirks
//! — capability advertisement, custom notifications, child-scope routing,
//! plan/subagent tool extraction, model effort suffixes, and out-of-band
//! subagent hydration — through [`AcpDialect`].

use std::borrow::Cow;
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};

use super::antigravity_subagents;
use super::launch;
use crate::agent_runtime::context::ChatContext;
use crate::chat::Sink;
use crate::model::{AgentKind, ChatConfigOption, ChatPermissionMode, SubagentState};
use crate::store::Store;

/// Client capabilities and runtime feature flags advertised/supported by a dialect.
#[derive(Debug, Clone, Copy)]
pub(super) struct DialectCaps {
    pub client_fs: bool,
    pub client_terminal: bool,
    pub subagent_transcript: bool,
    pub air_async_tasks: bool,
    pub opencode_child_updates: bool,
    pub notices: bool,
    pub compaction: bool,
    pub plans: bool,
    pub subagents: bool,
}

impl DialectCaps {
    /// Build the ACP `initialize.clientCapabilities` JSON object for this dialect.
    pub(super) fn client_capabilities(self) -> Value {
        let mut caps = json!({
            "fs": {
                "readTextFile": self.client_fs,
                "writeTextFile": self.client_fs,
            },
            "terminal": self.client_terminal,
            "elicitation": {"form": {}, "url": {}},
        });
        if self.air_async_tasks {
            caps["_meta"] = json!({
                "subagent-transcript": self.subagent_transcript,
                "jetbrains": {
                    "air": {
                        "version": 1,
                        "capabilities": ["asyncTasks"],
                    }
                }
            });
        } else if self.opencode_child_updates {
            caps["_meta"] = json!({"opencode/child-session-updates": true});
        }
        let mut session = serde_json::Map::new();
        if self.notices {
            session.insert("notices".into(), json!({}));
        }
        if self.compaction {
            session.insert("compaction".into(), json!({}));
        }
        if !session.is_empty() {
            caps["session"] = Value::Object(session);
        }
        caps
    }
}

/// Child-agent routing clue extracted from an incoming ACP `session/update` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ChildScopeHint<'a> {
    /// Child update correlated to a subagent tool call on the parent turn.
    ParentTool { tool_call_id: &'a str },
    /// Child update carrying an explicit child session ID and display title.
    ChildSession { session_id: &'a str, title: &'a str },
    /// Out-of-band external child session UUID encoded in `toolCallId`.
    ExternalChildUuid { child_uuid: &'a str },
}

/// Lifecycle settlement extracted from a child `session_info_update` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChildSettlement {
    pub session_id: String,
    pub title: String,
    pub state: Option<SubagentState>,
}

/// Provider dialect encapsulating all non-standard ACP behaviors and extensions.
pub(super) trait AcpDialect: Send + Sync {
    /// Underlying agent kind, or `None` for the generic protocol dialect.
    fn kind(&self) -> Option<AgentKind>;

    /// Capability flags advertised during `initialize` and surfaced to UI capabilities.
    fn caps(&self) -> DialectCaps;

    /// Translate provider-custom JSON-RPC notifications into standard `"session/update"` frames.
    fn translate_notification<'a>(
        &self,
        method: &str,
        params: &'a Value,
    ) -> Option<(&'static str, Cow<'a, Value>)> {
        if method == "session/update" {
            return Some(("session/update", Cow::Borrowed(params)));
        }
        None
    }

    /// Inspect an update payload for provider-specific child routing metadata.
    fn child_scope_hint<'a>(
        &self,
        _update: &'a Value,
        _main_session_id: Option<&str>,
    ) -> Option<ChildScopeHint<'a>> {
        None
    }

    /// Resolve an [`ChildScopeHint::ExternalChildUuid`] into a Cabin task ID when supported.
    fn resolve_external_child(
        &self,
        _native_session_id: &str,
        _child_uuid: &str,
        _ctx: &ChatContext,
        _root: &Path,
    ) -> Option<String> {
        None
    }

    /// Extract child-session status settlement from a `session_info_update` payload.
    fn child_settlement(&self, _update: &Value) -> Option<ChildSettlement> {
        None
    }

    /// Extract plan entries from a tool call if this provider reports plans as tool calls.
    fn extract_tool_plan(
        &self,
        _name: Option<&str>,
        _title: &str,
        _raw_input: &Value,
    ) -> Option<Vec<Value>> {
        None
    }

    /// Whether a tool call represents launching a subagent for this dialect.
    fn is_subagent_launch(
        &self,
        kind: Option<&str>,
        name: Option<&str>,
        _title: &str,
        _meta: &Value,
        _raw_input: &Value,
    ) -> bool {
        is_generic_subagent_launch(kind, name)
    }

    /// Whether a subagent launch tool completes immediately while child work runs out-of-band.
    fn is_async_subagent_launch(
        &self,
        _name: Option<&str>,
        _title: &str,
        _raw_input: &Value,
    ) -> bool {
        false
    }

    /// Sync out-of-band subagent transcripts/logs when the dialect uses external files.
    fn sync_subagents(
        &self,
        _native_session_id: &str,
        _root: &Path,
        _ctx: &ChatContext,
        _throttled: bool,
    ) {
    }

    /// Split provider model IDs carrying an effort suffix (e.g. `claude-sonnet-4-6-high`).
    fn split_model_effort<'a>(&self, model: &'a str) -> Option<(&'a str, &'a str)> {
        split_effort_suffix(model)
    }

    /// Option categories owned by the permission picker for this dialect.
    fn owned_categories(&self) -> &'static [&'static str] {
        self.kind().map_or(&[], launch::owned_categories)
    }

    /// Option values implementing the chosen permission mode, in priority order.
    fn settings_for(
        &self,
        permission: ChatPermissionMode,
    ) -> Vec<(&'static str, &'static [&'static str])> {
        self.kind()
            .map_or_else(Vec::new, |agent| launch::settings_for(agent, permission))
    }

    /// Whether the requested model delegates selection to the CLI's default.
    fn use_agent_default(&self, model: &str) -> bool {
        self.kind()
            .is_some_and(|agent| launch::use_agent_default(agent, model))
    }

    /// Whether the CLI owns native permission enforcement and user approvals.
    fn native_permissions(&self) -> bool {
        self.kind().is_some_and(launch::native_permissions)
    }

    /// Infer Cabin's permission mode from the dialect's current native options.
    fn permission_from(&self, options: &[ChatConfigOption]) -> Option<ChatPermissionMode> {
        self.kind()
            .and_then(|agent| launch::permission_from(agent, options))
    }

    /// Whether the dialect's current options already satisfy the requested mode.
    fn mode_matches_permission(
        &self,
        options: &[ChatConfigOption],
        permission: ChatPermissionMode,
    ) -> bool {
        self.kind()
            .is_some_and(|agent| launch::mode_matches_permission(agent, options, permission))
    }

    /// Whether a permission prompt should be auto-allowed by the client for this dialect.
    fn auto_allows_request(
        &self,
        permission: ChatPermissionMode,
        tool_kind: &str,
        command_text: &str,
        has_plan: bool,
    ) -> bool {
        self.kind().is_some_and(|agent| {
            launch::auto_allows_request(agent, permission, tool_kind, command_text, has_plan)
        })
    }
}

/// Return the [`AcpDialect`] implementation for a specific [`AgentKind`].
pub(super) fn for_agent(agent: AgentKind) -> Arc<dyn AcpDialect> {
    match agent {
        AgentKind::Antigravity => Arc::new(AntigravityDialect),
        AgentKind::Claude => Arc::new(ClaudeDialect),
        AgentKind::Codex => Arc::new(CodexDialect),
        AgentKind::Opencode => Arc::new(OpencodeDialect),
        AgentKind::Pi => Arc::new(PiDialect),
    }
}

/// Return a protocol-only dialect with no provider-specific extensions.
#[cfg(test)]
pub(super) fn generic() -> Arc<dyn AcpDialect> {
    Arc::new(GenericDialect)
}

/// Enrich a stored chat snapshot with any out-of-band subagent records recorded on disk.
pub fn enrich_stored(
    store: &Store,
    sink: &std::sync::RwLock<Option<Sink>>,
    chat_id: &str,
    parent_running: bool,
) {
    antigravity_subagents::enrich_stored(store, sink, chat_id, parent_running);
}

#[cfg(test)]
struct GenericDialect;

#[cfg(test)]
impl AcpDialect for GenericDialect {
    fn kind(&self) -> Option<AgentKind> {
        None
    }

    fn caps(&self) -> DialectCaps {
        DialectCaps {
            client_fs: true,
            client_terminal: true,
            subagent_transcript: false,
            air_async_tasks: false,
            opencode_child_updates: false,
            notices: false,
            compaction: false,
            plans: false,
            subagents: false,
        }
    }
}

struct AntigravityDialect;

impl AcpDialect for AntigravityDialect {
    fn kind(&self) -> Option<AgentKind> {
        Some(AgentKind::Antigravity)
    }

    fn caps(&self) -> DialectCaps {
        // Advertising client file writes to Antigravity swaps its native diff
        // edits for whole-file client_edit_file, and subagent client_view_file
        // reads can hang. Keep native fs/terminal tools for Antigravity.
        DialectCaps {
            client_fs: false,
            client_terminal: false,
            subagent_transcript: false,
            air_async_tasks: false,
            opencode_child_updates: false,
            notices: false,
            compaction: false,
            plans: false,
            subagents: true,
        }
    }

    fn child_scope_hint<'a>(
        &self,
        update: &'a Value,
        _main_session_id: Option<&str>,
    ) -> Option<ChildScopeHint<'a>> {
        let child_uuid = update["toolCallId"].as_str().and_then(antigravity_child_uuid)?;
        Some(ChildScopeHint::ExternalChildUuid { child_uuid })
    }

    fn resolve_external_child(
        &self,
        native_session_id: &str,
        child_uuid: &str,
        ctx: &ChatContext,
        root: &Path,
    ) -> Option<String> {
        antigravity_subagents::resolve_child_task_id(native_session_id, child_uuid, ctx, root)
    }

    fn extract_tool_plan(
        &self,
        name: Option<&str>,
        title: &str,
        raw_input: &Value,
    ) -> Option<Vec<Value>> {
        if title != "Run update_steps"
            && title != "Running update_steps"
            && name != Some("update_steps")
        {
            return None;
        }
        let steps = raw_input.get("steps")?.as_array()?;
        let entries: Vec<Value> = steps
            .iter()
            .filter_map(|step| {
                let title = step.get("title").and_then(Value::as_str)?.trim();
                if title.is_empty() {
                    return None;
                }
                let status = match step.get("status").and_then(Value::as_str) {
                    Some("done") | Some("completed") => "completed",
                    Some("in_progress") => "in_progress",
                    Some(other) => other,
                    None => "pending",
                };
                Some(json!({
                    "content": title,
                    "priority": "medium",
                    "status": status,
                }))
            })
            .collect();
        Some(entries)
    }

    fn is_subagent_launch(
        &self,
        kind: Option<&str>,
        name: Option<&str>,
        title: &str,
        _meta: &Value,
        raw_input: &Value,
    ) -> bool {
        is_generic_subagent_launch(kind, name)
            || is_antigravity_invoke_subagent(name, title, raw_input)
    }

    fn is_async_subagent_launch(
        &self,
        name: Option<&str>,
        title: &str,
        raw_input: &Value,
    ) -> bool {
        is_antigravity_invoke_subagent(name, title, raw_input)
    }

    fn sync_subagents(
        &self,
        native_session_id: &str,
        root: &Path,
        ctx: &ChatContext,
        throttled: bool,
    ) {
        antigravity_subagents::sync_live(native_session_id, root, ctx, throttled);
    }
}

struct ClaudeDialect;

impl AcpDialect for ClaudeDialect {
    fn kind(&self) -> Option<AgentKind> {
        Some(AgentKind::Claude)
    }

    fn caps(&self) -> DialectCaps {
        DialectCaps {
            client_fs: true,
            client_terminal: true,
            subagent_transcript: true,
            air_async_tasks: true,
            opencode_child_updates: false,
            notices: true,
            compaction: true,
            plans: true,
            subagents: true,
        }
    }

    fn child_scope_hint<'a>(
        &self,
        update: &'a Value,
        _main_session_id: Option<&str>,
    ) -> Option<ChildScopeHint<'a>> {
        let tool_call_id = update
            .pointer("/_meta/claudeCode/parentToolUseId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())?;
        Some(ChildScopeHint::ParentTool { tool_call_id })
    }

    fn is_subagent_launch(
        &self,
        kind: Option<&str>,
        name: Option<&str>,
        _title: &str,
        meta: &Value,
        _raw_input: &Value,
    ) -> bool {
        is_generic_subagent_launch(kind, name)
            || meta
                .pointer("/claudeCode/subagent")
                .and_then(Value::as_bool)
                == Some(true)
    }
}

struct CodexDialect;

impl AcpDialect for CodexDialect {
    fn kind(&self) -> Option<AgentKind> {
        Some(AgentKind::Codex)
    }

    fn caps(&self) -> DialectCaps {
        DialectCaps {
            client_fs: true,
            client_terminal: true,
            subagent_transcript: false,
            air_async_tasks: true,
            opencode_child_updates: false,
            notices: true,
            compaction: true,
            plans: true,
            subagents: true,
        }
    }
}

struct OpencodeDialect;

impl AcpDialect for OpencodeDialect {
    fn kind(&self) -> Option<AgentKind> {
        Some(AgentKind::Opencode)
    }

    fn caps(&self) -> DialectCaps {
        DialectCaps {
            client_fs: true,
            client_terminal: true,
            subagent_transcript: false,
            air_async_tasks: false,
            opencode_child_updates: true,
            notices: false,
            compaction: true,
            plans: false,
            subagents: true,
        }
    }

    fn translate_notification<'a>(
        &self,
        method: &str,
        params: &'a Value,
    ) -> Option<(&'static str, Cow<'a, Value>)> {
        if method == "session/update" {
            return Some(("session/update", Cow::Borrowed(params)));
        }
        if method.trim_start_matches('_') == "opencode/session/child_update" {
            return opencode_child_update(params)
                .map(|translated| ("session/update", Cow::Owned(translated)));
        }
        None
    }

    fn child_scope_hint<'a>(
        &self,
        update: &'a Value,
        main_session_id: Option<&str>,
    ) -> Option<ChildScopeHint<'a>> {
        let child = update
            .pointer("/_meta/opencode~1child-session")
            .filter(|child| child.is_object())?;
        let session_id = child["id"].as_str().unwrap_or_default();
        if session_id.is_empty() || main_session_id == Some(session_id) {
            return None;
        }
        let title = child["title"].as_str().unwrap_or_default();
        Some(ChildScopeHint::ChildSession { session_id, title })
    }

    fn child_settlement(&self, update: &Value) -> Option<ChildSettlement> {
        let state = update
            .pointer("/_meta/cabin~1child-status/status")
            .and_then(Value::as_str)?;
        let child = update.pointer("/_meta/opencode~1child-session")?;
        let id = child["id"].as_str().unwrap_or_default();
        if id.is_empty() {
            return None;
        }
        let title = child["title"].as_str().unwrap_or_default().to_string();
        let terminal = match state {
            "completed" => Some(SubagentState::Completed),
            "failed" => Some(SubagentState::Failed),
            "stopped" => Some(SubagentState::Stopped),
            _ => None,
        };
        Some(ChildSettlement {
            session_id: id.to_string(),
            title,
            state: terminal,
        })
    }
}

struct PiDialect;

impl AcpDialect for PiDialect {
    fn kind(&self) -> Option<AgentKind> {
        Some(AgentKind::Pi)
    }

    fn caps(&self) -> DialectCaps {
        DialectCaps {
            client_fs: true,
            client_terminal: true,
            subagent_transcript: false,
            air_async_tasks: false,
            opencode_child_updates: false,
            notices: false,
            compaction: false,
            plans: false,
            subagents: false,
        }
    }
}

fn is_generic_subagent_launch(kind: Option<&str>, name: Option<&str>) -> bool {
    kind == Some("agent")
        || matches!(
            name.unwrap_or_default(),
            "Agent" | "Task" | "spawn_agent" | "spawn_subagent"
        )
}

fn antigravity_child_uuid(tool_call_id: &str) -> Option<&str> {
    let (prefix, rest) = tool_call_id.split_once(':')?;
    if rest.is_empty() || prefix.len() != 36 {
        return None;
    }
    let bytes = prefix.as_bytes();
    if bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && prefix
            .bytes()
            .enumerate()
            .all(|(i, b)| matches!(i, 8 | 13 | 18 | 23) || b.is_ascii_hexdigit())
    {
        Some(prefix)
    } else {
        None
    }
}

fn is_antigravity_invoke_subagent(name: Option<&str>, title: &str, raw_input: &Value) -> bool {
    name == Some("invoke_subagent")
        || matches!(title, "Run invoke_subagent" | "Running invoke_subagent")
        || (raw_input.get("Role").and_then(Value::as_str).is_some()
            && (raw_input.get("TypeName").is_some() || raw_input.get("Prompt").is_some()))
}

fn split_effort_suffix(model: &str) -> Option<(&str, &str)> {
    for suffix in ["xhigh", "high", "medium", "low", "max"] {
        if let Some(family) = model.strip_suffix(&format!("-{suffix}"))
            && !family.is_empty()
        {
            return Some((family, suffix));
        }
    }
    None
}

/// Turn OpenCode 2's child-session notification into the same
/// `session/update` shape the parent session uses. Status frames carry a
/// private, client-owned marker so the view can settle the subagent without
/// treating lifecycle metadata as transcript content.
fn opencode_child_update(params: &Value) -> Option<Value> {
    let root = params["rootSessionId"]
        .as_str()
        .filter(|id| !id.is_empty())?;
    let child = params["childSessionId"]
        .as_str()
        .filter(|id| !id.is_empty())?;
    let parent = params["parentSessionId"].as_str().unwrap_or(root);
    let title = params["title"].as_str().unwrap_or_default();
    let child_meta = json!({
        "opencode/child-session": {
            "id": child,
            "parentID": parent,
            "depth": params["depth"].as_u64().unwrap_or(1),
            "title": title,
        }
    });
    match params["type"].as_str()? {
        "update" => {
            let mut update = params["update"].as_object()?.clone();
            // A child compaction belongs to the child, not to the root
            // transcript. Its durable items remain visible in the subagent.
            if update.get("sessionUpdate").and_then(Value::as_str) == Some("compaction_update") {
                return None;
            }
            let meta = update
                .entry("_meta")
                .or_insert_with(|| json!({}))
                .as_object_mut()?;
            meta.entry("opencode/child-session")
                .or_insert(child_meta["opencode/child-session"].clone());
            Some(json!({"sessionId": root, "update": Value::Object(update)}))
        }
        "status" => {
            let state = match params["status"].as_str().unwrap_or_default() {
                "completed" | "done" | "idle" => "completed",
                "failed" | "error" => "failed",
                "cancelled" | "canceled" | "aborted" | "stopped" => "stopped",
                "created" | "running" | "active" | "started" => "active",
                _ => return None,
            };
            let mut meta = child_meta;
            meta["cabin/child-status"] = json!({"status": state});
            Some(json!({
                "sessionId": root,
                "update": {
                    "sessionUpdate": "session_info_update",
                    "_meta": meta,
                },
            }))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialects_expose_isolated_provider_behaviors() {
        let ag = for_agent(AgentKind::Antigravity);
        assert!(!ag.caps().client_fs);
        assert!(!ag.caps().client_terminal);
        assert!(ag.caps().subagents);
        assert!(matches!(
            ag.child_scope_hint(
                &json!({"toolCallId": "550e8400-e29b-41d4-a716-446655440000:call_1"}),
                Some("main"),
            ),
            Some(ChildScopeHint::ExternalChildUuid { .. })
        ));
        assert!(ag.is_async_subagent_launch(Some("invoke_subagent"), "Run invoke_subagent", &json!({})));

        let claude = for_agent(AgentKind::Claude);
        assert!(claude.caps().client_fs);
        assert!(claude.caps().subagent_transcript);
        assert!(claude.caps().air_async_tasks);
        assert!(matches!(
            claude.child_scope_hint(
                &json!({"_meta": {"claudeCode": {"parentToolUseId": "tool_1"}}}),
                Some("main"),
            ),
            Some(ChildScopeHint::ParentTool { tool_call_id: "tool_1" })
        ));
        // Claude must not treat Antigravity toolCallId prefix as external subagent.
        assert!(claude
            .child_scope_hint(
                &json!({"toolCallId": "550e8400-e29b-41d4-a716-446655440000:call_1"}),
                Some("main"),
            )
            .is_none());

        let oc = for_agent(AgentKind::Opencode);
        assert!(oc.caps().opencode_child_updates);
        let status_frame = json!({
            "rootSessionId": "root_1",
            "childSessionId": "child_1",
            "type": "status",
            "status": "completed",
            "title": "Worker",
        });
        let translated = oc.translate_notification("_opencode/session/child_update", &status_frame);
        let (method, payload) = translated.expect("opencode notification");
        assert_eq!(method, "session/update");
        let settlement = oc
            .child_settlement(&payload["update"])
            .expect("settlement");
        assert_eq!(settlement.session_id, "child_1");
        assert_eq!(settlement.state, Some(SubagentState::Completed));

        assert_eq!(
            ag.split_model_effort("gemini-4-neon-high"),
            Some(("gemini-4-neon", "high"))
        );
        assert_eq!(
            ag.split_model_effort("gemini-4-neon-xhigh"),
            Some(("gemini-4-neon", "xhigh"))
        );
        assert_eq!(ag.split_model_effort("gemini-4-neon"), None);
    }
}
