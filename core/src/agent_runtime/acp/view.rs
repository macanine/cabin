//! ACP `session/update` → `ChatItem`, subagent and control snapshots.
//!
//! The view keeps what one session has streamed so far (the open text
//! segment, every tool call's merged fields, child sessions) and writes only
//! through `ChatContext`. It never guesses from an agent's name: tool kinds,
//! diffs, terminals and child sessions come from the protocol itself.

use crate::util::LockExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::{Value, json};

use super::super::context::{ChatContext, blank_item, file_changes, system_item};
use super::dialect::{AcpDialect, ChildScopeHint};
use super::terminal::Terminals;
use crate::model::{
    ChatAsyncTask, ChatCommand, ChatConfigChoice, ChatConfigOption, ChatContextUse, ChatControls,
    ChatGoal, ChatItem, ChatItemKind, ChatItemStatus, ChatPlanEntry, ChatUsage, SubagentState,
    SubagentTask, ToolAction,
};

/// Where an update belongs: the chat itself, one subagent task, or an
/// unmapped background child update that should not split the parent stream.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Scope {
    Main,
    Task(String),
    Ignore,
}

struct Segment {
    item: ChatItem,
}

#[derive(Default, Clone)]
pub(super) struct Tool {
    pub id: String,
    pub name: Option<String>,
    pub title: String,
    pub kind: Option<String>,
    pub status: String,
    pub raw_input: Value,
    pub raw_output: Value,
    pub meta: Value,
    pub text: Vec<String>,
    pub diffs: Vec<(String, Option<String>, String)>,
    pub terminals: Vec<String>,
    pub locations: Vec<String>,
    scope: Option<Scope>,
    /// Set while the user is asked to approve this call.
    pub waiting: bool,
}

#[derive(Default)]
struct State {
    seq: u64,
    segments: HashMap<Scope, Segment>,
    tools: HashMap<String, Tool>,
    /// Child ACP session id → subagent task id.
    children: HashMap<String, String>,
    /// Tool call that launched a subagent → its subagent task id.
    subagent_tools: HashMap<String, String>,
    /// Child tasks last seen working, checked when the parent moves on.
    active_children: std::collections::HashSet<String>,
    controls: ChatControls,
    /// `session/load` replays history we already hold; drop it.
    replaying: bool,
    turn_started: Option<Instant>,
    first_token: Option<Instant>,
    last_token: Option<Instant>,
    cost_at_turn_start: Option<u64>,
}

pub struct View {
    root: PathBuf,
    dialect: Arc<dyn AcpDialect>,
    session_id: Mutex<Option<String>>,
    state: Mutex<State>,
    terminals: Mutex<Option<Arc<Terminals>>>,
    /// Option categories the permission picker owns; shown hidden.
    owned: Mutex<&'static [&'static str]>,
    goal_actions: Mutex<Vec<String>>,
}

pub const TOOL_PREFIX: &str = "acp-tool:";
const TASK_PREFIX: &str = "acp-agent:";

fn normalize_goal_status(status: &str) -> String {
    let mut out = String::with_capacity(status.len() + 2);
    for (index, ch) in status.trim().chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || ch == '-' {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn goal_value(update: &Value) -> Option<&Value> {
    update
        .pointer("/_meta/goal")
        .or_else(|| update.pointer("/_meta/jetbrains/air/goal"))
        .or_else(|| update.pointer("/_meta/codex/goal"))
}

fn goal_snapshot(update: &Value, actions: &[String]) -> Option<Option<ChatGoal>> {
    let value = goal_value(update)?;
    if value.is_null() {
        return Some(None);
    }
    let objective = value
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())?;
    let number = |key: &str| value.get(key).and_then(Value::as_u64);
    Some(Some(ChatGoal {
        objective: objective.to_string(),
        status: normalize_goal_status(
            value
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("active"),
        ),
        token_budget: number("tokenBudget"),
        tokens_used: number("tokensUsed"),
        time_used_seconds: number("timeUsedSeconds"),
        actions: actions.to_vec(),
    }))
}

impl View {
    #[cfg(test)]
    pub fn new(root: &Path) -> Self {
        Self::with_dialect(root, super::dialect::generic())
    }

    pub(super) fn with_dialect(root: &Path, dialect: Arc<dyn AcpDialect>) -> Self {
        Self {
            root: root.to_path_buf(),
            dialect,
            session_id: Mutex::new(None),
            state: Mutex::new(State::default()),
            terminals: Mutex::new(None),
            owned: Mutex::new(&[]),
            goal_actions: Mutex::new(Vec::new()),
        }
    }

    pub fn set_goal_actions(&self, actions: Vec<String>) {
        *self.goal_actions.locked() = actions;
    }

    pub fn set_owned(&self, categories: &'static [&'static str]) {
        *self.owned.locked() = categories;
    }

    pub fn set_terminals(&self, terminals: Arc<Terminals>) {
        *self.terminals.locked() = Some(terminals);
    }

    pub fn set_session(&self, id: &str) {
        *self.session_id.locked() = Some(id.to_string());
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id.locked().clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.locked()
    }

    pub fn controls(&self) -> ChatControls {
        self.state().controls.clone()
    }

    pub fn edit_controls(&self, ctx: &ChatContext, edit: impl FnOnce(&mut ChatControls)) {
        let owned = *self.owned.locked();
        let controls = {
            let mut state = self.state();
            edit(&mut state.controls);
            for option in &mut state.controls.options {
                option.hidden = option
                    .category
                    .as_deref()
                    .is_some_and(|category| owned.contains(&category));
            }
            state.controls.clone()
        };
        ctx.publish_controls(controls);
    }

    pub fn set_replaying(&self, replaying: bool) {
        self.state().replaying = replaying;
    }

    /// A prompt turn begins: new text segments, fresh timing.
    pub fn begin_turn(&self, ctx: &ChatContext) {
        let controls = {
            let mut state = self.state();
            state.segments.clear();
            state.turn_started = Some(Instant::now());
            state.first_token = None;
            state.last_token = None;
            state.cost_at_turn_start = state
                .controls
                .context
                .as_ref()
                .and_then(|use_| use_.cost_micros);
            // Suggestions answer the previous turn only.
            if state.controls.suggestions.is_empty() {
                None
            } else {
                state.controls.suggestions.clear();
                Some(state.controls.clone())
            }
        };
        if let Some(controls) = controls {
            ctx.publish_controls(controls);
        }
    }

    /// End the open text segments so what streams next lands after a
    /// message the user just added (steering).
    pub fn close_segments(&self, ctx: &ChatContext) {
        let segments: Vec<_> = self.state().segments.drain().collect();
        for (scope, mut segment) in segments {
            segment.item.status = ChatItemStatus::Completed;
            self.write(ctx, &scope, segment.item);
        }
    }

    /// The prompt returned: close text segments and report usage.
    pub fn end_turn(&self, ctx: &ChatContext, turn_id: &str, usage: Option<&Value>) {
        let (segments, chat_usage) = {
            let mut state = self.state();
            let segments: Vec<_> = state.segments.drain().collect();
            let generation_ms = match (state.first_token, state.last_token) {
                (Some(first), Some(last)) => last.duration_since(first).as_millis() as u64,
                _ => 0,
            };
            let context = state.controls.context.clone();
            let cost_micros = match (&context, state.cost_at_turn_start) {
                (
                    Some(ChatContextUse {
                        cost_micros: Some(now),
                        currency,
                        ..
                    }),
                    start,
                ) if currency.as_deref().is_none_or(|currency| currency == "USD") => {
                    Some(now.saturating_sub(start.unwrap_or(0)))
                }
                _ => None,
            };
            let chat_usage = usage.filter(|usage| usage.is_object()).map(|usage| {
                let number = |key: &str| usage[key].as_u64().unwrap_or(0);
                ChatUsage {
                    input: number("inputTokens")
                        + number("cachedReadTokens")
                        + number("cachedWriteTokens"),
                    cached: number("cachedReadTokens"),
                    cache_write: number("cachedWriteTokens"),
                    output: number("outputTokens"),
                    reasoning: number("thoughtTokens"),
                    generation_ms,
                    cost_micros: cost_micros.filter(|cost| *cost > 0),
                    context_tokens: context.as_ref().map(|use_| use_.used),
                    context_window: context.as_ref().map(|use_| use_.size),
                }
            });
            (segments, chat_usage)
        };
        for (scope, mut segment) in segments {
            segment.item.status = ChatItemStatus::Completed;
            self.write(ctx, &scope, segment.item);
        }
        if let Some(usage) = chat_usage {
            ctx.usage(turn_id, usage);
        }
        self.settle_children(ctx);
    }

    fn next_id(&self, state: &mut State, prefix: &str) -> String {
        state.seq += 1;
        format!("{prefix}{}-{}", crate::util::now_millis(), state.seq)
    }

    fn write(&self, ctx: &ChatContext, scope: &Scope, item: ChatItem) {
        match scope {
            Scope::Main => {
                let _ = ctx.upsert(item);
            }
            Scope::Task(task) => {
                let mut item = item;
                // Subagent items are not presented by the store; do it here.
                if matches!(
                    item.kind,
                    ChatItemKind::Tool | ChatItemKind::CommandExecution | ChatItemKind::FileChange
                ) {
                    crate::chat_view::present(&mut item, &self.root);
                }
                let _ = ctx.upsert_subagent_item(task, item);
            }
            Scope::Ignore => {}
        }
    }

    /// Route an update to its scope: a known child session, a dialect child
    /// clue (such as parent tool ID, child-session tag, or external UUID), or
    /// the main chat itself.
    fn scope_of(&self, ctx: &ChatContext, params: &Value, update: &Value) -> Scope {
        let main = self.session_id.locked().clone();
        let session = params["sessionId"].as_str().unwrap_or_default();
        match self.dialect.child_scope_hint(update, main.as_deref()) {
            Some(ChildScopeHint::ParentTool { tool_call_id }) => {
                let task_id = self
                    .state()
                    .subagent_tools
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        self.child_task(ctx, &format!("tool:{tool_call_id}"), "Subagent", "", None)
                    });
                return Scope::Task(task_id);
            }
            Some(ChildScopeHint::ChildSession { session_id, title }) => {
                return Scope::Task(self.child_task(ctx, session_id, title, "", None));
            }
            Some(ChildScopeHint::ExternalChildUuid { child_uuid }) => {
                if main
                    .as_deref()
                    .is_some_and(|main| !session.is_empty() && main != session)
                {
                    return Scope::Task(self.child_task(ctx, session, "", "", None));
                }
                if let Some(task_id) = self.state().children.get(child_uuid).cloned() {
                    return Scope::Task(task_id);
                }
                if let Some(native_id) = main.as_deref()
                    && let Some(task_id) =
                        self.dialect
                            .resolve_external_child(native_id, child_uuid, ctx, &self.root)
                {
                    self.state()
                        .children
                        .insert(child_uuid.to_string(), task_id.clone());
                    return Scope::Task(task_id);
                }
                return Scope::Ignore;
            }
            None => {}
        }
        if main
            .as_deref()
            .is_some_and(|main| !session.is_empty() && main != session)
        {
            return Scope::Task(self.child_task(ctx, session, "", "", None));
        }
        Scope::Main
    }

    /// The task for a child session, created on first sight.
    fn child_task(
        &self,
        ctx: &ChatContext,
        child: &str,
        label: &str,
        detail: &str,
        kind: Option<&str>,
    ) -> String {
        let task_id = {
            let mut state = self.state();
            state
                .children
                .entry(child.to_string())
                .or_insert_with(|| format!("{TASK_PREFIX}{child}"))
                .clone()
        };
        self.state().active_children.insert(task_id.clone());
        let known = ctx.subagent(&task_id);
        let needs_write = match &known {
            None => true,
            Some(task) => {
                (!label.is_empty() && task.label != label)
                    || (!detail.is_empty() && task.detail != detail)
                    || (kind.is_some() && task.subagent_type.as_deref() != kind)
                    || task.state != SubagentState::Active
            }
        };
        if needs_write {
            let now = crate::util::now_millis();
            let _ = ctx.ensure_subagent(SubagentTask {
                id: task_id.clone(),
                label: if label.is_empty() {
                    known
                        .as_ref()
                        .map_or_else(|| "Subagent".into(), |task| task.label.clone())
                } else {
                    label.to_string()
                },
                subagent_type: kind.map(str::to_string),
                detail: detail.to_string(),
                state: SubagentState::Active,
                origin: "acp".into(),
                started_at: now,
                updated_at: now,
                items: vec![],
            });
        }
        task_id
    }

    /// The parent is working again. OpenCode reports no end for a background
    /// subagent: its child streams a final message and goes quiet, and the
    /// parent continues from what it was told. So a child whose latest item
    /// is a message, with nothing of its own still running, is done. If it
    /// speaks again, `child_task` makes it active again.
    fn settle_idle_children(&self, ctx: &ChatContext) {
        let candidates: Vec<String> = self.state().active_children.iter().cloned().collect();
        for task_id in candidates {
            let Some(task) = ctx.subagent(&task_id) else {
                continue;
            };
            if task.state != SubagentState::Active {
                self.state().active_children.remove(&task_id);
                continue;
            }
            let Some(last) = task.items.last() else {
                continue;
            };
            let busy = task.items.iter().any(|item| {
                item.kind != ChatItemKind::AssistantMessage
                    && item.kind != ChatItemKind::Reasoning
                    && matches!(
                        item.status,
                        ChatItemStatus::Running | ChatItemStatus::Pending | ChatItemStatus::Waiting
                    )
            });
            if last.kind != ChatItemKind::AssistantMessage || busy {
                continue;
            }
            // Its message is final as far as anyone can tell; it keeps its
            // segment, so a late chunk still extends the same message.
            let segment = self
                .state()
                .segments
                .get(&Scope::Task(task_id.clone()))
                .map(|segment| segment.item.clone());
            if let Some(mut item) = segment {
                item.status = ChatItemStatus::Completed;
                self.write(ctx, &Scope::Task(task_id.clone()), item);
            }
            let _ = ctx.finish_subagent(&task_id, SubagentState::Completed, None);
            self.state().active_children.remove(&task_id);
        }
    }

    /// Children whose work all ended are done once the parent's turn is.
    fn settle_children(&self, ctx: &ChatContext) {
        let tasks: Vec<String> = self.state().children.values().cloned().collect();
        for task_id in tasks {
            let Some(task) = ctx.subagent(&task_id) else {
                continue;
            };
            let busy = task.items.iter().any(|item| {
                matches!(
                    item.status,
                    ChatItemStatus::Running | ChatItemStatus::Pending | ChatItemStatus::Waiting
                )
            });
            if task.state == SubagentState::Active && !busy && !task.items.is_empty() {
                let _ = ctx.finish_subagent(&task_id, SubagentState::Completed, None);
            }
        }
    }

    pub fn update(&self, ctx: &ChatContext, params: &Value) {
        let update = &params["update"];
        let kind = update["sessionUpdate"].as_str().unwrap_or_default();
        if self.state().replaying
            && !matches!(
                kind,
                "available_commands_update"
                    | "config_option_update"
                    | "current_mode_update"
                    | "session_info_update"
                    | "usage_update"
            )
        {
            return;
        }
        match kind {
            "user_message_chunk" => {}
            "agent_message_chunk" => {
                self.chunk(ctx, params, update, ChatItemKind::AssistantMessage)
            }
            "agent_thought_chunk" => self.chunk(ctx, params, update, ChatItemKind::Reasoning),
            "tool_call" | "tool_call_update" => self.tool_update(ctx, params, update),
            "plan" => self.plan(ctx, update["entries"].as_array()),
            "plan_update" => {
                let plan = &update["plan"];
                match plan["type"].as_str() {
                    Some("items") => {
                        self.plan(ctx, plan["entries"].as_array().or(plan["items"].as_array()))
                    }
                    Some("markdown") => {
                        let text = plan["markdown"]
                            .as_str()
                            .or(plan["content"].as_str())
                            .unwrap_or_default();
                        let entries: Vec<Value> = text.lines().filter_map(markdown_task).collect();
                        self.plan(ctx, Some(&entries));
                    }
                    _ => {}
                }
            }
            "plan_removed" => self.plan(ctx, None),
            "available_commands_update" => {
                let commands = update["availableCommands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|command| {
                        Some(ChatCommand {
                            name: command["name"]
                                .as_str()?
                                .trim_start_matches('/')
                                .to_string(),
                            description: command["description"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                            hint: command
                                .pointer("/input/hint")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        })
                    })
                    .collect();
                self.edit_controls(ctx, |controls| controls.commands = commands);
            }
            "current_mode_update" => {
                let mode = update["currentModeId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                self.edit_controls(ctx, |controls| {
                    for option in &mut controls.options {
                        if option.category.as_deref() == Some("mode") || option.id == LEGACY_MODE {
                            option.current = mode.clone();
                        }
                    }
                });
            }
            "config_option_update" => {
                let options = config_options(&update["configOptions"]);
                self.edit_controls(ctx, |controls| merge_options(controls, options));
            }
            "session_info_update" => {
                if let Some(settlement) = self.dialect.child_settlement(update) {
                    let task_id =
                        self.child_task(ctx, &settlement.session_id, &settlement.title, "", None);
                    if let Some(state) = settlement.state {
                        let _ = ctx.finish_subagent(&task_id, state, None);
                    }
                }
                let actions = self.goal_actions.locked().clone();
                if let Some(goal) = goal_snapshot(update, &actions) {
                    self.edit_controls(ctx, |controls| controls.goal = goal);
                }
                if let Some(title) = update["title"].as_str() {
                    let title = title.trim().to_string();
                    self.edit_controls(ctx, |controls| {
                        controls.title = (!title.is_empty()).then_some(title)
                    });
                }
            }
            "usage_update" => {
                let context = ChatContextUse {
                    used: update["used"].as_u64().unwrap_or(0),
                    size: update["size"].as_u64().unwrap_or(0),
                    cost_micros: update
                        .pointer("/cost/amount")
                        .and_then(Value::as_f64)
                        .map(|amount| (amount.max(0.0) * 1_000_000.0).round() as u64),
                    currency: update
                        .pointer("/cost/currency")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                };
                self.edit_controls(ctx, |controls| controls.context = Some(context));
            }
            "prompt_suggestions_update" => {
                let suggestions = update["suggestions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|suggestion| content_text(&suggestion["prompt"]))
                    .filter(|text| !text.trim().is_empty())
                    .collect();
                self.edit_controls(ctx, |controls| controls.suggestions = suggestions);
            }
            "notice" => {
                let mut text = update["title"].as_str().unwrap_or_default().to_string();
                if let Some(description) = update["description"]
                    .as_str()
                    .filter(|text| !text.is_empty())
                {
                    text = format!("{text}\n\n{description}");
                }
                let id = {
                    let mut state = self.state();
                    self.next_id(&mut state, "acp-notice:")
                };
                let mut item = if update["severity"] == "error" {
                    super::super::context::error_item(&id, &text)
                } else {
                    system_item(&id, &text, ChatItemStatus::Completed)
                };
                if update["severity"] == "warning" {
                    item.title = "Warning".into();
                }
                let scope = self.scope_of(ctx, params, update);
                self.write(ctx, &scope, item);
            }
            "compaction_update" => {
                let id = format!(
                    "acp-compaction:{}",
                    update["compactionId"].as_str().unwrap_or("current")
                );
                let (status, text) = match update["status"].as_str().unwrap_or_default() {
                    "in_progress" => (
                        ChatItemStatus::Running,
                        "Compacting the conversation…".to_string(),
                    ),
                    "completed" => (
                        ChatItemStatus::Completed,
                        "Conversation compacted".to_string(),
                    ),
                    "cancelled" => (
                        ChatItemStatus::Cancelled,
                        "Compaction cancelled".to_string(),
                    ),
                    _ => (
                        ChatItemStatus::Failed,
                        format!(
                            "Compaction failed{}",
                            update["error"]
                                .as_str()
                                .map(|error| format!(": {error}"))
                                .unwrap_or_default()
                        ),
                    ),
                };
                let mut item = system_item(&id, &text, status);
                item.title = "Context".into();
                if let Some(summary) = update["summary"].as_array() {
                    item.detail = content_text(&Value::Array(summary.clone()));
                }
                let _ = ctx.upsert(item);
            }
            "compaction_summary_chunk" => {
                let id = format!(
                    "acp-compaction:{}",
                    update["compactionId"].as_str().unwrap_or("current")
                );
                let chunk = content_text(&update["content"]);
                let _ = ctx.update_item(&id, |item| item.detail.push_str(&chunk));
            }
            "subagent_update" => self.subagent_update(ctx, update),
            "async_task_spawned" | "async_task_progress" | "async_task_state_update" => {
                self.async_task(ctx, update)
            }
            "session_message" | "session_message_chunk" => self.session_message(ctx, update),
            // Editor cursor positions mean nothing outside an editor.
            "cursor_update" => {}
            _ => {}
        }
    }

    fn chunk(&self, ctx: &ChatContext, params: &Value, update: &Value, kind: ChatItemKind) {
        let text = content_text(&update["content"]);
        if text.is_empty() {
            return;
        }
        let scope = self.scope_of(ctx, params, update);
        if scope == Scope::Main {
            self.settle_idle_children(ctx);
        }
        let item = {
            let mut state = self.state();
            let now = Instant::now();
            state.first_token.get_or_insert(now);
            state.last_token = Some(now);
            let reuse = state
                .segments
                .get(&scope)
                .is_some_and(|segment| segment.item.kind == kind);
            let segment = if reuse {
                let Some(segment) = state.segments.get_mut(&scope) else {
                    return;
                };
                segment
            } else {
                if let Some(mut done) = state.segments.remove(&scope) {
                    done.item.status = ChatItemStatus::Completed;
                    drop(state);
                    self.write(ctx, &scope, done.item);
                    state = self.state();
                }
                let prefix = if kind == ChatItemKind::Reasoning {
                    "acp-thought:"
                } else {
                    "acp-message:"
                };
                let id = self.next_id(&mut state, prefix);
                let mut item = blank_item(&id, kind, ChatItemStatus::Running);
                if kind == ChatItemKind::Reasoning {
                    item.title = "Thinking".into();
                }
                state
                    .segments
                    .entry(scope.clone())
                    .insert_entry(Segment { item })
                    .into_mut()
            };
            segment.item.text.push_str(&text);
            segment.item.clone()
        };
        self.write(ctx, &scope, item);
    }

    fn plan(&self, ctx: &ChatContext, entries: Option<&Vec<Value>>) {
        let plan = entries
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                Some(ChatPlanEntry {
                    content: entry["content"]
                        .as_str()
                        .or(entry["title"].as_str())?
                        .to_string(),
                    priority: entry["priority"].as_str().unwrap_or("medium").to_string(),
                    status: entry["status"].as_str().unwrap_or("pending").to_string(),
                })
            })
            .collect();
        self.edit_controls(ctx, |controls| controls.plan = plan);
    }

    fn subagent_update(&self, ctx: &ChatContext, update: &Value) {
        let Some(child) = update["sessionId"]
            .as_str()
            .or_else(|| update["subagentId"].as_str())
            .or_else(|| update["subagent_id"].as_str())
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let label = update["title"]
            .as_str()
            .or_else(|| update["label"].as_str())
            .or_else(|| update["role"].as_str())
            .unwrap_or_default();
        let detail = update["description"].as_str().unwrap_or_default();
        let kind = update["subagentType"]
            .as_str()
            .or_else(|| update["subagent_type"].as_str())
            .or_else(|| update["typeName"].as_str());
        let task_id = self
            .state()
            .subagent_tools
            .get(child)
            .cloned()
            .unwrap_or_else(|| self.child_task(ctx, child, label, detail, kind));
        let raw_state = update
            .pointer("/state/state")
            .and_then(Value::as_str)
            .or_else(|| update["state"].as_str())
            .or_else(|| update["status"].as_str());
        let state = match raw_state {
            Some("idle" | "completed" | "done" | "success") => Some(SubagentState::Completed),
            Some("failed" | "error") => Some(SubagentState::Failed),
            Some("stopped" | "cancelled" | "canceled" | "killed") => Some(SubagentState::Stopped),
            Some("running" | "requires_action" | "pending" | "working") => None,
            _ => None,
        };
        if let Some(state) = state {
            let _ = ctx.finish_subagent(&task_id, state, None);
        }
    }

    /// Merge the AIR async-task channel. A spawn carries the identity; the
    /// progress and state frames only revise fields they actually include, so
    /// a late partial frame cannot blank a task's name or output path.
    fn async_task(&self, ctx: &ChatContext, update: &Value) {
        let Some(id) = update["asyncTaskId"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        let spawned = update["sessionUpdate"].as_str() == Some("async_task_spawned");
        let Some(mut task) = ctx.task(id) else {
            if !spawned {
                return;
            }
            let now = crate::util::now_millis();
            let mut task = ChatAsyncTask {
                id: id.to_string(),
                name: update["name"].as_str().unwrap_or(id).to_string(),
                task_type: update["taskType"].as_str().unwrap_or("task").to_string(),
                description: update["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                state: "running".into(),
                can_stop: update["canStop"].as_bool().unwrap_or(false),
                summary: None,
                last_tool_name: None,
                total_tokens: None,
                tool_uses: None,
                duration_ms: None,
                output_file_path: None,
                tool_call_id: None,
                item_id: None,
                started_at: now,
                updated_at: now,
            };
            merge_async_task(&mut task, update);
            ctx.upsert_task(task);
            return;
        };
        merge_async_task(&mut task, update);
        task.updated_at = crate::util::now_millis();
        ctx.upsert_task(task);
    }

    fn session_message(&self, ctx: &ChatContext, update: &Value) {
        let main = self.session_id.locked().clone();
        let sender = update["senderSessionId"].as_str();
        let Some(child) = sender.filter(|sender| Some(*sender) != main.as_deref()) else {
            return;
        };
        let task = self.child_task(ctx, child, "", "", None);
        let id = format!(
            "acp-session-message:{}",
            update["messageId"].as_str().unwrap_or_default()
        );
        let text = content_text(&update["content"]);
        if text.is_empty() {
            return;
        }
        let scope = Scope::Task(task);
        let mut item = blank_item(
            &id,
            ChatItemKind::AssistantMessage,
            ChatItemStatus::Completed,
        );
        item.text = text;
        if update["sessionUpdate"] == "session_message_chunk" {
            // Chunks extend the message with the same id.
            if let Some(task) = match &scope {
                Scope::Task(task) => ctx.subagent(task),
                Scope::Main | Scope::Ignore => None,
            } && let Some(existing) = task.items.iter().find(|existing| existing.id == id)
            {
                item.text = format!("{}{}", existing.text, item.text);
            }
        }
        self.write(ctx, &scope, item);
    }

    fn tool_update(&self, ctx: &ChatContext, params: &Value, update: &Value) {
        let Some(id) = update["toolCallId"].as_str() else {
            return;
        };
        let scope = self.scope_of(ctx, params, update);
        if scope == Scope::Ignore {
            return;
        }
        if scope == Scope::Main {
            self.settle_idle_children(ctx);
        }
        let (tool, closed) = {
            let mut state = self.state();
            // A tool call ends the text segment before it in the same scope.
            let closed = state.segments.remove(&scope);
            let tool = state.tools.entry(id.to_string()).or_insert_with(|| Tool {
                id: id.to_string(),
                status: "pending".into(),
                ..Tool::default()
            });
            tool.scope.get_or_insert(scope.clone());
            merge_tool(tool, update);
            (tool.clone(), closed)
        };
        if let Some(mut closed) = closed {
            closed.item.status = ChatItemStatus::Completed;
            self.write(ctx, &scope, closed.item);
        }
        // A subagent tool names the child session it started.
        if let Some(child) = child_session(&tool) {
            let label = subagent_label(&tool);
            let detail = subagent_detail(&tool);
            let kind = subagent_kind(&tool);
            let task_id = self.child_task(ctx, child, label, detail, kind);
            self.state()
                .subagent_tools
                .insert(tool.id.clone(), task_id);
        }
        if let Some(steps) =
            self.dialect
                .extract_tool_plan(tool.name.as_deref(), &tool.title, &tool.raw_input)
        {
            self.plan(ctx, Some(&steps));
        }
        self.render(ctx, &tool);
    }

    /// Re-render every tool card that embeds this terminal.
    pub fn terminal_changed(&self, ctx: &ChatContext, terminal: &str) {
        let tools: Vec<Tool> = self
            .state()
            .tools
            .values()
            .filter(|tool| tool.terminals.iter().any(|id| id == terminal))
            .cloned()
            .collect();
        for tool in tools {
            self.render(ctx, &tool);
        }
    }

    /// Merge a permission request's tool call (which may be the first we
    /// hear of it) and mark it waiting.
    pub fn tool_waiting(
        &self,
        ctx: &ChatContext,
        params: &Value,
        tool_call: &Value,
        waiting: bool,
    ) -> Tool {
        let Some(id) = tool_call["toolCallId"].as_str() else {
            return Tool::default();
        };
        let scope = self.scope_of(ctx, params, tool_call);
        let tool = {
            let mut state = self.state();
            let tool = state.tools.entry(id.to_string()).or_insert_with(|| Tool {
                id: id.to_string(),
                status: "pending".into(),
                ..Tool::default()
            });
            tool.scope.get_or_insert(scope);
            merge_tool(tool, tool_call);
            tool.waiting = waiting;
            tool.clone()
        };
        self.render(ctx, &tool);
        tool
    }

    fn is_subagent_launch(&self, tool: &Tool) -> bool {
        self.dialect.is_subagent_launch(
            tool.kind.as_deref(),
            tool.name.as_deref(),
            &tool.title,
            &tool.meta,
            &tool.raw_input,
        )
    }

    pub fn render(&self, ctx: &ChatContext, tool: &Tool) {
        let scope = tool.scope.clone().unwrap_or(Scope::Main);
        let mut resolved_task_id: Option<String> = None;
        if scope == Scope::Main {
            if let Some(child) = child_session(tool) {
                let label = subagent_label(tool);
                let detail = subagent_detail(tool);
                let kind = subagent_kind(tool);
                let task_id = self.child_task(ctx, child, label, detail, kind);
                self.state()
                    .subagent_tools
                    .insert(tool.id.clone(), task_id.clone());
                resolved_task_id = Some(task_id);
            } else if self.is_subagent_launch(tool) {
                let label = subagent_label(tool);
                let detail = subagent_detail(tool);
                let kind = subagent_kind(tool);
                let task_id =
                    self.child_task(ctx, &format!("tool:{}", tool.id), label, detail, kind);
                self.state()
                    .subagent_tools
                    .insert(tool.id.clone(), task_id.clone());
                let async_launch = self.dialect.is_async_subagent_launch(
                    tool.name.as_deref(),
                    &tool.title,
                    &tool.raw_input,
                );
                if !tool.waiting && matches!(tool.status.as_str(), "completed" | "failed") {
                    let output = if !tool.text.is_empty() {
                        tool.text.join("\n")
                    } else {
                        raw_output_text(&tool.raw_output)
                    };
                    if !output.trim().is_empty() {
                        let has_reply = ctx.subagent(&task_id).is_some_and(|task| {
                            task.items
                                .iter()
                                .any(|item| item.kind == ChatItemKind::AssistantMessage)
                        });
                        if !has_reply {
                            let mut reply = blank_item(
                                &format!("acp-subagent-reply:{}", tool.id),
                                ChatItemKind::AssistantMessage,
                                ChatItemStatus::Completed,
                            );
                            reply.text = output.clone();
                            self.write(ctx, &Scope::Task(task_id.clone()), reply);
                        }
                    }
                    // Antigravity's `invoke_subagent` returns immediately after launching
                    // the background child; leave it Active unless it failed or carried output.
                    if tool.status == "failed" || !async_launch || !output.trim().is_empty() {
                        let final_state = if tool.status == "failed" {
                            SubagentState::Failed
                        } else {
                            SubagentState::Completed
                        };
                        let _ = ctx.finish_subagent(&task_id, final_state, None);
                        self.state().active_children.remove(&task_id);
                    }
                }
                resolved_task_id = Some(task_id);
            }
        }
        let mut item = self.tool_item(tool);
        if item.task_id.is_none() {
            item.task_id = resolved_task_id;
        }
        self.write(ctx, &scope, item);
    }

    pub fn tool_item(&self, tool: &Tool) -> ChatItem {
        let status = if tool.waiting {
            ChatItemStatus::Waiting
        } else {
            match tool.status.as_str() {
                "completed" => ChatItemStatus::Completed,
                "failed" => ChatItemStatus::Failed,
                _ => ChatItemStatus::Running,
            }
        };
        let id = format!("{TOOL_PREFIX}{}", tool.id);
        let mut output = tool.text.join("\n");
        let mut exit = None;
        if let Some(terminals) = self.terminals.locked().as_ref() {
            for terminal in &tool.terminals {
                if let Some((text, code)) = terminals.text(terminal) {
                    // The agent reads the raw output; the card shows it cleaned.
                    let text = crate::util::clean_terminal_text(&text);
                    if !output.is_empty() && !text.is_empty() {
                        output.push('\n');
                    }
                    output.push_str(&text);
                    exit = exit.or(code);
                }
            }
        }
        if output.is_empty() {
            output = raw_output_text(&tool.raw_output);
        }
        let kind = tool.kind.as_deref().unwrap_or("other");
        let input = if tool
            .raw_input
            .as_object()
            .is_some_and(|object| !object.is_empty())
        {
            tool.raw_input.to_string()
        } else {
            String::new()
        };
        let mut item = if kind == "execute" {
            let command = command_text(&tool.raw_input).unwrap_or_else(|| tool.title.clone());
            let mut item = blank_item(&id, ChatItemKind::CommandExecution, status);
            item.title = "Command".into();
            item.text = command.clone();
            item.subject = command;
            item
        } else if !tool.diffs.is_empty() {
            let mut item = blank_item(&id, ChatItemKind::FileChange, status);
            item.title = tool.name.clone().unwrap_or_else(|| "Edit".into());
            item.text = input.clone();
            item.changes = tool
                .diffs
                .iter()
                .flat_map(|(path, old, new)| {
                    let patch = unified_patch(&self.root, path, old.as_deref(), new);
                    file_changes(&self.root, path, old.as_deref(), Some(new), Some(&patch))
                })
                .collect();
            for change in &mut item.changes {
                if let Some(patch) = &change.patch {
                    let (added, removed) = patch_counts(patch);
                    change.additions = Some(added);
                    change.deletions = Some(removed);
                }
            }
            item
        } else {
            let mut item = blank_item(&id, ChatItemKind::Tool, status);
            item.title = tool
                .name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| tool.title.clone());
            item.text = if input.is_empty() && !tool.locations.is_empty() {
                json!({"path": tool.locations[0]}).to_string()
            } else {
                input
            };
            item.action = kind_action(kind);
            item
        };
        // The agent's own one-line title is the fallback subject.
        if item.subject.is_empty() && tool.title != item.title {
            item.subject = tool.title.clone();
        }
        item.task_id = if let Some(child) = child_session(tool) {
            Some(format!("{TASK_PREFIX}{child}"))
        } else if self.is_subagent_launch(tool) {
            Some(format!("{TASK_PREFIX}tool:{}", tool.id))
        } else {
            None
        };
        if let Some(code) = exit.filter(|code| *code != 0) {
            output.push_str(&format!("\n[exit code {code}]"));
        }
        item.detail = output;
        item
    }
}

const LEGACY_MODE: &str = "__mode";
const LEGACY_MODEL: &str = "__model";

fn subagent_label(tool: &Tool) -> &str {
    [
        "Role",
        "description",
        "title",
        "subagent_type",
        "TypeName",
        "agent",
        "Prompt",
        "prompt",
    ]
    .iter()
    .find_map(|key| tool.raw_input.get(*key).and_then(Value::as_str))
    .map(str::trim)
    .filter(|text| !text.is_empty())
    .unwrap_or(&tool.title)
}

fn subagent_detail(tool: &Tool) -> &str {
    ["Prompt", "prompt", "description"]
        .iter()
        .find_map(|key| tool.raw_input.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .unwrap_or("")
}

fn subagent_kind(tool: &Tool) -> Option<&str> {
    ["subagent_type", "TypeName", "agent"]
        .iter()
        .find_map(|key| tool.raw_input.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

fn child_session(tool: &Tool) -> Option<&str> {
    tool.raw_output
        .pointer("/metadata/sessionID")
        .or_else(|| tool.raw_output.pointer("/sessionId"))
        .or_else(|| tool.raw_output.pointer("/conversationId"))
        .or_else(|| tool.raw_output.pointer("/subagent_id"))
        .and_then(Value::as_str)
        .filter(|child| !child.is_empty())
}

fn merge_async_task(task: &mut ChatAsyncTask, update: &Value) {
    let text = |key: &str| update[key].as_str().filter(|value| !value.is_empty());
    if let Some(name) = text("name") {
        task.name = name.to_string();
    }
    if let Some(kind) = text("taskType") {
        task.task_type = kind.to_string();
    }
    if let Some(description) = text("description") {
        task.description = description.to_string();
    }
    if let Some(state) = text("state") {
        task.state = state.to_string();
    }
    if let Some(can_stop) = update["canStop"].as_bool() {
        task.can_stop = can_stop;
    }
    if let Some(summary) = text("summary") {
        task.summary = Some(summary.to_string());
    }
    if let Some(tool) = text("lastToolName") {
        task.last_tool_name = Some(tool.to_string());
    }
    if let Some(path) = text("outputFilePath") {
        task.output_file_path = Some(path.to_string());
    }
    if let Some(call) = text("toolCallId") {
        task.tool_call_id = Some(call.to_string());
        task.item_id = Some(format!("{TOOL_PREFIX}{call}"));
    }
    if let Some(usage) = update["usage"].as_object() {
        if let Some(tokens) = usage.get("totalTokens").and_then(Value::as_u64) {
            task.total_tokens = Some(tokens);
        }
        if let Some(tools) = usage.get("toolUses").and_then(Value::as_u64) {
            task.tool_uses = Some(tools);
        }
        if let Some(duration) = usage.get("durationMs").and_then(Value::as_u64) {
            task.duration_ms = Some(duration);
        }
    }
}

fn merge_tool(tool: &mut Tool, update: &Value) {
    if let Some(name) = update["name"].as_str().filter(|name| !name.is_empty()) {
        tool.name = Some(name.to_string());
    }
    if let Some(title) = update["title"].as_str().filter(|title| !title.is_empty()) {
        tool.title = title.to_string();
    }
    if let Some(kind) = update["kind"].as_str() {
        tool.kind = Some(kind.to_string());
    }
    if let Some(status) = update["status"].as_str() {
        tool.status = status.to_string();
    }
    if update["rawInput"]
        .as_object()
        .is_some_and(|input| !input.is_empty())
    {
        tool.raw_input = update["rawInput"].clone();
    }
    if !update["rawOutput"].is_null() {
        tool.raw_output = update["rawOutput"].clone();
    }
    if let Some(meta) = update["_meta"].as_object()
        && !meta.is_empty()
    {
        tool.meta = update["_meta"].clone();
    }
    if let Some(locations) = update["locations"].as_array()
        && !locations.is_empty()
    {
        tool.locations = locations
            .iter()
            .filter_map(|location| location["path"].as_str().map(str::to_string))
            .collect();
    }
    // `content` replaces the call's content whole when present.
    if let Some(content) = update["content"].as_array() {
        tool.text.clear();
        tool.diffs.clear();
        let mut terminals = Vec::new();
        for block in content {
            match block["type"].as_str() {
                Some("diff") => {
                    if let (Some(path), Some(new)) =
                        (block["path"].as_str(), block["newText"].as_str())
                    {
                        tool.diffs.push((
                            path.to_string(),
                            block["oldText"].as_str().map(str::to_string),
                            new.to_string(),
                        ));
                    }
                }
                Some("terminal") => {
                    if let Some(id) = block["terminalId"].as_str() {
                        terminals.push(id.to_string());
                    }
                }
                _ => {
                    let text = content_text(&block["content"]);
                    if !text.is_empty() {
                        tool.text.push(text);
                    }
                }
            }
        }
        if !terminals.is_empty() {
            tool.terminals = terminals;
        }
    }
}

fn kind_action(kind: &str) -> Option<ToolAction> {
    match kind {
        "read" => Some(ToolAction::Read),
        "edit" | "delete" | "move" => Some(ToolAction::Edit),
        "search" => Some(ToolAction::CodeSearch),
        "fetch" => Some(ToolAction::WebSearch),
        "switch_mode" => Some(ToolAction::Plan),
        _ => None,
    }
}

fn command_text(input: &Value) -> Option<String> {
    match &input["command"] {
        Value::String(command) => Some(command.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => ["CommandLine", "command_line", "commandLine", "cmd"]
            .iter()
            .find_map(|key| input[*key].as_str())
            .map(str::to_string),
    }
    .map(|command| crate::chat_view::unwrap_shell(&command))
}

fn raw_output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Object(object) => [
            "combinedOutput",
            "combined_output",
            "output",
            "stdout",
            "result",
            "content",
            "text",
        ]
        .iter()
        .find_map(|key| object.get(*key))
        .map(|value| match value {
            Value::String(text) => text.clone(),
            other => content_text(other),
        })
        .unwrap_or_default(),
        _ => String::new(),
    }
}

/// Plain text of one content block or an array of them, as Markdown.
pub fn content_text(content: &Value) -> String {
    match content {
        Value::Array(blocks) => blocks.iter().map(content_text).collect::<Vec<_>>().join(""),
        Value::Object(block) => match block.get("type").and_then(Value::as_str) {
            Some("text") => block
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Some("resource_link") => {
                let name = block
                    .get("title")
                    .or_else(|| block.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("link");
                format!(
                    "[{name}]({})",
                    block.get("uri").and_then(Value::as_str).unwrap_or_default()
                )
            }
            Some("resource") => block
                .get("resource")
                .and_then(|resource| resource.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            Some("image") => "\n[image]\n".into(),
            Some("audio") => "\n[audio]\n".into(),
            Some("content") => block.get("content").map(content_text).unwrap_or_default(),
            _ => String::new(),
        },
        Value::String(text) => text.clone(),
        _ => String::new(),
    }
}

pub fn config_options(value: &Value) -> Vec<ChatConfigOption> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|option| {
            let kind = option["type"].as_str().unwrap_or("select").to_string();
            let current = match &option["currentValue"] {
                Value::String(value) => value.clone(),
                Value::Bool(value) => value.to_string(),
                other => other.to_string(),
            };
            let mut choices = Vec::new();
            for entry in option["options"].as_array().into_iter().flatten() {
                if let Some(group) = entry["options"].as_array() {
                    let name = entry["name"]
                        .as_str()
                        .or(entry["group"].as_str())
                        .map(str::to_string);
                    for choice in group {
                        if let Some(choice) = config_choice(choice, name.clone()) {
                            choices.push(choice);
                        }
                    }
                } else if let Some(choice) = config_choice(entry, None) {
                    choices.push(choice);
                }
            }
            let id = option["id"].as_str()?.to_string();
            let category = option["category"]
                .as_str()
                .map(str::to_string)
                .or_else(|| match id.as_str() {
                    "model" | "mode" | "thought_level" => Some(id.clone()),
                    _ => None,
                });
            Some(ChatConfigOption {
                id,
                name: option["name"].as_str().unwrap_or_default().to_string(),
                description: option["description"].as_str().map(str::to_string),
                category,
                kind,
                current,
                choices,
                hidden: false,
            })
        })
        .collect()
}

fn config_choice(choice: &Value, group: Option<String>) -> Option<ChatConfigChoice> {
    let value = choice["value"].as_str()?.to_string();
    let mut name = choice["name"].as_str().unwrap_or_default().to_string();
    let mut group = group;
    // Ungrouped `provider/model` choices (OpenCode names them
    // "opencode/Big Pickle"): the provider becomes the group and the name is
    // the model's own id after the slash, the same as before a session starts.
    if group.is_none()
        && let Some((provider, model)) = value.split_once('/')
        && !provider.is_empty()
        && !model.is_empty()
        && (name.is_empty() || name.starts_with(&format!("{provider}/")))
    {
        group = Some(provider.to_string());
        name = model.to_string();
    }
    Some(ChatConfigChoice {
        value,
        name,
        description: choice["description"].as_str().map(str::to_string),
        group,
    })
}

/// Legacy `modes` (before config options) shown as one mode selector.
pub fn legacy_modes(modes: &Value) -> Option<ChatConfigOption> {
    let available = modes["availableModes"].as_array()?;
    if available.is_empty() {
        return None;
    }
    Some(ChatConfigOption {
        id: LEGACY_MODE.into(),
        name: "Mode".into(),
        description: None,
        category: Some("mode".into()),
        kind: "select".into(),
        current: modes["currentModeId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        choices: available
            .iter()
            .filter_map(|mode| {
                Some(ChatConfigChoice {
                    value: mode["id"].as_str()?.to_string(),
                    name: mode["name"].as_str().unwrap_or_default().to_string(),
                    description: mode["description"].as_str().map(str::to_string),
                    group: None,
                })
            })
            .collect(),
        hidden: false,
    })
}

pub fn is_legacy_mode(option: &str) -> bool {
    option == LEGACY_MODE
}

/// Legacy `models` (before config options) shown as one model selector.
pub fn legacy_models(models: &Value) -> Option<ChatConfigOption> {
    let available = models["availableModels"].as_array()?;
    if available.is_empty() {
        return None;
    }
    let choices: Vec<ChatConfigChoice> = available
        .iter()
        .filter_map(|entry| {
            let value = entry["modelId"]
                .as_str()
                .or_else(|| entry["id"].as_str())?
                .to_string();
            let name = entry["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .unwrap_or(&value)
                .to_string();
            Some(ChatConfigChoice {
                value,
                name,
                description: entry["description"].as_str().map(str::to_string),
                group: None,
            })
        })
        .collect();
    if choices.is_empty() {
        return None;
    }
    Some(ChatConfigOption {
        id: LEGACY_MODEL.into(),
        name: "Model".into(),
        description: None,
        category: Some("model".into()),
        kind: "select".into(),
        current: models["currentModelId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        choices,
        hidden: false,
    })
}

pub fn is_legacy_model(option: &str) -> bool {
    option == LEGACY_MODEL
}

/// Config options replace the whole set; legacy mode/model selectors stay
/// unless the agent now sends its own config option for that category.
pub fn merge_options(controls: &mut ChatControls, options: Vec<ChatConfigOption>) {
    let keep = |legacy_id: &str, category: &str| {
        controls
            .options
            .iter()
            .find(|option| option.id == legacy_id)
            .cloned()
            .filter(|_| {
                !options
                    .iter()
                    .any(|option| option.category.as_deref() == Some(category))
            })
    };
    let legacy_model = keep(LEGACY_MODEL, "model");
    let legacy_mode = keep(LEGACY_MODE, "mode");
    controls.options = options;
    if let Some(legacy) = legacy_model {
        controls.options.push(legacy);
    }
    if let Some(legacy) = legacy_mode {
        controls.options.push(legacy);
    }
}

fn markdown_task(line: &str) -> Option<Value> {
    let line = line.trim_start();
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    let (status, content) = if let Some(rest) = rest
        .strip_prefix("[x] ")
        .or_else(|| rest.strip_prefix("[X] "))
    {
        ("completed", rest)
    } else if let Some(rest) = rest.strip_prefix("[ ] ") {
        ("pending", rest)
    } else if let Some(rest) = rest
        .strip_prefix("[~] ")
        .or_else(|| rest.strip_prefix("[-] "))
    {
        ("in_progress", rest)
    } else {
        ("pending", rest)
    };
    Some(json!({"content": content.trim(), "status": status, "priority": "medium"}))
}

/// A unified diff between two texts, with three lines of context.
pub fn unified_patch(root: &Path, path: &str, old: Option<&str>, new: &str) -> String {
    let display = crate::agent_runtime::relativize(path, root).unwrap_or_else(|| path.to_string());
    let old_lines: Vec<&str> = old.unwrap_or_default().lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let ops = line_diff(&old_lines, &new_lines);
    let mut out = String::new();
    out.push_str(&format!(
        "--- {}\n+++ b/{display}\n",
        if old.is_none() {
            "/dev/null".to_string()
        } else {
            format!("a/{display}")
        }
    ));
    const CONTEXT: usize = 3;
    // Group the ops into hunks around changes.
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Same(..)))
        .map(|(index, _)| index)
        .collect();
    if changed.is_empty() {
        return out;
    }
    let mut hunks: Vec<(usize, usize)> = Vec::new();
    for index in changed {
        let start = index.saturating_sub(CONTEXT);
        let end = (index + CONTEXT + 1).min(ops.len());
        match hunks.last_mut() {
            Some(last) if start <= last.1 => last.1 = end,
            _ => hunks.push((start, end)),
        }
    }
    for (start, end) in hunks {
        let (mut old_start, mut new_start) = (1, 1);
        for op in &ops[..start] {
            match op {
                Op::Same(..) => {
                    old_start += 1;
                    new_start += 1;
                }
                Op::Del(_) => old_start += 1,
                Op::Add(_) => new_start += 1,
            }
        }
        let old_count = ops[start..end]
            .iter()
            .filter(|op| !matches!(op, Op::Add(_)))
            .count();
        let new_count = ops[start..end]
            .iter()
            .filter(|op| !matches!(op, Op::Del(_)))
            .count();
        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            if old_count == 0 {
                old_start - 1
            } else {
                old_start
            },
            old_count,
            if new_count == 0 {
                new_start - 1
            } else {
                new_start
            },
            new_count
        ));
        for op in &ops[start..end] {
            match op {
                Op::Same(line, _) => out.push_str(&format!(" {line}\n")),
                Op::Del(line) => out.push_str(&format!("-{line}\n")),
                Op::Add(line) => out.push_str(&format!("+{line}\n")),
            }
        }
    }
    out
}

enum Op<'a> {
    Same(&'a str, ()),
    Del(&'a str),
    Add(&'a str),
}

/// Line diff by longest common subsequence; large inputs fall back to
/// delete-all/add-all so a huge file never costs quadratic memory.
fn line_diff<'a>(old: &[&'a str], new: &[&'a str]) -> Vec<Op<'a>> {
    // Trim the common prefix and suffix first; most edits are small.
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (a, b) = (
        &old[prefix..old.len() - suffix],
        &new[prefix..new.len() - suffix],
    );
    let mut ops: Vec<Op<'a>> = old[..prefix]
        .iter()
        .map(|line| Op::Same(line, ()))
        .collect();
    if a.len().saturating_mul(b.len()) > 4_000_000 {
        ops.extend(a.iter().map(|line| Op::Del(line)));
        ops.extend(b.iter().map(|line| Op::Add(line)));
    } else {
        let mut table = vec![vec![0u32; b.len() + 1]; a.len() + 1];
        for i in (0..a.len()).rev() {
            for j in (0..b.len()).rev() {
                table[i][j] = if a[i] == b[j] {
                    table[i + 1][j + 1] + 1
                } else {
                    table[i + 1][j].max(table[i][j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if a[i] == b[j] {
                ops.push(Op::Same(a[i], ()));
                i += 1;
                j += 1;
            } else if table[i + 1][j] >= table[i][j + 1] {
                ops.push(Op::Del(a[i]));
                i += 1;
            } else {
                ops.push(Op::Add(b[j]));
                j += 1;
            }
        }
        ops.extend(a[i..].iter().map(|line| Op::Del(line)));
        ops.extend(b[j..].iter().map(|line| Op::Add(line)));
    }
    ops.extend(
        old[old.len() - suffix..]
            .iter()
            .map(|line| Op::Same(line, ())),
    );
    ops
}

fn patch_counts(patch: &str) -> (u32, u32) {
    let mut added = 0;
    let mut removed = 0;
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        AgentKind, ChatPermissionMode, Session, SessionMode, SessionStatus, Workspace,
    };
    use crate::store::Store;
    use std::sync::RwLock;

    fn context() -> Arc<ChatContext> {
        let store = Arc::new(Store::in_memory().unwrap());
        store
            .insert_workspace(&Workspace {
                id: "w1".into(),
                name: "workspace".into(),
                path: "/workspace".into(),
                model_overrides: vec![],
                archived: false,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        store
            .insert_session(&Session {
                id: "s1".into(),
                workspace_id: "w1".into(),
                worktree: None,
                name: "chat".into(),
                agent: Some(AgentKind::Opencode),
                mode: SessionMode::Ui,
                archived: false,
                starred: false,
                created_at: 1,
                updated_at: 1,
                last_started_at: None,
                status: SessionStatus::Stopped,
            })
            .unwrap();
        ChatContext::new(
            "s1",
            PathBuf::from("/workspace"),
            store,
            Arc::new(RwLock::new(None)),
            ChatPermissionMode::ApprovalRequired,
        )
    }

    #[test]
    fn provider_prefixed_model_choices_are_grouped_by_provider() {
        let options = config_options(&json!([{
            "id": "model", "name": "Model", "category": "model", "type": "select",
            "currentValue": "opencode/big-pickle",
            "options": [
                { "value": "opencode/big-pickle", "name": "opencode/Big Pickle" },
                { "value": "anthropic/claude-haiku", "name": "" },
                { "value": "plain", "name": "Plain" }
            ]
        }]));
        let choices = &options[0].choices;
        assert_eq!(
            (choices[0].name.as_str(), choices[0].group.as_deref()),
            ("big-pickle", Some("opencode"))
        );
        assert_eq!(
            (choices[1].name.as_str(), choices[1].group.as_deref()),
            ("claude-haiku", Some("anthropic"))
        );
        assert_eq!(
            (choices[2].name.as_str(), choices[2].group.as_deref()),
            ("Plain", None)
        );
        assert_eq!(choices[0].value, "opencode/big-pickle");
    }

    #[test]
    fn async_task_links_the_tool_card_that_started_it() {
        let mut task = ChatAsyncTask {
            id: "t1".into(),
            name: "npm test".into(),
            task_type: "shell".into(),
            description: String::new(),
            state: "running".into(),
            can_stop: true,
            summary: None,
            last_tool_name: None,
            total_tokens: None,
            tool_uses: None,
            duration_ms: None,
            output_file_path: None,
            tool_call_id: None,
            item_id: None,
            started_at: 0,
            updated_at: 0,
        };
        merge_async_task(
            &mut task,
            &json!({ "toolCallId": "call_7", "state": "completed", "summary": "42 passed" }),
        );
        assert_eq!(task.item_id.as_deref(), Some("acp-tool:call_7"));
        assert_eq!(task.state, "completed");
        assert_eq!(task.summary.as_deref(), Some("42 passed"));
        // A later frame without the field keeps the link.
        merge_async_task(&mut task, &json!({ "state": "completed" }));
        assert_eq!(task.item_id.as_deref(), Some("acp-tool:call_7"));
    }

    #[test]
    fn goal_snapshots_read_neutral_air_and_legacy_channels() {
        let actions = vec!["set".into(), "pause".into(), "clear".into()];
        let neutral = json!({
            "sessionUpdate": "session_info_update",
            "_meta": { "goal": {
                "objective": "Ship the release",
                "status": "budgetLimited",
                "tokensUsed": 90,
                "tokenBudget": 100,
                "timeUsedSeconds": 12
            }}
        });
        let goal = goal_snapshot(&neutral, &actions).flatten().unwrap();
        assert_eq!(goal.objective, "Ship the release");
        assert_eq!(goal.status, "budget_limited");
        assert_eq!(goal.tokens_used, Some(90));
        assert_eq!(goal.token_budget, Some(100));
        assert_eq!(goal.time_used_seconds, Some(12));
        assert_eq!(goal.actions, actions);

        let air = json!({
            "sessionUpdate": "session_info_update",
            "_meta": { "jetbrains": { "air": { "goal": {
                "objective": "Review", "status": "active"
            }}}}
        });
        assert_eq!(
            goal_snapshot(&air, &actions).flatten().unwrap().objective,
            "Review"
        );

        let legacy = json!({
            "sessionUpdate": "session_info_update",
            "_meta": { "codex": { "goal": {
                "objective": "Old", "status": "complete"
            }}}
        });
        assert_eq!(
            goal_snapshot(&legacy, &actions).flatten().unwrap().status,
            "complete"
        );

        let cleared = json!({
            "sessionUpdate": "session_info_update",
            "_meta": { "goal": null }
        });
        assert_eq!(goal_snapshot(&cleared, &actions), Some(None));
        assert_eq!(goal_snapshot(&json!({}), &actions), None);
    }

    /// Replay a recorded run: every agent → client frame, in order. Each
    /// prompt response ends a turn; permission requests mark their tool.
    fn replay(name: &str, text: &str) -> (Arc<ChatContext>, View) {
        replay_until(name, text, |_| false)
    }

    /// Replay until a frame matches `stop` (that frame included).
    fn replay_until(
        name: &str,
        text: &str,
        stop: impl Fn(&Value) -> bool,
    ) -> (Arc<ChatContext>, View) {
        let ctx = context();
        let view = View::with_dialect(
            Path::new("/workspace"),
            crate::agent_runtime::acp::dialect::for_agent(crate::model::AgentKind::Opencode),
        );
        let mut turn = 0;
        for line in text.lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            let done = stop(&record["msg"]);
            let message = &record["msg"];
            if record["dir"] == "out" && message["method"] == "session/prompt" {
                view.begin_turn(&ctx);
                turn += 1;
                continue;
            }
            if record["dir"] != "in" {
                continue;
            }
            if let Some(session) = message.pointer("/result/sessionId").and_then(Value::as_str) {
                view.set_session(session);
                let options = config_options(&message["result"]["configOptions"]);
                view.edit_controls(&ctx, |controls| merge_options(controls, options));
            }
            match message["method"].as_str() {
                Some("session/update") => view.update(&ctx, &message["params"]),
                Some("session/request_permission") => {
                    view.tool_waiting(
                        &ctx,
                        &message["params"],
                        &message["params"]["toolCall"],
                        true,
                    );
                    view.tool_waiting(
                        &ctx,
                        &message["params"],
                        &message["params"]["toolCall"],
                        false,
                    );
                }
                _ => {}
            }
            if message["result"]["stopReason"].is_string() {
                view.end_turn(
                    &ctx,
                    &format!("{name}-{turn}"),
                    message["result"].get("usage"),
                );
            }
            if done {
                break;
            }
        }
        (ctx, view)
    }

    fn task_state(ctx: &ChatContext, child_suffix: &str) -> SubagentState {
        ctx.store
            .chat_subagents("s1")
            .unwrap()
            .into_iter()
            .find(|task| task.id.ends_with(child_suffix))
            .unwrap_or_else(|| panic!("no task for {child_suffix}"))
            .state
    }

    #[test]
    fn opencode_background_subagents_finish_when_the_parent_moves_on() {
        let text = include_str!("fixtures/opencode-2.3.2/subagents.jsonl");
        // b.txt's child has answered "2"; a.txt's child is still on its command.
        let (ctx, _view) = replay_until("subagents", text, |message| {
            message
                .pointer("/params/update/content/text")
                .and_then(Value::as_str)
                == Some("I've launched both subagents in parallel —")
                && message.pointer("/params/update/_meta").is_none()
        });
        assert_eq!(task_state(&ctx, "w4Kk08"), SubagentState::Completed);
        assert_eq!(task_state(&ctx, "rlYUeG"), SubagentState::Active);

        let (ctx, _view) = replay("subagents", text);
        let tasks = ctx.store.chat_subagents("s1").unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(
            tasks
                .iter()
                .all(|task| task.state == SubagentState::Completed),
            "{tasks:?}"
        );
        for task in &tasks {
            let messages: Vec<_> = task
                .items
                .iter()
                .filter(|item| item.kind == ChatItemKind::AssistantMessage)
                .collect();
            assert_eq!(messages.len(), 1, "one final message per child");
            assert_eq!(messages[0].status, ChatItemStatus::Completed);
            assert!(
                task.items
                    .iter()
                    .any(|item| item.kind == ChatItemKind::CommandExecution)
            );
        }
    }

    #[test]
    fn opencode_tools_messages_subagent_and_controls() {
        let (ctx, view) = replay("tools", include_str!("fixtures/opencode-2.3.2/tools.jsonl"));
        let items = ctx.store.chat_items("s1").unwrap();
        let messages: Vec<_> = items
            .iter()
            .filter(|item| item.kind == ChatItemKind::AssistantMessage)
            .collect();
        assert_eq!(messages[0].text, "pong");
        assert!(
            messages
                .iter()
                .all(|item| item.status == ChatItemStatus::Completed)
        );
        assert!(
            items
                .iter()
                .any(|item| item.kind == ChatItemKind::Reasoning)
        );

        let read = items
            .iter()
            .find(|item| item.title == "read")
            .expect("the read tool");
        assert_eq!(read.action, Some(ToolAction::Read));
        assert_eq!(read.subject, "notes.txt");
        assert_eq!(read.status, ChatItemStatus::Completed);

        let write = items
            .iter()
            .find(|item| item.title == "write")
            .expect("the write tool");
        assert_eq!(write.action, Some(ToolAction::Edit));
        assert_eq!(write.files, vec!["notes.txt".to_string()]);

        let shell = items
            .iter()
            .find(|item| item.kind == ChatItemKind::CommandExecution)
            .expect("the shell command");
        assert_eq!(shell.text, "ls -la");
        assert!(
            shell.detail.contains("notes.txt"),
            "command output: {}",
            shell.detail
        );

        // The background subagent and its tagged tool call.
        let tasks = ctx.store.chat_subagents("s1").unwrap();
        assert_eq!(tasks.len(), 1, "{tasks:?}");
        assert_eq!(tasks[0].label, "Count files in folder");
        assert_eq!(tasks[0].subagent_type.as_deref(), Some("general"));
        assert!(
            tasks[0]
                .items
                .iter()
                .any(|item| item.kind == ChatItemKind::CommandExecution)
        );
        assert!(
            !items
                .iter()
                .any(|item| item.id.contains("ses_e0e4cd623ffeYyRhAtMe5y9bx2:")),
            "child calls stay out of the parent timeline"
        );

        let controls = view.controls();
        let names: Vec<_> = controls
            .options
            .iter()
            .map(|option| option.id.as_str())
            .collect();
        assert_eq!(names, ["model", "effort", "mode"]);
        let model = &controls.options[0];
        assert_eq!(model.current, "opencode/deepseek-v4.1-flash-free");
        assert!(
            controls
                .commands
                .iter()
                .any(|command| command.name == "compact")
        );
        let context = controls.context.unwrap();
        assert_eq!((context.used, context.size), (9443, 200000));

        let usage = ctx.store.chat_usage("s1").unwrap();
        assert_eq!(usage.len(), 3);
        assert_eq!(usage[0].usage.output, 4);
        assert_eq!(usage[0].usage.input, 6529 + 1920);
        assert_eq!(usage[0].usage.cached, 1920);
        assert_eq!(usage[1].usage.reasoning, 61);
    }

    #[test]
    fn opencode_permission_edit_carries_a_unified_diff() {
        let (ctx, _view) = replay("perm", include_str!("fixtures/opencode-2.3.2/perm.jsonl"));
        let items = ctx.store.chat_items("s1").unwrap();
        let edit = items
            .iter()
            .find(|item| item.kind == ChatItemKind::FileChange)
            .expect("the edit as a file change");
        assert_eq!(edit.status, ChatItemStatus::Completed);
        assert_eq!(edit.files, vec!["notes.txt".to_string()]);
        let change = &edit.changes[0];
        assert_eq!(change.path, "notes.txt");
        let patch = change.patch.as_deref().unwrap();
        assert!(patch.contains("+second"), "{patch}");
        assert!(
            patch.contains(" hello world"),
            "unchanged line kept as context: {patch}"
        );
        assert_eq!((change.additions, change.deletions), (Some(1), Some(0)));
        let shell = items
            .iter()
            .find(|item| item.kind == ChatItemKind::CommandExecution)
            .unwrap();
        assert_eq!(shell.text, "echo hi");
    }

    #[test]
    fn opencode_steered_turn_keeps_its_items_in_order() {
        let (ctx, _view) = replay("steer", include_str!("fixtures/opencode-2.3.2/steer.jsonl"));
        let items = ctx.store.chat_items("s1").unwrap();
        let last = items
            .iter()
            .rfind(|item| item.kind == ChatItemKind::AssistantMessage)
            .unwrap();
        assert_eq!(last.text, "STEERED");
        assert_eq!(
            items
                .iter()
                .filter(|item| item.kind == ChatItemKind::CommandExecution)
                .count(),
            1
        );
    }

    #[test]
    fn unified_patch_has_context_and_hunk_counts() {
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\nb\nc\nD\ne\nf\ng\nh\ni\n";
        let patch = unified_patch(Path::new("/w"), "/w/x.txt", Some(old), new);
        assert!(patch.starts_with("--- a/x.txt\n+++ b/x.txt\n"));
        assert!(patch.contains("@@ -1,8 +1,9 @@"), "{patch}");
        assert!(patch.contains("-d\n+D\n"));
        assert!(patch.ends_with("+i\n"));
        let added = unified_patch(Path::new("/w"), "/w/new.txt", None, "x\n");
        assert!(added.starts_with("--- /dev/null\n"));
        assert!(added.contains("@@ -0,0 +1,1 @@"));
    }

    #[test]
    fn grouped_select_options_flatten_with_their_group() {
        let options = config_options(&json!([{
            "id": "model", "name": "Model", "category": "model", "type": "select",
            "currentValue": "b",
            "options": [{"group": "g1", "name": "Group 1", "options": [{"value": "a", "name": "A"}]},
                        {"group": "g2", "name": "Group 2", "options": [{"value": "b", "name": "B"}]}]
        }, {"id": "fast", "name": "Fast", "type": "boolean", "currentValue": true}]));
        assert_eq!(options[0].choices.len(), 2);
        assert_eq!(options[0].choices[1].group.as_deref(), Some("Group 2"));
        assert_eq!(options[1].kind, "boolean");
        assert_eq!(options[1].current, "true");
    }

    #[test]
    fn antigravity_legacy_models_command_fields_and_update_steps_are_supported() {
        let legacy = legacy_models(&json!({
            "currentModelId": "gemini-4-neon-high",
            "availableModels": [
                {"modelId": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)"},
                {"modelId": "claude-opus-5-5-high", "name": "Claude Opus 5.5 (High)"}
            ]
        }))
        .unwrap();
        assert!(is_legacy_model(&legacy.id));
        assert_eq!(legacy.category.as_deref(), Some("model"));
        assert_eq!(legacy.current, "gemini-4-neon-high");
        assert_eq!(legacy.choices.len(), 2);

        let mut controls = ChatControls::default();
        controls.options.push(legacy.clone());
        merge_options(
            &mut controls,
            config_options(&json!([{
                "id": "thought_level",
                "name": "Thinking Level",
                "type": "select",
                "currentValue": "high",
                "options": [{"value": "low", "name": "Low"}, {"value": "high", "name": "High"}]
            }])),
        );
        assert!(controls.options.iter().any(|o| is_legacy_model(&o.id)));
        assert!(
            controls
                .options
                .iter()
                .any(|o| o.category.as_deref() == Some("thought_level"))
        );

        let ctx = context();
        let view = View::with_dialect(
            Path::new("/workspace"),
            crate::agent_runtime::acp::dialect::for_agent(crate::model::AgentKind::Antigravity),
        );
        view.update(
            &ctx,
            &json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "cmd-1",
                    "kind": "execute",
                    "title": "Run command",
                    "status": "completed",
                    "rawInput": {"CommandLine": "cargo test"},
                    "rawOutput": {"combinedOutput": "ok 42 passed"}
                }
            }),
        );
        let items = ctx.store.chat_items("s1").unwrap();
        let cmd = items
            .iter()
            .find(|item| item.kind == ChatItemKind::CommandExecution)
            .unwrap();
        assert_eq!(cmd.text, "cargo test");
        assert_eq!(cmd.detail, "ok 42 passed");

        view.update(
            &ctx,
            &json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "steps-1",
                    "title": "Run update_steps",
                    "status": "completed",
                    "rawInput": {
                        "steps": [
                            {"title": "Inspect ACP", "status": "done"},
                            {"title": "Run tests", "status": "in_progress"}
                        ]
                    }
                }
            }),
        );
        let plan = view.controls().plan;
        assert_eq!(plan.len(), 2);
        assert_eq!((plan[0].content.as_str(), plan[0].status.as_str()), ("Inspect ACP", "completed"));
        assert_eq!((plan[1].content.as_str(), plan[1].status.as_str()), ("Run tests", "in_progress"));
    }

    #[test]
    fn antigravity_invoke_subagent_and_claude_agent_tools_create_and_finish_subagents() {
        let ctx = context();
        let view = View::with_dialect(
            Path::new("/workspace"),
            crate::agent_runtime::acp::dialect::for_agent(crate::model::AgentKind::Antigravity),
        );
        view.update(
            &ctx,
            &json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "sub-1",
                    "name": "invoke_subagent",
                    "title": "Run invoke_subagent",
                    "kind": "other",
                    "status": "in_progress",
                    "rawInput": {
                        "TypeName": "research",
                        "Role": "Codebase Researcher",
                        "Prompt": "Inspect src/worker.ts"
                    }
                }
            }),
        );
        let tasks = ctx.store.chat_subagents("s1").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].label, "Codebase Researcher");
        assert_eq!(tasks[0].subagent_type.as_deref(), Some("research"));
        assert_eq!(tasks[0].detail, "Inspect src/worker.ts");
        assert_eq!(tasks[0].state, SubagentState::Active);

        view.update(
            &ctx,
            &json!({
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "toolCallId": "sub-1",
                    "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "Found 3 endpoints."}}]
                }
            }),
        );
        let tasks = ctx.store.chat_subagents("s1").unwrap();
        assert_eq!(tasks[0].state, SubagentState::Completed);
        assert_eq!(tasks[0].items.len(), 1);
        assert_eq!(tasks[0].items[0].text, "Found 3 endpoints.");

        let items = ctx.store.chat_items("s1").unwrap();
        let launch_item = items.iter().find(|i| i.id == "acp-tool:sub-1").unwrap();
        assert_eq!(launch_item.task_id.as_deref(), Some("acp-agent:tool:sub-1"));
    }

    #[test]
    fn antigravity_child_prefixed_tool_calls_do_not_split_main_message() {
        let ctx = context();
        let view = View::with_dialect(
            Path::new("/workspace"),
            crate::agent_runtime::acp::dialect::for_agent(crate::model::AgentKind::Antigravity),
        );
        view.set_session("parent-uuid");
        view.update(
            &ctx,
            &json!({
                "sessionId": "parent-uuid",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "已启动 3 个子代理"}
                }
            }),
        );
        view.update(
            &ctx,
            &json!({
                "sessionId": "parent-uuid",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "727f665c-9285-4f84-87ff-e66a40e2f2b5:call_1",
                    "kind": "execute",
                    "title": "uname -a",
                    "status": "in_progress"
                }
            }),
        );
        view.update(
            &ctx,
            &json!({
                "sessionId": "parent-uuid",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "，正在收集实时系统数据："}
                }
            }),
        );
        let items = ctx.store.chat_items("s1").unwrap();
        let messages: Vec<_> = items
            .iter()
            .filter(|i| i.kind == ChatItemKind::AssistantMessage)
            .collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, "已启动 3 个子代理，正在收集实时系统数据：");
        assert!(!items.iter().any(|i| i.id.contains("727f665c")));
    }

    #[test]
    fn generic_view_isolates_provider_extensions() {
        let ctx = context();
        let view = View::new(Path::new("/workspace"));
        view.set_session("main-1");
        view.update(
            &ctx,
            &json!({
                "sessionId": "main-1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "steps-1",
                    "title": "Run update_steps",
                    "status": "completed",
                    "rawInput": {
                        "steps": [{"title": "Should stay tool-only", "status": "done"}]
                    }
                }
            }),
        );
        // Generic dialect must not translate Antigravity `update_steps` into plan controls.
        assert!(view.controls().plan.is_empty());

        // Generic dialect still handles standard ACP `Agent` subagent tool launches.
        view.update(
            &ctx,
            &json!({
                "sessionId": "main-1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "toolCallId": "agent-1",
                    "name": "Agent",
                    "title": "Researcher",
                    "status": "in_progress"
                }
            }),
        );
        let tasks = ctx.store.chat_subagents("s1").unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].label, "Researcher");
    }
}
