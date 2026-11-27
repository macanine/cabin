//! One live chat over a native provider session. Rust owns the durable turn
//! items and run state; providers only translate their wire protocol into
//! `ChatItem` snapshots.

use crate::util::LockExt;
use std::sync::{Arc, Mutex, RwLock};

use super::context::{self, ChatContext, LiveSession, TurnInput};
use crate::chat::{Sink, emit, pending_message};
use crate::error::{Error, Result};
use crate::model::{
    AppEvent, ChatAsyncTask, ChatAttachment, ChatItem, ChatItemKind, ChatItemStatus,
    ChatPermissionMode, ChatRunState, ChatState, PlanDecision, PlanReview, SessionStatus,
};
use crate::store::Store;

pub use super::context::RuntimeSpec;

fn start_prompt(item: &mut crate::model::ChatItem) {
    // A prompt always starts a turn. A steering item that arrived too late is
    // reused here, so replace rather than preserve its old turn id.
    item.turn_id = item.id.clone();
}

fn approval_decision<'a>(item: Option<&ChatItem>, decision: &'a str) -> Result<&'a str> {
    let normalized = match decision {
        "allow" => "accept",
        "allow_session" => "acceptForSession",
        "deny" => "decline",
        other => other,
    };
    let base = normalized
        .split_once(':')
        .map_or(normalized, |(base, _)| base);
    if !matches!(
        base,
        "accept" | "acceptForSession" | "acceptAlways" | "decline" | "declineAlways" | "cancel"
    ) {
        return Err(Error::invalid("Unknown approval decision"));
    }
    let offered = item
        .and_then(|item| item.approval.as_ref())
        .is_some_and(|approval| {
            approval
                .decisions
                .iter()
                .any(|decision| decision.decision == normalized)
        });
    if !offered {
        return Err(Error::invalid(format!(
            "This request does not offer the decision \"{normalized}\""
        )));
    }
    Ok(normalized)
}

pub struct ChatSession {
    ctx: Arc<ChatContext>,
    runtime: Mutex<Option<Arc<dyn LiveSession>>>,
    model: Mutex<Option<String>>,
    prompt: Mutex<()>,
}

impl ChatSession {
    pub fn start(
        id: &str,
        store: Arc<Store>,
        sink: Arc<RwLock<Option<Sink>>>,
        spec: RuntimeSpec,
    ) -> Result<Arc<Self>> {
        let ctx = ChatContext::new(id, spec.cwd.clone(), store, sink, spec.permission);
        let chat = Arc::new(Self {
            ctx: ctx.clone(),
            runtime: Mutex::new(None),
            model: Mutex::new(spec.model.clone()),
            prompt: Mutex::new(()),
        });
        emit(
            &chat.ctx.sink,
            AppEvent::SessionStatus {
                id: id.into(),
                status: SessionStatus::Starting,
            },
        );
        let worker = chat.clone();
        if let Err(error) = std::thread::Builder::new().spawn(move || worker.open(spec)) {
            chat.ctx
                .fail(format!("Could not start the chat worker: {error}"));
        }
        Ok(chat)
    }

    fn open(self: Arc<Self>, spec: RuntimeSpec) {
        let runtime = match context::spawn(spec.clone(), self.ctx.clone()) {
            Ok(runtime) => runtime,
            Err(error) => {
                self.ctx.fail(error.to_string());
                return;
            }
        };
        {
            let mut slot = self.runtime.locked();
            if !self.ctx.is_active() {
                drop(slot);
                runtime.stop();
                return;
            }
            *slot = Some(runtime);
        }
        if spec.prompt.is_some() || !spec.attachments.is_empty() {
            if let Err(error) = self.send_prepared(
                spec.prompt.as_deref().unwrap_or(""),
                spec.prompt_id,
                spec.attachments,
                ChatRunState::Starting,
            ) {
                self.fail(error.to_string());
            }
        } else if self.ctx.state().state == ChatRunState::Starting {
            self.ctx.set_state(ChatRunState::Ready, None);
        }
    }

    pub fn set_model(&self, model: Option<String>) {
        *self.model.locked() = model.clone();
        if let Ok(runtime) = self.runtime() {
            let _ = runtime.set_model(model);
        }
    }

    pub fn status(&self) -> SessionStatus {
        let state = self.ctx.state();
        match state.state {
            ChatRunState::Starting => SessionStatus::Starting,
            ChatRunState::Stopped => SessionStatus::Stopped,
            ChatRunState::Error => SessionStatus::Failed {
                message: state.message.unwrap_or_else(|| "The agent stopped".into()),
            },
            _ => SessionStatus::Running { pid: self.pid() },
        }
    }

    pub fn state(&self) -> ChatState {
        self.ctx.state()
    }

    pub fn fail(&self, message: String) {
        self.ctx.fail(message);
        if let Some(runtime) = self.runtime.locked().take() {
            runtime.stop();
        }
    }

    fn pid(&self) -> u32 {
        self.runtime
            .locked()
            .as_ref()
            .map_or(0, |runtime| runtime.pid())
    }

    fn runtime(&self) -> Result<Arc<dyn LiveSession>> {
        self.runtime
            .locked()
            .clone()
            .ok_or_else(|| Error::Unavailable("Conversation is not running".into()))
    }

    pub fn send(&self, text: &str, attachments: Vec<ChatAttachment>) -> Result<()> {
        let text = text.trim();
        if text.is_empty() && attachments.is_empty() {
            return Err(Error::invalid("Message is empty"));
        }
        self.send_prepared(text, None, attachments, ChatRunState::Ready)
    }

    fn send_prepared(
        &self,
        text: &str,
        existing: Option<String>,
        attachments: Vec<ChatAttachment>,
        expected: ChatRunState,
    ) -> Result<()> {
        let _prompt = self.prompt.locked();
        if self.state().state != expected || !self.ctx.is_active() {
            return Err(Error::Conflict(
                "The agent is not ready for another message".into(),
            ));
        }
        let runtime = self.runtime()?;
        let mut item = match existing {
            Some(id) => self
                .ctx
                .store
                .chat_item(&self.ctx.id, &id)?
                .ok_or_else(|| Error::NotFound("pending chat message".into()))?,
            None => pending_message(text, attachments.clone()),
        };
        item.status = ChatItemStatus::Pending;
        start_prompt(&mut item);
        let item = self.ctx.upsert(item)?;
        self.ctx.begin_turn();
        let model = self.model.locked().clone();
        let input = TurnInput {
            id: item.id.clone(),
            text: text.to_string(),
            attachments,
            model,
        };
        let ctx = self.ctx.clone();
        let item_id = item.id.clone();
        if let Err(error) = std::thread::Builder::new().spawn(move || {
            let outcome = runtime.send(input);
            let _ = ctx.update_item(&item.id, |item| match &outcome {
                Ok(()) => item.status = ChatItemStatus::Completed,
                Err(error) => {
                    item.status = ChatItemStatus::Failed;
                    item.detail = error.to_string();
                }
            });
            if let Err(error) = outcome {
                ctx.fail(error.to_string());
                runtime.stop();
            }
        }) {
            let message = format!("Could not start the prompt worker: {error}");
            let _ = self.ctx.update_item(&item_id, |item| {
                item.status = ChatItemStatus::Failed;
                item.detail = message.clone();
            });
            self.fail(message);
        }
        Ok(())
    }

    /// Send a message while a turn runs. It joins the running turn when the
    /// agent supports steering; if the turn ended first it becomes a new prompt.
    pub fn steer(self: &Arc<Self>, text: &str, attachments: Vec<ChatAttachment>) -> Result<()> {
        let text = text.trim();
        if text.is_empty() && attachments.is_empty() {
            return Err(Error::invalid("Message is empty"));
        }
        match self.state().state {
            ChatRunState::Ready => {
                return self.send_prepared(text, None, attachments, ChatRunState::Ready);
            }
            ChatRunState::Running | ChatRunState::Waiting => {}
            _ => {
                return Err(Error::Conflict("The agent is not running".into()));
            }
        }
        let runtime = self.runtime()?;
        let mut item = pending_message(text, attachments.clone());
        item.status = ChatItemStatus::Pending;
        // A delivered steering message is part of the turn it interrupts.
        item.turn_id = self.ctx.current_turn();
        let item = self.ctx.upsert(item)?;
        let input = TurnInput {
            id: item.id.clone(),
            text: text.to_string(),
            attachments: attachments.clone(),
            model: None,
        };
        let chat = self.clone();
        let text = text.to_string();
        let item_id = item.id.clone();
        if let Err(error) = std::thread::Builder::new().spawn(move || match runtime.steer(input) {
            Ok(context::SteerOutcome::Delivered) => {
                let _ = chat.ctx.update_item(&item.id, |item| {
                    item.status = ChatItemStatus::Completed;
                    item.title = "Steered".into();
                });
            }
            Ok(context::SteerOutcome::PromptRequired) => {
                // The turn ended first: wait for the chat to settle, then prompt.
                for _ in 0..600 {
                    if !matches!(
                        chat.state().state,
                        ChatRunState::Running | ChatRunState::Waiting
                    ) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                if let Err(error) = chat.send_prepared(
                    &text,
                    Some(item.id.clone()),
                    attachments,
                    ChatRunState::Ready,
                ) {
                    let _ = chat.ctx.update_item(&item.id, |item| {
                        item.status = ChatItemStatus::Failed;
                        item.detail = error.to_string();
                    });
                }
            }
            Err(error) => {
                let _ = chat.ctx.update_item(&item.id, |item| {
                    item.status = ChatItemStatus::Failed;
                    item.detail = error.to_string();
                });
            }
        }) {
            let _ = self.ctx.update_item(&item_id, |item| {
                item.status = ChatItemStatus::Failed;
                item.detail = format!("Could not start the steering worker: {error}");
            });
        }
        Ok(())
    }

    pub fn set_option(&self, option: &str, value: &str) -> Result<()> {
        self.runtime()?.set_option(option, value)
    }

    pub fn controls(&self) -> crate::model::ChatControls {
        self.ctx.controls()
    }

    pub fn tasks(&self) -> Vec<ChatAsyncTask> {
        self.ctx.tasks()
    }

    pub fn stop_task(&self, task_id: &str) -> Result<bool> {
        let stopped = self.runtime()?.stop_task(task_id)?;
        if stopped && let Some(mut task) = self.ctx.task(task_id) {
            task.state = "stopped".into();
            task.can_stop = false;
            task.updated_at = crate::util::now_millis();
            self.ctx.upsert_task(task);
        }
        Ok(stopped)
    }

    pub fn goal_control(&self, action: &str) -> Result<()> {
        self.runtime()?.goal_control(action)
    }

    pub fn respond(
        &self,
        request_id: &str,
        decision: &str,
        answers: serde_json::Value,
    ) -> Result<()> {
        let items = self.ctx.store.chat_items(&self.ctx.id)?;
        let item = items
            .iter()
            .find(|item| item.request_id.as_deref() == Some(request_id));
        if item.is_some_and(|item| item.kind == ChatItemKind::UserInputRequest) {
            return self.runtime()?.respond(request_id, decision, &answers);
        }
        let normalized = approval_decision(item, decision)?;
        self.runtime()?.respond(request_id, normalized, &answers)
    }

    pub fn respond_plan(
        &self,
        review: &PlanReview,
        decision: PlanDecision,
        feedback: Option<&str>,
    ) -> Result<()> {
        self.runtime()?.respond_plan(review, decision, feedback)
    }

    pub fn begin_plan_execution(&self, plan_id: String, revision: i64) {
        self.ctx.begin_plan_execution(plan_id, revision);
    }

    pub fn clear_plan_execution(&self) {
        self.ctx.clear_plan_execution();
    }

    pub fn interrupt(&self) -> Result<()> {
        self.runtime()?.interrupt()
    }

    pub fn set_permission(&self, permission: ChatPermissionMode) -> Result<()> {
        self.runtime()?.set_permission(permission)?;
        self.ctx.set_permission(permission);
        Ok(())
    }

    pub fn stop(&self) {
        self.ctx.stop();
        let runtime = self.runtime.locked().take();
        if let Some(runtime) = runtime {
            runtime.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AgentKind, ChatItemStatus, Session, SessionMode, SessionStatus, Workspace};

    fn context(root: &std::path::Path) -> crate::error::Result<Arc<ChatContext>> {
        let store = Arc::new(Store::in_memory()?);
        store.insert_workspace(&Workspace {
            id: "w1".into(),
            name: "workspace".into(),
            path: root.display().to_string(),
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
        Ok(ChatContext::new(
            "s1",
            root.to_path_buf(),
            store,
            Arc::new(RwLock::new(None)),
            ChatPermissionMode::Auto,
        ))
    }

    #[test]
    fn steering_stays_in_its_turn_and_a_late_steer_starts_a_new_one() -> crate::error::Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path())?;

        let mut prompt = pending_message("first", vec![]);
        start_prompt(&mut prompt);
        let prompt = ctx.upsert(prompt)?;

        let mut steering = pending_message("steer", vec![]);
        steering.turn_id = ctx.current_turn();
        let steering = ctx.upsert(steering)?;
        assert_eq!(steering.turn_id, prompt.turn_id);
        assert_eq!(ctx.current_turn(), prompt.turn_id);

        let mut work = crate::agent_runtime::context::blank_item(
            "assistant-1",
            ChatItemKind::AssistantMessage,
            ChatItemStatus::Completed,
        );
        work.turn_id.clear();
        let work = ctx.upsert(work)?;
        assert_eq!(work.turn_id, prompt.turn_id);

        let mut late = steering;
        start_prompt(&mut late);
        let late = ctx.upsert(late)?;
        assert_eq!(late.turn_id, late.id);
        assert_ne!(late.turn_id, prompt.turn_id);
        assert_eq!(ctx.current_turn(), late.id);
        Ok(())
    }
}
#[test]
fn approval_requires_the_exact_offered_option() -> Result<()> {
    let item = context::approval_item(
        "approval-1",
        "request-1",
        "plan",
        "Review",
        "Plan",
        &[
            ("accept", "Approve"),
            ("accept:with:note", "Approve with note"),
        ],
    );
    assert_eq!(
        approval_decision(Some(&item), "accept:with:note")?,
        "accept:with:note"
    );
    assert_eq!(approval_decision(Some(&item), "allow")?, "accept");
    assert!(approval_decision(Some(&item), "accept:unknown").is_err());
    Ok(())
}
