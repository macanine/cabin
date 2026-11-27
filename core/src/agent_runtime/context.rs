//! The write path every chat runtime gets (`ChatContext`), the live-session
//! interface (`LiveSession`), and small `ChatItem` builders. Chats run over
//! ACP (`super::acp`); Rust owns the durable items and run state.

use crate::util::LockExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;

use crate::chat::{Sink, emit, settle_items};
use crate::error::{Error, Result};
use crate::model::{
    AgentKind, AppEvent, ChatAsyncTask, ChatAttachment, ChatControls, ChatItem, ChatItemKind,
    ChatItemStatus, ChatPermissionMode, ChatQuestion, ChatRunState, ChatState, ChatTurnUsage,
    ChatUsage, PlanExecutionStatus, PlanReview, SessionStatus, SubagentState, SubagentTask,
    ToolAction,
};
use crate::store::Store;

#[derive(Clone)]
pub struct RuntimeSpec {
    pub provider: String,
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub model: Option<String>,
    pub permission: ChatPermissionMode,
    pub native_ref: Option<String>,
    pub prompt: Option<String>,
    pub prompt_id: Option<String>,
    pub attachments: Vec<ChatAttachment>,
    pub env: Vec<(String, String)>,
    pub install: Option<Vec<String>>,
}

pub struct TurnInput {
    pub id: String,
    pub text: String,
    pub attachments: Vec<ChatAttachment>,
    pub model: Option<String>,
}

/// What happened to a message sent while a turn runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    /// The agent took it into the running turn (or started one with it).
    Delivered,
    /// The turn ended first; the message must be sent as a new prompt.
    PromptRequired,
}

pub trait LiveSession: Send + Sync {
    fn pid(&self) -> u32;
    fn send(&self, turn: TurnInput) -> Result<()>;
    /// Deliver a message into the running turn.
    fn steer(&self, _turn: TurnInput) -> Result<SteerOutcome> {
        Err(Error::Conflict(
            "This agent cannot take a message while it works".into(),
        ))
    }
    /// Change one of the agent's session config options.
    fn set_option(&self, _option: &str, _value: &str) -> Result<()> {
        Err(Error::Unavailable(
            "This agent has no session options".into(),
        ))
    }
    fn respond(&self, request_id: &str, decision: &str, answers: &Value) -> Result<()>;
    fn respond_plan(
        &self,
        review: &PlanReview,
        decision: crate::model::PlanDecision,
        feedback: Option<&str>,
    ) -> Result<()>;
    fn interrupt(&self) -> Result<()>;
    fn set_permission(&self, permission: ChatPermissionMode) -> Result<()>;
    fn set_model(&self, model: Option<String>) -> Result<()>;
    /// Stop one provider-reported background task.
    fn stop_task(&self, _task_id: &str) -> Result<bool> {
        Err(Error::Unavailable(
            "This agent has no background tasks".into(),
        ))
    }
    /// Pause or clear the agent's current goal through its advertised channel.
    fn goal_control(&self, _action: &str) -> Result<()> {
        Err(Error::Unavailable("This agent has no goal control".into()))
    }
    fn stop(&self);
}

/// Durable chat state and the only write path provider adapters get.
pub struct ChatContext {
    pub id: String,
    pub root: PathBuf,
    pub store: Arc<Store>,
    pub sink: Arc<RwLock<Option<Sink>>>,
    state: Mutex<ChatState>,
    permission: Mutex<ChatPermissionMode>,
    active: AtomicBool,
    baseline: Mutex<Option<String>>,
    plan_execution: Mutex<Option<(String, i64)>>,
    controls: Mutex<ChatControls>,
    current_turn: Mutex<String>,
    tasks: Mutex<HashMap<String, ChatAsyncTask>>,
}

impl ChatContext {
    pub fn new(
        id: &str,
        root: PathBuf,
        store: Arc<Store>,
        sink: Arc<RwLock<Option<Sink>>>,
        permission: ChatPermissionMode,
    ) -> Arc<Self> {
        let current_turn = store
            .chat_items(id)
            .ok()
            .and_then(|items| {
                items
                    .iter()
                    .rev()
                    .find(|item| item.kind == ChatItemKind::UserMessage)
                    .map(|item| {
                        if item.turn_id.is_empty() {
                            item.id.clone()
                        } else {
                            item.turn_id.clone()
                        }
                    })
            })
            .unwrap_or_else(|| "start".into());
        Arc::new(Self {
            id: id.to_string(),
            root,
            store,
            sink,
            state: Mutex::new(ChatState {
                state: ChatRunState::Starting,
                message: None,
            }),
            permission: Mutex::new(permission),
            active: AtomicBool::new(true),
            baseline: Mutex::new(None),
            plan_execution: Mutex::new(None),
            controls: Mutex::new(ChatControls::default()),
            current_turn: Mutex::new(current_turn),
            tasks: Mutex::new(HashMap::new()),
        })
    }

    pub fn controls(&self) -> ChatControls {
        self.controls.locked().clone()
    }

    pub fn publish_controls(&self, controls: ChatControls) {
        {
            let mut current = self.controls.locked();
            if *current == controls {
                return;
            }
            *current = controls.clone();
        }
        emit(
            &self.sink,
            AppEvent::ChatControls {
                id: self.id.clone(),
                controls,
            },
        );
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub fn state(&self) -> ChatState {
        self.state.locked().clone()
    }

    pub fn permission(&self) -> ChatPermissionMode {
        *self.permission.locked()
    }

    /// The agent changed its own mode: keep the chat's choice and the UI in step.
    pub fn persist_permission(&self, permission: ChatPermissionMode) {
        if self.permission() == permission {
            return;
        }
        self.set_permission(permission);
        if self.store.set_chat_permission(&self.id, permission).is_ok() {
            emit(
                &self.sink,
                AppEvent::ChatPermission {
                    id: self.id.clone(),
                    permission,
                },
            );
        }
    }

    pub fn set_permission(&self, permission: ChatPermissionMode) {
        *self.permission.locked() = permission;
    }

    pub fn set_state(&self, next: ChatRunState, message: Option<String>) {
        self.transition(next, message, false);
    }

    fn transition(&self, next: ChatRunState, message: Option<String>, final_state: bool) {
        let state = ChatState {
            state: next,
            message,
        };
        {
            let mut current = self.state.locked();
            if !final_state && !self.is_active() {
                return;
            }
            *current = state.clone();
        }
        let status = match next {
            ChatRunState::Starting => SessionStatus::Starting,
            ChatRunState::Stopped => SessionStatus::Stopped,
            ChatRunState::Error => SessionStatus::Failed {
                message: state
                    .message
                    .clone()
                    .unwrap_or_else(|| "The agent stopped".into()),
            },
            _ => SessionStatus::Running { pid: 0 },
        };
        emit(
            &self.sink,
            AppEvent::ChatState {
                id: self.id.clone(),
                state,
            },
        );
        emit(
            &self.sink,
            AppEvent::SessionStatus {
                id: self.id.clone(),
                status,
            },
        );
    }

    pub fn begin_turn(&self) {
        *self.baseline.locked() = crate::workspace_diff::snapshot(&self.root);
        self.set_state(ChatRunState::Running, None);
    }

    pub fn current_turn(&self) -> String {
        self.current_turn.locked().clone()
    }

    fn assign_turn(&self, item: &mut ChatItem) {
        if item.kind == ChatItemKind::UserMessage {
            if item.turn_id.is_empty() {
                item.turn_id = item.id.clone();
            }
            *self.current_turn.locked() = item.turn_id.clone();
            return;
        }
        if item.turn_id.is_empty() {
            item.turn_id = self.current_turn();
        }
    }

    pub fn upsert(&self, mut item: ChatItem) -> Result<ChatItem> {
        self.assign_turn(&mut item);
        if matches!(
            item.kind,
            ChatItemKind::Tool | ChatItemKind::CommandExecution | ChatItemKind::FileChange
        ) {
            crate::chat_view::present(&mut item, &self.root);
        }
        let item = self.store.put_chat_item(&self.id, item)?;
        self.publish_item(&item);
        Ok(item)
    }

    fn publish_item(&self, item: &ChatItem) {
        emit(
            &self.sink,
            AppEvent::ChatItem {
                id: self.id.clone(),
                item: item.clone(),
            },
        );
    }

    pub fn tasks(&self) -> Vec<ChatAsyncTask> {
        let mut tasks: Vec<_> = self.tasks.locked().values().cloned().collect();
        tasks.sort_by(|left, right| {
            left.started_at
                .cmp(&right.started_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        tasks
    }

    pub fn task(&self, id: &str) -> Option<ChatAsyncTask> {
        self.tasks.locked().get(id).cloned()
    }

    pub fn upsert_task(&self, task: ChatAsyncTask) {
        {
            let mut tasks = self.tasks.locked();
            tasks.insert(task.id.clone(), task.clone());
        }
        emit(
            &self.sink,
            AppEvent::ChatTask {
                id: self.id.clone(),
                task,
            },
        );
    }

    pub fn update_item(
        &self,
        id: &str,
        update: impl FnOnce(&mut ChatItem),
    ) -> Result<Option<ChatItem>> {
        let item = self.store.update_chat_item(&self.id, id, update)?;
        if let Some(item) = &item {
            self.publish_item(item);
        }
        Ok(item)
    }

    pub fn plan_draft(&self, mut plan: PlanReview) -> Result<PlanReview> {
        plan.agent = AgentKind::parse(plan.agent.as_str())?;
        let plan = self.store.put_plan_draft(&self.id, plan)?;
        emit(
            &self.sink,
            AppEvent::ChatPlan {
                id: self.id.clone(),
                plan: plan.clone(),
            },
        );
        Ok(plan)
    }

    pub fn record_plan(&self, mut plan: PlanReview) -> Result<PlanReview> {
        plan.complete = true;
        let plan = self.store.record_plan_review(&self.id, plan)?;
        emit(
            &self.sink,
            AppEvent::ChatPlan {
                id: self.id.clone(),
                plan: plan.clone(),
            },
        );
        Ok(plan)
    }

    pub fn publish_plan(&self, plan: PlanReview) {
        emit(
            &self.sink,
            AppEvent::ChatPlan {
                id: self.id.clone(),
                plan,
            },
        );
    }

    pub fn begin_plan_execution(&self, plan_id: String, revision: i64) {
        *self.plan_execution.locked() = Some((plan_id, revision));
    }

    pub fn clear_plan_execution(&self) {
        self.plan_execution.locked().take();
    }

    pub fn subagent(&self, id: &str) -> Option<SubagentTask> {
        self.store
            .chat_subagents(&self.id)
            .ok()?
            .into_iter()
            .find(|task| task.id == id)
    }

    pub fn ensure_subagent(&self, mut task: SubagentTask) -> Result<SubagentTask> {
        if let Some(current) = self.subagent(&task.id) {
            task.started_at = current.started_at;
            if task.label.trim().is_empty() {
                task.label = current.label;
            }
            if task.subagent_type.is_none() {
                task.subagent_type = current.subagent_type;
            }
            if task.detail.trim().is_empty() {
                task.detail = current.detail;
            }
            if task.items.is_empty() {
                task.items = current.items;
            }
        }
        let task = self.store.put_chat_subagent(&self.id, task)?;
        emit(
            &self.sink,
            AppEvent::ChatSubagent {
                id: self.id.clone(),
                task: task.clone(),
            },
        );
        Ok(task)
    }

    pub fn upsert_subagent_item(&self, task_id: &str, mut item: ChatItem) -> Result<()> {
        let mut task = self
            .subagent(task_id)
            .ok_or_else(|| Error::NotFound("subagent".into()))?;
        let now = crate::util::now_millis();
        if item.turn_id.is_empty() {
            item.turn_id = task_id.to_string();
        }
        item.updated_at = now;
        if let Some(current) = task.items.iter_mut().find(|current| current.id == item.id) {
            let started = current.started_at;
            item.started_at = if started > 0 { started } else { now };
            item.ordinal = current.ordinal;
            item.revision = current.revision.saturating_add(1);
            *current = item;
        } else {
            item.started_at = now;
            item.ordinal = task.items.last().map_or(1, |item| item.ordinal + 1);
            item.revision = 1;
            task.items.push(item);
        }
        task.updated_at = now;
        let task = self.store.put_chat_subagent(&self.id, task)?;
        emit(
            &self.sink,
            AppEvent::ChatSubagent {
                id: self.id.clone(),
                task,
            },
        );
        Ok(())
    }

    pub fn finish_subagent(
        &self,
        task_id: &str,
        state: SubagentState,
        detail: Option<&str>,
    ) -> Result<()> {
        let Some(mut task) = self.subagent(task_id) else {
            return Ok(());
        };
        task.state = state;
        if let Some(detail) = detail.map(str::trim).filter(|detail| !detail.is_empty()) {
            task.detail = detail.to_string();
        }
        let task = self.store.put_chat_subagent(&self.id, task)?;
        emit(
            &self.sink,
            AppEvent::ChatSubagent {
                id: self.id.clone(),
                task,
            },
        );
        Ok(())
    }

    pub fn settle_subagents(&self) {
        if let Ok(tasks) = self.store.settle_active_subagents(&self.id) {
            for task in tasks {
                emit(
                    &self.sink,
                    AppEvent::ChatSubagent {
                        id: self.id.clone(),
                        task,
                    },
                );
            }
        }
    }

    pub fn expire_plans(&self) {
        if let Ok(plans) = self.store.expire_open_plans(&self.id) {
            for plan in plans {
                emit(
                    &self.sink,
                    AppEvent::ChatPlan {
                        id: self.id.clone(),
                        plan,
                    },
                );
            }
        }
    }

    pub fn usage(&self, turn_id: &str, usage: ChatUsage) {
        let turn = ChatTurnUsage {
            turn_id: turn_id.to_string(),
            usage,
        };
        if self.store.put_chat_usage(&self.id, &turn).is_ok() {
            emit(
                &self.sink,
                AppEvent::ChatUsage {
                    id: self.id.clone(),
                    turn,
                },
            );
        }
    }

    pub fn set_thread(&self, native_ref: &str) {
        let _ = self.store.set_chat_thread(&self.id, native_ref);
    }

    pub fn finish_turn(&self, completed: bool) {
        if !self.is_active() {
            return;
        }
        settle_items(&self.store, &self.sink, &self.id, completed);
        let execution = self.plan_execution.locked().take();
        if let Some((plan_id, revision)) = execution
            && let Ok(Some(plan)) = self.store.set_plan_execution(
                &self.id,
                &plan_id,
                revision,
                if completed {
                    PlanExecutionStatus::Completed
                } else {
                    PlanExecutionStatus::Failed
                },
            )
        {
            self.publish_plan(plan);
        }
        self.report_workspace_changes();
        self.set_state(ChatRunState::Ready, None);
    }

    pub fn fail(&self, message: String) {
        if !self.active.swap(false, Ordering::AcqRel) {
            return;
        }
        settle_items(&self.store, &self.sink, &self.id, false);
        self.settle_subagents();
        self.expire_plans();
        self.transition(ChatRunState::Error, Some(message), true);
    }

    pub fn stop(&self) {
        if !self.active.swap(false, Ordering::AcqRel) {
            return;
        }
        settle_items(&self.store, &self.sink, &self.id, false);
        self.settle_subagents();
        self.expire_plans();
        self.transition(ChatRunState::Stopped, None, true);
    }

    pub fn waiting(&self) {
        if self.is_active() {
            self.set_state(ChatRunState::Waiting, None);
        }
    }

    fn report_workspace_changes(&self) {
        let Some(base) = self.baseline.locked().take() else {
            return;
        };
        let Some(head) = crate::workspace_diff::snapshot(&self.root) else {
            return;
        };
        let Ok(items) = self.store.chat_items(&self.id) else {
            return;
        };
        let turn_start = items
            .iter()
            .rposition(|item| item.kind == ChatItemKind::UserMessage)
            .unwrap_or(0);
        let reported: std::collections::HashSet<&str> = items[turn_start..]
            .iter()
            .filter(|item| {
                item.kind == ChatItemKind::FileChange && !item.id.starts_with("workspace-diff:")
            })
            .flat_map(|item| item.files.iter().map(String::as_str))
            .collect();
        let changes: Vec<_> = crate::workspace_diff::diff(&self.root, &base, &head)
            .into_iter()
            .filter(|change| !reported.contains(change.path.as_str()))
            .collect();
        if changes.is_empty() {
            return;
        }
        let anchor = items
            .get(turn_start)
            .map_or("turn", |item| item.id.as_str());
        let mut item = blank_item(
            &format!("workspace-diff:{anchor}"),
            ChatItemKind::FileChange,
            ChatItemStatus::Completed,
        );
        item.title = "Workspace changes".into();
        item.action = Some(ToolAction::Edit);
        item.files = changes.iter().map(|change| change.path.clone()).collect();
        item.subject = item.files.join(", ");
        item.changes = changes;
        let _ = self.upsert(item);
    }
}

pub fn spawn(spec: RuntimeSpec, ctx: Arc<ChatContext>) -> Result<Arc<dyn LiveSession>> {
    let agent = AgentKind::parse(&spec.provider)?;
    Ok(super::acp::AcpSession::spawn(agent, spec, ctx)?)
}

pub fn blank_item(id: &str, kind: ChatItemKind, status: ChatItemStatus) -> ChatItem {
    ChatItem {
        id: id.to_string(),
        ordinal: 0,
        revision: 0,
        turn_id: String::new(),
        kind,
        status,
        title: String::new(),
        text: String::new(),
        detail: String::new(),
        request_id: None,
        questions: vec![],
        attachments: vec![],
        started_at: 0,
        updated_at: 0,
        action: None,
        subject: String::new(),
        files: vec![],
        approval: None,
        changes: vec![],
        task_id: None,
    }
}

pub fn approval_item(
    id: &str,
    request_id: &str,
    kind: &str,
    title: &str,
    text: &str,
    decisions: &[(&str, &str)],
) -> ChatItem {
    let mut item = blank_item(id, ChatItemKind::ApprovalRequest, ChatItemStatus::Waiting);
    item.title = title.into();
    item.text = text.into();
    item.request_id = Some(request_id.into());
    item.approval = Some(crate::model::ApprovalView {
        kind: kind.into(),
        decisions: decisions
            .iter()
            .map(|(decision, label)| crate::model::ApprovalDecisionView {
                decision: (*decision).into(),
                label: (*label).into(),
                warning: None,
            })
            .collect(),
    });
    item
}

pub fn question_item(
    id: &str,
    request_id: &str,
    title: &str,
    questions: Vec<ChatQuestion>,
) -> ChatItem {
    let mut item = blank_item(id, ChatItemKind::UserInputRequest, ChatItemStatus::Waiting);
    item.title = title.into();
    item.request_id = Some(request_id.into());
    item.questions = questions;
    item
}

pub fn system_item(id: &str, text: &str, status: ChatItemStatus) -> ChatItem {
    let mut item = blank_item(id, ChatItemKind::SystemNotice, status);
    item.title = "Notice".into();
    item.text = text.into();
    item
}

pub fn error_item(id: &str, text: &str) -> ChatItem {
    let mut item = blank_item(id, ChatItemKind::Error, ChatItemStatus::Failed);
    item.title = "Error".into();
    item.text = text.into();
    item
}

pub fn file_changes(
    root: &Path,
    path: &str,
    old: Option<&str>,
    new: Option<&str>,
    patch: Option<&str>,
) -> Vec<crate::model::FileChangeView> {
    let Some(relative) = crate::agent_runtime::relativize(path, root) else {
        return vec![];
    };
    let kind = match (old, new) {
        (None, Some(_)) => crate::model::ChangeKind::Added,
        (Some(_), None) => crate::model::ChangeKind::Deleted,
        _ => crate::model::ChangeKind::Modified,
    };
    vec![crate::model::FileChangeView {
        path: relative,
        origin: crate::model::ChangeOrigin::Provider,
        kind,
        additions: new.map(|text| text.lines().count() as u32),
        deletions: old.map(|text| text.lines().count() as u32),
        patch: patch.map(str::to_string).or_else(|| {
            Some(whole_file_patch(
                path,
                old.unwrap_or_default(),
                new.unwrap_or_default(),
            ))
        }),
    }]
}

fn whole_file_patch(path: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut out = format!(
        "--- a/{path}\n+++ b/{path}\n@@ -{},{} +{},{} @@\n",
        u32::from(!old_lines.is_empty()),
        old_lines.len(),
        u32::from(!new_lines.is_empty()),
        new_lines.len()
    );
    for line in &old_lines {
        out.push('-');
        out.push_str(line);
        out.push('\n');
    }
    for line in &new_lines {
        out.push('+');
        out.push_str(line);
        out.push('\n');
    }
    out
}

pub fn announce_ready(ctx: &ChatContext) {
    if ctx.state().state == ChatRunState::Starting && ctx.is_active() {
        ctx.set_state(ChatRunState::Ready, None);
    }
}

pub fn provider_missing(provider: &str, error: std::io::Error) -> Error {
    Error::Unavailable(format!(
        "Could not start {provider}: {error}. Install it from Setup and check its login."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        PlanReviewKind, PlanReviewStatus, Session, SessionMode, SessionStatus, Workspace,
    };

    #[test]
    fn approved_plan_execution_settles_on_the_next_turn_completion() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Arc::new(Store::in_memory()?);
        store.insert_workspace(&Workspace {
            id: "w1".into(),
            name: "workspace".into(),
            path: dir.path().display().to_string(),
            model_overrides: vec![],
            archived: false,
            created_at: 1,
            updated_at: 1,
        })?;
        store.insert_session(&Session {
            id: "s1".into(),
            workspace_id: "w1".into(),
            worktree: None,
            name: "chat".into(),
            agent: Some(AgentKind::Codex),
            mode: SessionMode::Ui,
            archived: false,
            starred: false,
            created_at: 1,
            updated_at: 1,
            last_started_at: None,
            status: SessionStatus::Stopped,
        })?;
        let now = crate::util::now_millis();
        let review = store.record_plan_review(
            "s1",
            PlanReview {
                plan_id: "p1".into(),
                agent: AgentKind::Codex,
                session_id: "s1".into(),
                revision: 0,
                title: "Plan".into(),
                content: "# Plan".into(),
                content_hash: "hash".into(),
                complete: true,
                review_kind: PlanReviewKind::NextTurn,
                native_plan_id: Some("native".into()),
                exec_permission: ChatPermissionMode::Auto,
                status: PlanReviewStatus::AwaitingReview,
                execution: PlanExecutionStatus::NotStarted,
                decision: None,
                feedback: None,
                decision_intent_at: None,
                applied_at: None,
                created_at: now,
                updated_at: now,
                superseded_by: None,
            },
        )?;
        let ctx = ChatContext::new(
            "s1",
            dir.path().to_path_buf(),
            store.clone(),
            Arc::new(RwLock::new(None)),
            ChatPermissionMode::Auto,
        );
        ctx.begin_plan_execution(review.plan_id.clone(), review.revision);
        ctx.finish_turn(true);
        assert_eq!(
            store
                .chat_plan("s1", &review.plan_id, review.revision)?
                .unwrap()
                .execution,
            PlanExecutionStatus::Completed
        );
        Ok(())
    }
}
