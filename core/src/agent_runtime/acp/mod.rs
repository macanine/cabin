//! Agent Client Protocol (ACP) sessions: one agent process per chat, spoken to
//! over JSON-RPC on stdio. Implements the client side of ACP v1 with its
//! unstable additions: prompt turns, cancellation, session load/resume, config
//! options and modes, slash commands, plans, usage, permission requests,
//! elicitation, client file system and terminals, subagent updates and the
//! `_session/steering` extension.
//!
//! Protocol reference: <https://agentclientprotocol.com> (Apache-2.0).

mod antigravity_subagents;
mod dialect;
mod launch;
pub(super) mod rpc;
mod terminal;
mod view;

pub use self::dialect::enrich_stored;
pub(super) use self::view::config_options;

use crate::util::LockExt;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use serde_json::{Value, json};

use self::dialect::{AcpDialect, DialectCaps};
use self::rpc::{Handler, Peer, RpcError};
use self::terminal::Terminals;
use self::view::View;
use super::context::{
    ChatContext, LiveSession, RuntimeSpec, SteerOutcome, TurnInput, approval_item, error_item,
    question_item, system_item,
};
use super::stdio::StdioSpec;
use crate::error::{Error, Result};
use crate::model::{
    AgentKind, ChatAttachment, ChatItemStatus, ChatPermissionMode, ChatPromptCapabilities,
    ChatQuestion, ChatRuntimeCapabilities, PlanDecision, PlanReview,
};

const PROTOCOL_VERSION: u64 = 1;
const START_TIMEOUT: Duration = Duration::from_secs(90);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Default, Clone)]
struct Caps {
    load: bool,
    resume: bool,
    fork: bool,
    list: bool,
    steer: bool,
    images: bool,
    audio: bool,
    embedded: bool,
    plans: bool,
    subagents: bool,
    async_tasks: bool,
    notices: bool,
    compaction: bool,
    goal: bool,
    goal_control_method: Option<String>,
    goal_actions: Vec<String>,
}

fn advertised_goal(init: &Value) -> Option<&Value> {
    init.pointer("/_meta/goal")
        .or_else(|| init.pointer("/_meta/jetbrains/air/goal"))
        .filter(|goal| {
            goal.get("version")
                .and_then(Value::as_i64)
                .is_some_and(|version| version >= 1)
        })
}

fn goal_control(goal: Option<&Value>) -> (Option<String>, Vec<String>) {
    let Some(goal) = goal else {
        return (None, Vec::new());
    };
    let method = goal
        .get("controlMethod")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|method| !method.is_empty())
        .map(str::to_string);
    let actions = goal
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    (method, actions)
}

enum Answer {
    Permission(Option<String>),
    Elicitation(Value),
}

enum Pending {
    Permission {
        item_id: String,
        /// Our decision name → the agent's option id.
        options: Vec<(String, String)>,
    },
    Elicitation {
        item_id: String,
        fields: Vec<Field>,
    },
}

#[derive(Clone)]
struct Field {
    key: String,
    kind: String,
    multiple: bool,
    choices: Vec<String>,
    /// An optional free-text field that goes with this choice ("Other").
    companion: Option<String>,
}

/// The client half: answers the agent's requests and applies its updates.
struct Client {
    dialect: Arc<dyn AcpDialect>,
    ctx: Arc<ChatContext>,
    view: Arc<View>,
    terminals: Arc<Terminals>,
    root: PathBuf,
    env: Vec<(String, String)>,
    seq: AtomicU64,
    pending: Mutex<HashMap<String, (Pending, mpsc::Sender<Answer>)>>,
    stopping: AtomicBool,
    /// Set while the client itself changes the permission options.
    applying: AtomicBool,
    /// Throttle out-of-band subagent syncs during a turn.
    last_subagent_sync: Mutex<Option<Instant>>,
}

impl Client {
    fn next_request(&self) -> String {
        format!("acp-req-{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn permission(&self, params: &Value) -> std::result::Result<Value, RpcError> {
        let tool_call = &params["toolCall"];
        let options: Vec<&Value> = params["options"].as_array().into_iter().flatten().collect();
        let pick = |kinds: &[&str]| {
            options
                .iter()
                .find(|option| kinds.contains(&option["kind"].as_str().unwrap_or_default()))
                .and_then(|option| option["optionId"].as_str())
                .map(str::to_string)
        };
        let tool = self.view.tool_waiting(&self.ctx, params, tool_call, false);
        let tool_kind = tool.kind.clone().unwrap_or_else(|| "other".into());
        let plan = tool.raw_input["plan"]
            .as_str()
            .filter(|plan| !plan.trim().is_empty())
            .map(str::to_string);
        let rendered = self.view.tool_item(&tool);
        let command_text = if rendered.kind == crate::model::ChatItemKind::CommandExecution {
            rendered.text.as_str()
        } else if !rendered.subject.is_empty() {
            rendered.subject.as_str()
        } else {
            tool.title.as_str()
        };
        if self.dialect.auto_allows_request(
            self.ctx.permission(),
            &tool_kind,
            command_text,
            plan.is_some(),
        ) && let Some(option) = pick(&["allow_once", "allow_always"])
        {
            return Ok(json!({"outcome": {"outcome": "selected", "optionId": option}}));
        }
        // One decision per option. An agent may offer several options of one
        // kind (Claude's plan approval offers three ways to allow); the first
        // keeps the plain name, the others get `<kind>:<optionId>`.
        let mut decisions: Vec<(String, String)> = Vec::new();
        let mut labels: Vec<(String, String)> = Vec::new();
        for option in &options {
            let base = match option["kind"].as_str().unwrap_or_default() {
                "allow_once" => "accept",
                "allow_always" => "acceptAlways",
                "reject_once" => "decline",
                "reject_always" => "declineAlways",
                _ => continue,
            };
            let Some(id) = option["optionId"].as_str() else {
                continue;
            };
            let decision = if decisions.iter().any(|(name, _)| name == base) {
                format!("{base}:{id}")
            } else {
                base.to_string()
            };
            labels.push((
                decision.clone(),
                option["name"].as_str().unwrap_or(base).to_string(),
            ));
            decisions.push((decision, id.to_string()));
        }
        if decisions.is_empty() {
            return Ok(json!({"outcome": {"outcome": "cancelled"}}));
        }
        let request_id = self.next_request();
        let item_id = format!("acp-approval:{request_id}");
        let (approval_kind, title) = match tool_kind.as_str() {
            _ if plan.is_some() => ("plan", "Ready to implement this plan?"),
            "execute" => ("command", "Run this command?"),
            "edit" | "delete" | "move" => ("file-change", "Allow this change?"),
            "read" | "search" => ("file-read", "Allow reading?"),
            "fetch" => ("network", "Allow this request?"),
            "switch_mode" => ("mode", "Switch mode?"),
            _ => ("tool", "Allow this tool?"),
        };
        let pairs: Vec<(&str, &str)> = labels
            .iter()
            .map(|(decision, label)| (decision.as_str(), label.as_str()))
            .collect();
        let text = match &plan {
            Some(plan) => plan.clone(),
            None if rendered.subject.is_empty() => tool.title.clone(),
            None => rendered.subject.clone(),
        };
        let mut item = approval_item(&item_id, &request_id, approval_kind, title, &text, &pairs);
        item.changes = rendered.changes.clone();
        item.files = rendered.files.clone();
        item.detail = if rendered.kind == crate::model::ChatItemKind::CommandExecution {
            rendered.text.clone()
        } else {
            String::new()
        };
        let rx = self.publish_request(
            request_id,
            Pending::Permission {
                item_id,
                options: decisions,
            },
            item,
        )?;
        self.view.tool_waiting(&self.ctx, params, tool_call, true);
        self.ctx.waiting();
        let answer = rx.recv().ok();
        if self.ctx.is_active() {
            self.ctx
                .set_state(crate::model::ChatRunState::Running, None);
        }
        let option = match answer {
            Some(Answer::Permission(option)) => option,
            _ => None,
        };
        if !tool.id.is_empty() {
            self.view
                .tool_waiting(&self.ctx, params, &json!({"toolCallId": tool.id}), false);
        }
        Ok(match option {
            Some(option) => json!({"outcome": {"outcome": "selected", "optionId": option}}),
            None => json!({"outcome": {"outcome": "cancelled"}}),
        })
    }

    fn elicitation(&self, params: &Value) -> std::result::Result<Value, RpcError> {
        let message = params["message"].as_str().unwrap_or_default().to_string();
        let request_id = self.next_request();
        let item_id = format!("acp-question:{request_id}");
        let mut fields = Vec::new();
        let questions = match params["mode"].as_str() {
            Some("url") => {
                let url = params["url"].as_str().unwrap_or_default();
                fields.push(Field {
                    key: "__url".into(),
                    kind: "url".into(),
                    multiple: false,
                    choices: vec![],
                    companion: None,
                });
                vec![ChatQuestion {
                    id: "__url".into(),
                    question: format!("{message}\n\n{url}"),
                    options: vec!["Done".into()],
                    multiple: false,
                }]
            }
            _ => {
                let schema = &params["requestedSchema"];
                let required: Vec<&str> = schema["required"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                let mut questions: Vec<ChatQuestion> = Vec::new();
                for (key, property) in schema["properties"].as_object().into_iter().flatten() {
                    let kind = property["type"].as_str().unwrap_or("string").to_string();
                    let (choices, multiple) = enum_choices(property);
                    // A free-text field after a choice is that choice's "other"
                    // answer; the question card already takes typed answers.
                    if kind == "string"
                        && choices.is_empty()
                        && !required.contains(&key.as_str())
                        && let Some(previous) = fields.last_mut()
                        && !previous.choices.is_empty()
                        && previous.companion.is_none()
                    {
                        previous.companion = Some(key.clone());
                        continue;
                    }
                    let header = property["title"].as_str().filter(|text| !text.is_empty());
                    let body = property["description"]
                        .as_str()
                        .filter(|text| !text.is_empty())
                        .or_else(|| {
                            (questions.is_empty()
                                && !message.is_empty()
                                && !(header.is_some() && is_generic_question_count(&message)))
                            .then_some(message.as_str())
                        });
                    let question = match (header, body) {
                        (Some(header), Some(body)) if header != body => format!("{header}\n{body}"),
                        (_, Some(body)) => body.to_string(),
                        (Some(header), None) => header.to_string(),
                        (None, None) => key.clone(),
                    };
                    let options = if kind == "boolean" {
                        vec!["Yes".to_string(), "No".to_string()]
                    } else {
                        choices.clone()
                    };
                    fields.push(Field {
                        key: key.clone(),
                        kind,
                        multiple,
                        choices: options.clone(),
                        companion: None,
                    });
                    questions.push(ChatQuestion {
                        id: key.clone(),
                        question,
                        options,
                        multiple,
                    });
                }
                if questions.is_empty() {
                    fields.push(Field {
                        key: "__confirm".into(),
                        kind: "confirm".into(),
                        multiple: false,
                        choices: vec![],
                        companion: None,
                    });
                    questions.push(ChatQuestion {
                        id: "__confirm".into(),
                        question: message.clone(),
                        options: vec!["OK".into()],
                        multiple: false,
                    });
                }
                questions
            }
        };
        let item = question_item(&item_id, &request_id, "The agent asks", questions);
        let rx =
            self.publish_request(request_id, Pending::Elicitation { item_id, fields }, item)?;
        self.ctx.waiting();
        let answer = rx.recv().ok();
        if self.ctx.is_active() {
            self.ctx
                .set_state(crate::model::ChatRunState::Running, None);
        }
        Ok(match answer {
            Some(Answer::Elicitation(value)) => value,
            _ => json!({"action": "cancel"}),
        })
    }

    fn publish_request(
        &self,
        request_id: String,
        request: Pending,
        item: crate::model::ChatItem,
    ) -> std::result::Result<mpsc::Receiver<Answer>, RpcError> {
        let (tx, rx) = mpsc::channel();
        {
            let mut pending = self.pending.locked();
            if self.stopping.load(Ordering::Acquire) {
                return Err(RpcError::internal("The agent is stopping"));
            }
            pending.insert(request_id.clone(), (request, tx));
        }
        let item = self.ctx.upsert(item).map_err(|error| {
            self.pending.locked().remove(&request_id);
            RpcError::internal(format!("Could not save the agent request: {error}"))
        })?;
        if self.stopping.load(Ordering::Acquire) {
            // Stop may have drained the request before the card was saved.
            let _ = self.ctx.update_item(&item.id, |item| {
                if item.status == ChatItemStatus::Waiting {
                    item.status = ChatItemStatus::Cancelled;
                }
            });
        }
        Ok(rx)
    }

    fn read_file(&self, params: &Value) -> std::result::Result<Value, RpcError> {
        let path = absolute(params["path"].as_str().unwrap_or_default())?;
        let text = std::fs::read_to_string(&path)
            .map_err(|error| RpcError::new(-32002, format!("{}: {error}", path.display())))?;
        let start = params["line"]
            .as_u64()
            .map_or(0, |line| line.saturating_sub(1) as usize);
        let limit = params["limit"].as_u64().map(|limit| limit as usize);
        let content = if start == 0 && limit.is_none() {
            text
        } else {
            text.split_inclusive('\n')
                .skip(start)
                .take(limit.unwrap_or(usize::MAX))
                .collect()
        };
        Ok(json!({"content": content}))
    }

    fn write_file(&self, params: &Value) -> std::result::Result<Value, RpcError> {
        let path = absolute(params["path"].as_str().unwrap_or_default())?;
        let content = params["content"].as_str().unwrap_or_default();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| RpcError::internal(format!("{}: {error}", parent.display())))?;
        }
        std::fs::write(&path, content)
            .map_err(|error| RpcError::internal(format!("{}: {error}", path.display())))?;
        Ok(Value::Null)
    }

    fn cancel_pending(&self) {
        let pending: Vec<_> = self.pending.locked().drain().collect();
        for (_, (pending, tx)) in pending {
            let item_id = match &pending {
                Pending::Permission { item_id, .. } | Pending::Elicitation { item_id, .. } => {
                    item_id.clone()
                }
            };
            let _ = self.ctx.update_item(&item_id, |item| {
                item.status = ChatItemStatus::Cancelled;
            });
            let _ = tx.send(match pending {
                Pending::Permission { .. } => Answer::Permission(None),
                Pending::Elicitation { .. } => Answer::Elicitation(json!({"action": "cancel"})),
            });
        }
    }
}

impl Handler for Client {
    fn notification(&self, method: &str, params: &Value) {
        if let Some(("session/update", update)) = self.dialect.translate_notification(method, params)
        {
            self.view.update(&self.ctx, update.as_ref());
            let kind = update
                .pointer("/update/sessionUpdate")
                .and_then(Value::as_str);
            if matches!(kind, Some("current_mode_update" | "config_option_update"))
                && !self.applying.load(Ordering::Acquire)
            {
                let options = self.view.controls().options;
                if !self
                    .dialect
                    .mode_matches_permission(&options, self.ctx.permission())
                    && let Some(permission) = self.dialect.permission_from(&options)
                {
                    self.ctx.persist_permission(permission);
                }
            }
            if let Some(native_id) = self.view.session_id() {
                let due = {
                    let mut last = self.last_subagent_sync.locked();
                    let ok = last.is_none_or(|t| t.elapsed() >= Duration::from_secs(1));
                    if ok {
                        *last = Some(Instant::now());
                    }
                    ok
                };
                if due {
                    self.dialect
                        .sync_subagents(&native_id, &self.root, &self.ctx, true);
                }
            }
            return;
        }
        // URL elicitation finished on the agent's side.
        let _ = method == "elicitation/complete";
    }

    fn request(&self, method: &str, params: &Value) -> std::result::Result<Value, RpcError> {
        match method {
            "session/request_permission" => self.permission(params),
            "elicitation/create" => self.elicitation(params),
            "fs/read_text_file" => self.read_file(params),
            "fs/write_text_file" => self.write_file(params),
            "terminal/create" => self.terminals.create(params, &self.root, &self.env),
            "terminal/output" => self.terminals.output(params),
            "terminal/wait_for_exit" => self.terminals.wait_for_exit(params),
            "terminal/kill" => self.terminals.kill(params),
            "terminal/release" => self.terminals.release(params),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    fn exited(&self, code: Option<i32>, stderr: &str, expected: bool) {
        let stopping = self.stopping.swap(true, Ordering::AcqRel);
        self.terminals.kill_all();
        self.cancel_pending();
        if expected || stopping {
            return;
        }
        let tail = stderr
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or_default();
        let agent = agent_display_name(self.dialect.as_ref());
        self.ctx.fail(match (code, tail.is_empty()) {
            (_, false) => format!("{agent} stopped: {tail}"),
            (Some(code), true) => format!("{agent} stopped (exit code {code})"),
            (None, true) => format!("{agent} stopped"),
        });
    }
}

pub struct AcpSession {
    ctx: Arc<ChatContext>,
    peer: Arc<Peer>,
    client: Arc<Client>,
    session_id: String,
    caps: Caps,
}

impl AcpSession {
    pub fn spawn(agent: AgentKind, spec: RuntimeSpec, ctx: Arc<ChatContext>) -> Result<Arc<Self>> {
        if let Some(install) = &spec.install {
            super::runtimes::ensure_installed_until(&spec.binary, install, &spec.env, &|| {
                !ctx.is_active()
            })?;
        }
        if !ctx.is_active() {
            return Err(Error::Cancelled);
        }
        let dialect = dialect::for_agent(agent);
        let launch = launch::launch(agent, &spec)?;
        let view = Arc::new(View::with_dialect(&spec.cwd, dialect.clone()));
        let terminal_view = Arc::downgrade(&view);
        let terminal_ctx = Arc::downgrade(&ctx);
        let terminals = Arc::new(Terminals::new(Arc::new(move |terminal: &str| {
            if let (Some(view), Some(ctx)) = (terminal_view.upgrade(), terminal_ctx.upgrade()) {
                view.terminal_changed(&ctx, terminal);
            }
        })));
        view.set_terminals(terminals.clone());
        let client = Arc::new(Client {
            dialect: dialect.clone(),
            ctx: ctx.clone(),
            view: view.clone(),
            terminals,
            root: spec.cwd.clone(),
            env: launch.env.clone(),
            seq: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            stopping: AtomicBool::new(false),
            // Initial native mode updates describe the agent's default until
            // the client's stored permission has been applied.
            applying: AtomicBool::new(dialect.native_permissions()),
            last_subagent_sync: Mutex::new(None),
        });
        let peer = Peer::spawn(
            StdioSpec {
                argv: launch.argv.clone(),
                cwd: &spec.cwd,
                env: &launch.env,
            },
            client.clone(),
        )
        .map_err(|error| match error {
            Error::Io { source, .. } => {
                super::context::provider_missing(crate::agents::adapter(agent).spec().name, source)
            }
            other => other,
        })?;
        match Self::open(&dialect, &spec, &ctx, &peer, &client) {
            Ok(session) => Ok(session),
            Err(error) => {
                client.stopping.store(true, Ordering::Release);
                client.cancel_pending();
                client.terminals.kill_all();
                peer.stop();
                Err(error)
            }
        }
    }

    fn open(
        dialect: &Arc<dyn AcpDialect>,
        spec: &RuntimeSpec,
        ctx: &Arc<ChatContext>,
        peer: &Arc<Peer>,
        client: &Arc<Client>,
    ) -> Result<Arc<Self>> {
        let dialect_caps: DialectCaps = dialect.caps();
        let client_capabilities = dialect_caps.client_capabilities();
        let init = peer.request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": client_capabilities,
                "clientInfo": {"name": "cabin", "version": env!("CARGO_PKG_VERSION")},
            }),
            Some(START_TIMEOUT),
        )?;
        let agent_caps = &init["agentCapabilities"];
        let session_caps = &agent_caps["sessionCapabilities"];
        let goal = advertised_goal(&init);
        let (goal_control_method, goal_actions) = goal_control(goal);
        let caps = Caps {
            load: agent_caps["loadSession"].as_bool().unwrap_or(false),
            resume: session_caps["resume"].is_object(),
            fork: session_caps["fork"].is_object(),
            list: session_caps["list"].is_object(),
            steer: init
                .pointer("/_meta/steering/supported")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || agent_caps
                    .pointer("/_meta/steering/supported")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            images: agent_caps
                .pointer("/promptCapabilities/image")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            audio: agent_caps
                .pointer("/promptCapabilities/audio")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            embedded: agent_caps
                .pointer("/promptCapabilities/embeddedContext")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            plans: dialect_caps.plans,
            subagents: dialect_caps.subagents,
            async_tasks: dialect_caps.air_async_tasks,
            notices: dialect_caps.notices,
            compaction: dialect_caps.compaction,
            goal: goal.is_some(),
            goal_control_method,
            goal_actions,
        };
        let cwd = spec.cwd.display().to_string();
        let base = json!({"cwd": cwd, "mcpServers": []});
        client.view.set_owned(dialect.owned_categories());
        client.view.set_goal_actions(caps.goal_actions.clone());
        let reopened = match spec.native_ref.as_deref() {
            Some(native) if caps.resume || caps.load => {
                let mut params = base.clone();
                params["sessionId"] = json!(native);
                client.view.set_session(native);
                let result = if caps.resume {
                    peer.request("session/resume", params, Some(START_TIMEOUT))
                } else {
                    client.view.set_replaying(true);
                    let result = peer.request("session/load", params, Some(START_TIMEOUT));
                    client.view.set_replaying(false);
                    result
                };
                match result {
                    Ok(result) => Some((native.to_string(), result)),
                    Err(error) => {
                        notice_new_session(ctx, dialect.as_ref(), Some(&error.to_string()));
                        None
                    }
                }
            }
            Some(_) => {
                notice_new_session(ctx, dialect.as_ref(), None);
                None
            }
            None => None,
        };
        let opened = match reopened {
            Some(opened) => opened,
            None => {
                let result = peer.request("session/new", base, Some(START_TIMEOUT))?;
                let id = result["sessionId"]
                    .as_str()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| {
                        Error::Unavailable("The agent opened a session without an id".into())
                    })?
                    .to_string();
                (id, result)
            }
        };
        let (session_id, result) = opened;
        client.view.set_session(&session_id);
        ctx.set_thread(&session_id);
        client.view.edit_controls(ctx, |controls| {
            controls.agent_name = init
                .pointer("/agentInfo/title")
                .or_else(|| init.pointer("/agentInfo/name"))
                .and_then(Value::as_str)
                .map(str::to_string);
            controls.agent_version = init
                .pointer("/agentInfo/version")
                .and_then(Value::as_str)
                .map(str::to_string);
            controls.can_steer = caps.steer;
            controls.images = caps.images;
            controls.capabilities = ChatRuntimeCapabilities {
                prompt: ChatPromptCapabilities {
                    images: caps.images,
                    audio: caps.audio,
                    embedded_context: caps.embedded,
                },
                steer: caps.steer,
                resume: caps.resume,
                load: caps.load,
                fork: caps.fork,
                list: caps.list,
                plans: caps.plans,
                subagents: caps.subagents,
                async_tasks: caps.async_tasks,
                notices: caps.notices,
                compaction: caps.compaction,
                file_read: dialect_caps.client_fs,
                file_write: dialect_caps.client_fs,
                terminals: dialect_caps.client_terminal,
                goal: caps.goal,
            };
            if result["configOptions"].is_array() {
                view::merge_options(controls, view::config_options(&result["configOptions"]));
            }
            if let Some(models) = view::legacy_models(&result["models"])
                && !controls
                    .options
                    .iter()
                    .any(|option| option.category.as_deref() == Some("model"))
            {
                controls.options.push(models);
            }
            if let Some(modes) = view::legacy_modes(&result["modes"])
                && !controls
                    .options
                    .iter()
                    .any(|option| option.category.as_deref() == Some("mode"))
            {
                controls.options.push(modes);
            }
        });
        let session = Arc::new(Self {
            ctx: ctx.clone(),
            peer: peer.clone(),
            client: client.clone(),
            session_id,
            caps,
        });
        if let Some(model) = spec
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            && let Err(error) = session.apply_model(model)
        {
            let _ = ctx.upsert(system_item(
                "acp-model-warning",
                &format!("Could not select the model {model}: {error}"),
                ChatItemStatus::Completed,
            ));
        }
        let permission = session.apply_permission(spec.permission);
        if dialect.native_permissions() {
            permission?;
        }
        Ok(session)
    }

    fn option(&self, category: &str) -> Option<crate::model::ChatConfigOption> {
        self.client
            .view
            .controls()
            .options
            .into_iter()
            .find(|option| option.category.as_deref() == Some(category))
    }

    fn set_option_value(&self, option_id: &str, value: &str) -> Result<()> {
        if view::is_legacy_mode(option_id) {
            self.peer.request(
                "session/set_mode",
                json!({"sessionId": self.session_id, "modeId": value}),
                Some(REQUEST_TIMEOUT),
            )?;
            self.client.view.edit_controls(&self.ctx, |controls| {
                for option in &mut controls.options {
                    if option.id == option_id {
                        option.current = value.to_string();
                    }
                }
            });
            return Ok(());
        }
        if view::is_legacy_model(option_id) {
            self.peer.request(
                "session/set_model",
                json!({"sessionId": self.session_id, "modelId": value}),
                Some(REQUEST_TIMEOUT),
            )?;
            self.client.view.edit_controls(&self.ctx, |controls| {
                for option in &mut controls.options {
                    if option.id == option_id {
                        option.current = value.to_string();
                    }
                }
            });
            return Ok(());
        }
        let boolean = self
            .client
            .view
            .controls()
            .options
            .iter()
            .any(|option| option.id == option_id && option.kind == "boolean");
        let params = if boolean {
            json!({"sessionId": self.session_id, "configId": option_id, "type": "boolean", "value": value == "true"})
        } else {
            json!({"sessionId": self.session_id, "configId": option_id, "value": value})
        };
        let result =
            self.peer
                .request("session/set_config_option", params, Some(REQUEST_TIMEOUT))?;
        if result["configOptions"].is_array() {
            let options = view::config_options(&result["configOptions"]);
            self.client
                .view
                .edit_controls(&self.ctx, |controls| view::merge_options(controls, options));
        } else {
            self.client.view.edit_controls(&self.ctx, |controls| {
                for option in &mut controls.options {
                    if option.id == option_id {
                        option.current = value.to_string();
                    }
                }
            });
        }
        Ok(())
    }

    fn apply_model(&self, model: &str) -> Result<()> {
        if self.client.dialect.use_agent_default(model) {
            return Ok(());
        }
        let Some(option) = self.option("model") else {
            return Ok(());
        };
        let (value, inferred_effort) = match match_choice(&option, model) {
            Some(value) => (value, None),
            None => match self
                .client
                .dialect
                .split_model_effort(model)
                .and_then(|(family, effort)| Some((match_choice(&option, family)?, effort)))
            {
                Some((value, effort)) => (value, Some(effort)),
                None => {
                    return Err(Error::invalid(format!(
                        "{model} is not offered by this agent"
                    )));
                }
            },
        };
        if option.current != value {
            self.set_option_value(&option.id, &value)?;
        }
        if let Some(effort) = inferred_effort
            && let Some(thought) = self.option("thought_level")
            && thought.current != effort
            && thought.choices.iter().any(|choice| choice.value == effort)
        {
            self.set_option_value(&thought.id, effort)?;
        }
        Ok(())
    }

    /// Set the options that carry the permission choice. Mode updates the
    /// agent sends meanwhile are not read back as a choice of the user's.
    fn apply_permission(&self, permission: ChatPermissionMode) -> Result<()> {
        self.client.applying.store(true, Ordering::Release);
        let result = (|| {
            let settings = self.client.dialect.settings_for(permission);
            let native = self.client.dialect.native_permissions();
            let unavailable =
                || Error::Unavailable("The agent does not offer this permission mode".into());
            if native && settings.is_empty() {
                return Err(unavailable());
            }
            for (category, wanted) in settings {
                let choice = self.option(category).and_then(|option| {
                    let value = wanted
                        .iter()
                        .copied()
                        .find(|value| option.choices.iter().any(|choice| choice.value == *value))?;
                    Some((option, value))
                });
                let Some((option, value)) = choice else {
                    if native {
                        return Err(unavailable());
                    }
                    continue;
                };
                if option.current != value {
                    self.set_option_value(&option.id, value)?;
                }
            }
            Ok(())
        })();
        self.client.applying.store(false, Ordering::Release);
        result
    }

    fn prompt_blocks(&self, text: &str, attachments: &[ChatAttachment]) -> Vec<Value> {
        let mut blocks = Vec::new();
        if !text.is_empty() {
            blocks.push(json!({"type": "text", "text": text}));
        }
        for attachment in attachments {
            let uri = url::Url::from_file_path(&attachment.path)
                .map(|url| url.to_string())
                .unwrap_or_else(|_| format!("file://{}", attachment.path));
            let mime = attachment.mime_type.as_str();
            let bytes = std::fs::read(&attachment.path).ok();
            let block = match bytes {
                Some(bytes) if mime.starts_with("image/") && self.caps.images => json!({
                    "type": "image",
                    "mimeType": mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    "uri": uri,
                }),
                Some(bytes) if mime.starts_with("audio/") && self.caps.audio => json!({
                    "type": "audio",
                    "mimeType": mime,
                    "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                }),
                Some(bytes) if self.caps.embedded && is_text(mime, &bytes) => json!({
                    "type": "resource",
                    "resource": {
                        "uri": uri,
                        "mimeType": mime,
                        "text": String::from_utf8_lossy(&bytes),
                    },
                }),
                Some(bytes) if self.caps.embedded => json!({
                    "type": "resource",
                    "resource": {
                        "uri": uri,
                        "mimeType": mime,
                        "blob": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    },
                }),
                _ => json!({
                    "type": "resource_link",
                    "uri": uri,
                    "name": attachment.name,
                    "mimeType": mime,
                }),
            };
            blocks.push(block);
        }
        blocks
    }

    /// Run one prompt turn to its end. Agent errors end the turn with an
    /// error card; only a lost process fails the chat.
    fn run_turn(&self, turn: &TurnInput) -> Result<()> {
        self.client.view.begin_turn(&self.ctx);
        let blocks = self.prompt_blocks(&turn.text, &turn.attachments);
        let reply = self.peer.request_raw(
            "session/prompt",
            json!({"sessionId": self.session_id, "prompt": blocks}),
            None,
        )?;
        match reply {
            Ok(result) => {
                self.client.dialect.sync_subagents(
                    &self.session_id,
                    &self.client.root,
                    &self.ctx,
                    false,
                );
                self.client
                    .view
                    .end_turn(&self.ctx, &turn.id, result.get("usage"));
                let stop = result["stopReason"].as_str().unwrap_or("end_turn");
                let notice = match stop {
                    "max_tokens" => Some("The reply hit the model's output limit."),
                    "max_turn_requests" => {
                        Some("The agent reached its limit of requests for one turn.")
                    }
                    "refusal" => Some(
                        "The model refused to continue. This message and what follows will not be part of later turns.",
                    ),
                    _ => None,
                };
                if let Some(notice) = notice {
                    let _ = self.ctx.upsert(system_item(
                        &format!("acp-stop:{}", turn.id),
                        notice,
                        ChatItemStatus::Completed,
                    ));
                }
                self.ctx.finish_turn(stop != "cancelled");
            }
            Err(error) => {
                self.client.view.end_turn(&self.ctx, &turn.id, None);
                let error = Error::from(error);
                let _ = self.ctx.upsert(error_item(
                    &format!("acp-error:{}", turn.id),
                    &error.to_string(),
                ));
                self.ctx.finish_turn(false);
            }
        }
        Ok(())
    }
}

impl LiveSession for AcpSession {
    fn pid(&self) -> u32 {
        self.peer.pid()
    }

    fn send(&self, turn: TurnInput) -> Result<()> {
        if let Some(model) = turn
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            && let Err(error) = self.apply_model(model)
        {
            let _ = self.ctx.upsert(system_item(
                &format!("acp-model-warning:{}", turn.id),
                &format!("Could not select the model {model}: {error}"),
                ChatItemStatus::Completed,
            ));
        }
        self.run_turn(&turn)
    }

    fn steer(&self, turn: TurnInput) -> Result<SteerOutcome> {
        if !self.caps.steer {
            return Err(Error::Conflict(format!(
                "{} cannot take a message while it works. Queue it instead.",
                agent_display_name(self.client.dialect.as_ref())
            )));
        }
        self.client.view.close_segments(&self.ctx);
        let blocks = self.prompt_blocks(&turn.text, &turn.attachments);
        let result = self.peer.request(
            "_session/steering",
            json!({
                "sessionId": self.session_id,
                "prompt": blocks,
                "_meta": {"steering": {"idleBehavior": "promptRequired"}},
            }),
            Some(REQUEST_TIMEOUT),
        )?;
        match result["outcome"].as_str() {
            Some("injected") | Some("startedNewTurn") => Ok(SteerOutcome::Delivered),
            Some("promptRequired") => Ok(SteerOutcome::PromptRequired),
            other => Err(Error::Unavailable(format!(
                "The agent answered the message with an unknown outcome {other:?}"
            ))),
        }
    }

    fn respond(&self, request_id: &str, decision: &str, answers: &Value) -> Result<()> {
        let entry = self.client.pending.locked().remove(request_id);
        let Some((pending, tx)) = entry else {
            return Err(Error::NotFound("request".into()));
        };
        match pending {
            Pending::Permission { item_id, options } => {
                let option = options
                    .iter()
                    .find(|(name, _)| name == decision)
                    .map(|(_, option)| option.clone());
                let approved = decision.starts_with("accept");
                let _ = self.ctx.update_item(&item_id, |item| {
                    item.status = if option.is_none() {
                        ChatItemStatus::Cancelled
                    } else if approved {
                        ChatItemStatus::Completed
                    } else {
                        ChatItemStatus::Failed
                    };
                    item.detail = match (option.is_some(), approved) {
                        (false, _) => "Cancelled".into(),
                        (true, true) => "Allowed".into(),
                        (true, false) => "Denied".into(),
                    };
                });
                let _ = tx.send(Answer::Permission(option));
            }
            Pending::Elicitation { item_id, fields } => {
                let declined = matches!(decision, "deny" | "decline" | "cancel");
                let value = if declined {
                    json!({"action": if decision == "cancel" { "cancel" } else { "decline" }})
                } else {
                    let mut content = serde_json::Map::new();
                    for field in fields.iter().filter(|field| !field.key.starts_with("__")) {
                        let picked: Vec<String> = match &answers[&field.key] {
                            Value::Array(values) => values
                                .iter()
                                .filter_map(|value| value.as_str().map(str::to_string))
                                .collect(),
                            Value::String(text) => vec![text.clone()],
                            _ => vec![],
                        };
                        // Typed answers go to the companion field when there is one.
                        let (picked, typed): (Vec<String>, Vec<String>) = match &field.companion {
                            Some(_) => picked
                                .into_iter()
                                .partition(|value| field.choices.contains(value)),
                            None => (picked, vec![]),
                        };
                        if let Some(companion) = &field.companion
                            && !typed.is_empty()
                        {
                            content.insert(companion.clone(), json!(typed.join(", ")));
                        }
                        if picked.is_empty() && !typed.is_empty() {
                            continue;
                        }
                        let value = match field.kind.as_str() {
                            "boolean" => json!(picked.first().is_some_and(|value| value == "Yes")),
                            "number" | "integer" => picked
                                .first()
                                .and_then(|value| value.parse::<f64>().ok())
                                .map_or(Value::Null, |number| json!(number)),
                            _ if field.multiple => json!(picked),
                            _ => json!(picked.join(", ")),
                        };
                        if !value.is_null() {
                            content.insert(field.key.clone(), value);
                        }
                    }
                    json!({"action": "accept", "content": content})
                };
                let _ = self.ctx.update_item(&item_id, |item| {
                    item.status = if declined {
                        ChatItemStatus::Cancelled
                    } else {
                        ChatItemStatus::Completed
                    };
                });
                let _ = tx.send(Answer::Elicitation(value));
            }
        }
        Ok(())
    }

    fn respond_plan(
        &self,
        _review: &PlanReview,
        _decision: PlanDecision,
        _feedback: Option<&str>,
    ) -> Result<()> {
        Err(Error::Unavailable(
            "This agent does not wait for plan approval over ACP".into(),
        ))
    }

    fn interrupt(&self) -> Result<()> {
        self.client.cancel_pending();
        self.peer
            .notify("session/cancel", json!({"sessionId": self.session_id}))
    }

    fn set_permission(&self, permission: ChatPermissionMode) -> Result<()> {
        self.apply_permission(permission)
    }

    fn set_model(&self, model: Option<String>) -> Result<()> {
        match model.as_deref().filter(|model| !model.trim().is_empty()) {
            Some(model) => self.apply_model(model),
            None => Ok(()),
        }
    }

    fn set_option(&self, option_id: &str, value: &str) -> Result<()> {
        let known = self
            .client
            .view
            .controls()
            .options
            .into_iter()
            .find(|option| option.id == option_id)
            .ok_or_else(|| Error::NotFound("option".into()))?;
        if known.kind == "select" && !known.choices.iter().any(|choice| choice.value == value) {
            return Err(Error::invalid(format!(
                "{value} is not a choice of {}",
                known.name
            )));
        }
        self.set_option_value(option_id, value)
    }

    fn stop_task(&self, task_id: &str) -> Result<bool> {
        let result = self.peer.request(
            "_session/async_task/stop",
            json!({"sessionId": self.session_id, "asyncTaskId": task_id}),
            Some(REQUEST_TIMEOUT),
        )?;
        Ok(result["stopped"].as_bool().unwrap_or(false))
    }

    fn goal_control(&self, action: &str) -> Result<()> {
        let method = self
            .caps
            .goal_control_method
            .as_deref()
            .ok_or_else(|| Error::Unavailable("This agent has no goal control".into()))?;
        if !self.caps.goal_actions.iter().any(|known| known == action) {
            return Err(Error::invalid(format!(
                "This agent does not offer the goal action \"{action}\""
            )));
        }
        self.peer.request(
            method,
            json!({"sessionId": self.session_id, "action": action}),
            Some(REQUEST_TIMEOUT),
        )?;
        Ok(())
    }

    fn stop(&self) {
        self.client.stopping.store(true, Ordering::Release);
        self.client.cancel_pending();
        self.client.terminals.kill_all();
        self.peer.stop();
        self.ctx.stop();
    }
}

fn absolute(path: &str) -> std::result::Result<PathBuf, RpcError> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(RpcError::invalid_params("File paths must be absolute"))
    }
}

fn enum_choices(property: &Value) -> (Vec<String>, bool) {
    let labels = |values: &Value| -> Vec<String> {
        values
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|value| {
                value
                    .as_str()
                    .or_else(|| value["const"].as_str())
                    .map(str::to_string)
            })
            .collect()
    };
    if property["type"] == "array" {
        let items = &property["items"];
        let choices = if items["enum"].is_array() {
            labels(&items["enum"])
        } else {
            labels(&items["anyOf"])
        };
        return (choices, true);
    }
    if property["enum"].is_array() {
        return (labels(&property["enum"]), false);
    }
    (labels(&property["oneOf"]), false)
}

fn is_text(mime: &str, bytes: &[u8]) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json" | "application/xml" | "application/javascript" | "application/x-sh"
        )
        || (std::str::from_utf8(bytes).is_ok() && !bytes.contains(&0))
}

fn agent_display_name(dialect: &dyn AcpDialect) -> &'static str {
    dialect
        .kind()
        .map_or("Agent", |agent| crate::agents::adapter(agent).spec().name)
}

/// The conversation could not be reopened: say so in the timeline and carry on
/// in a new agent session. The messages stay; the agent does not remember them.
fn notice_new_session(ctx: &ChatContext, dialect: &dyn AcpDialect, reason: Option<&str>) {
    let name = agent_display_name(dialect);
    let mut text = format!(
        "{name} started a new session: the earlier conversation could not be reopened, so it does not remember the messages above."
    );
    if let Some(reason) = reason {
        text.push_str(&format!("\n\n{reason}"));
    }
    let _ = ctx.upsert(system_item(
        &format!("acp-new-session:{}", crate::util::now_millis()),
        &text,
        ChatItemStatus::Completed,
    ));
}

fn is_generic_question_count(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed == "The agent has a question." {
        return true;
    }
    trimmed
        .strip_prefix("The agent has ")
        .and_then(|rest| rest.strip_suffix(" questions."))
        .is_some_and(|count| !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit()))
}

/// The choice a model selection names: the exact value, then the same value
/// or name ignoring case, then a value that starts with it (`gpt-5` →
/// `gpt-5/medium`).
fn match_choice(option: &crate::model::ChatConfigOption, wanted: &str) -> Option<String> {
    if option.choices.is_empty() || option.current == wanted {
        return Some(wanted.to_string());
    }
    let lower = wanted.to_lowercase();
    option
        .choices
        .iter()
        .find(|choice| choice.value == wanted)
        .or_else(|| {
            option.choices.iter().find(|choice| {
                choice.value.to_lowercase() == lower || choice.name.to_lowercase() == lower
            })
        })
        .or_else(|| {
            option.choices.iter().find(|choice| {
                let value = choice.value.to_lowercase();
                value.starts_with(&format!("{lower}/"))
                    || value.starts_with(&format!("{lower}["))
                    || value.rsplit('/').next() == Some(lower.as_str())
            })
        })
        .map(|choice| choice.value.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatConfigChoice, ChatConfigOption};

    fn models(values: &[&str]) -> ChatConfigOption {
        ChatConfigOption {
            id: "model".into(),
            name: "Model".into(),
            description: None,
            category: Some("model".into()),
            kind: "select".into(),
            current: values[0].into(),
            choices: values
                .iter()
                .map(|value| ChatConfigChoice {
                    value: (*value).into(),
                    name: value.to_uppercase(),
                    description: None,
                    group: None,
                })
                .collect(),
            hidden: false,
        }
    }

    #[test]
    fn a_model_choice_matches_exactly_then_loosely() {
        let option = models(&["default", "opus", "gpt-5.3-codex/medium", "openai/gpt-5"]);
        assert_eq!(match_choice(&option, "opus").as_deref(), Some("opus"));
        assert_eq!(match_choice(&option, "OPUS").as_deref(), Some("opus"));
        assert_eq!(
            match_choice(&option, "gpt-5.3-codex").as_deref(),
            Some("gpt-5.3-codex/medium")
        );
        assert_eq!(
            match_choice(&option, "gpt-5").as_deref(),
            Some("openai/gpt-5")
        );
        assert_eq!(match_choice(&option, "sonnet"), None);
        assert!(is_generic_question_count("The agent has a question."));
        assert!(is_generic_question_count("The agent has 2 questions."));
        assert!(!is_generic_question_count("Choose a target branch."));
    }

    #[test]
    fn elicitation_schema_choices_cover_enum_one_of_and_multi_select() {
        assert_eq!(
            enum_choices(&json!({"type": "string", "enum": ["a", "b"]})),
            (vec!["a".into(), "b".into()], false)
        );
        assert_eq!(
            enum_choices(&json!({"type": "string", "oneOf": [{"const": "x", "title": "X"}]})),
            (vec!["x".into()], false)
        );
        assert_eq!(
            enum_choices(
                &json!({"type": "array", "items": {"anyOf": [{"const": "p"}, {"const": "q"}]}})
            ),
            (vec!["p".into(), "q".into()], true)
        );
    }

    #[test]
    fn goal_extension_is_read_from_the_neutral_and_air_homes() {
        let neutral = json!({
            "_meta": {"goal": {
                "version": 1,
                "controlMethod": "_session/goal",
                "actions": ["set", "clear"]
            }}
        });
        let goal = advertised_goal(&neutral).unwrap();
        assert_eq!(
            goal_control(Some(goal)),
            (
                Some("_session/goal".into()),
                vec!["set".into(), "clear".into()]
            )
        );

        let air = json!({
            "_meta": {"jetbrains": {"air": {"goal": {
                "version": 2,
                "controlMethod": "_session/goal",
                "actions": ["set", "pause", "resume", "clear"]
            }}}}
        });
        let goal = advertised_goal(&air).unwrap();
        assert_eq!(goal_control(Some(goal)).1.len(), 4);

        assert!(advertised_goal(&json!({"_meta": {"goal": {"version": 0}}})).is_none());
        assert!(advertised_goal(&json!({"_meta": {"goal": {}}})).is_none());
    }
}
