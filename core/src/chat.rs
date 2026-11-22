//! Supervision of chat sessions. Every chat runs on a native provider process
//! (see `agent_runtime`); Rust owns the durable state. This module keeps the
//! map of live chats and the shared helpers.
use crate::util::{LockExt, RwLockExt};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;

use crate::agent_runtime::session::{ChatSession, RuntimeSpec};
use crate::error::{Error, Result};
use crate::model::{
    AppEvent, ChatAttachment, ChatItem, ChatItemKind, ChatItemStatus, ChatPermissionMode,
    ChatRunState, ChatSnapshot, ChatState, PlanDecision, PlanExecutionStatus, PlanRespondOutcome,
    PlanRespondResult, SessionStatus,
};
use crate::store::PlanClaim;
use crate::store::Store;

pub(crate) type Sink = Arc<dyn Fn(AppEvent) + Send + Sync>;

pub fn pending_message(text: &str, attachments: Vec<ChatAttachment>) -> ChatItem {
    ChatItem {
        id: crate::util::new_id("msg"),
        ordinal: 0,
        revision: 0,
        turn_id: String::new(),
        kind: ChatItemKind::UserMessage,
        status: ChatItemStatus::Pending,
        title: String::new(),
        text: text.to_string(),
        detail: String::new(),
        request_id: None,
        questions: vec![],
        attachments,
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

pub struct ChatSupervisor {
    store: Arc<Store>,
    sink: Arc<RwLock<Option<Sink>>>,
    failures: Mutex<HashMap<String, String>>,
    live: Mutex<HashMap<String, Arc<ChatSession>>>,
}

pub(crate) fn emit(sink: &RwLock<Option<Sink>>, event: AppEvent) {
    let handler = sink.read_lock().clone();
    if let Some(handler) = handler {
        handler(event);
    }
}

/// Anything still waiting or running when a chat ends is closed out, so the timeline never shows a
/// request nobody can answer.
pub(crate) fn settle_items(store: &Store, sink: &RwLock<Option<Sink>>, id: &str, completed: bool) {
    if let Ok(items) = store.chat_items(id) {
        for mut item in items {
            if item.status == ChatItemStatus::Waiting {
                item.status = ChatItemStatus::Cancelled;
                item.detail = "The request is no longer active".into();
            } else if item.status == ChatItemStatus::Running {
                item.status = if completed
                    && matches!(
                        item.kind,
                        ChatItemKind::AssistantMessage
                            | ChatItemKind::Reasoning
                            | ChatItemKind::ProposedPlan
                    ) {
                    ChatItemStatus::Completed
                } else {
                    ChatItemStatus::Interrupted
                };
            } else {
                continue;
            }
            if let Ok(item) = store.put_chat_item(id, item) {
                emit(
                    sink,
                    AppEvent::ChatItem {
                        id: id.into(),
                        item,
                    },
                );
            }
        }
    }
}

fn settle_inactive(store: &Store, sink: &RwLock<Option<Sink>>, id: &str) {
    settle_items(store, sink, id, false);
    if let Ok(plans) = store.expire_open_plans(id) {
        for plan in plans {
            emit(
                sink,
                AppEvent::ChatPlan {
                    id: id.into(),
                    plan,
                },
            );
        }
    }
    if let Ok(tasks) = store.settle_active_subagents(id) {
        for task in tasks {
            emit(
                sink,
                AppEvent::ChatSubagent {
                    id: id.into(),
                    task,
                },
            );
        }
    }
}

impl ChatSupervisor {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            sink: Arc::new(RwLock::new(None)),
            failures: Mutex::new(HashMap::new()),
            live: Mutex::new(HashMap::new()),
        }
    }
    pub fn set_sink(&self, sink: impl Fn(AppEvent) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }
    fn live(&self, id: &str) -> Option<Arc<ChatSession>> {
        self.live.locked()
            .get(id)
            .cloned()
    }

    /// Start (or restart) a chat on its provider process.
    pub fn start(&self, id: &str, spec: RuntimeSpec) -> Result<()> {
        if !spec.cwd.is_dir() {
            return Err(Error::invalid(format!(
                "The workspace folder {} does not exist",
                spec.cwd.display()
            )));
        }
        if self.live(id).is_some_and(|chat| chat.status().is_live()) {
            return Err(Error::Conflict("This session is already running".into()));
        }
        if let Some(old) = self
            .live.locked()
            .remove(id)
        {
            old.stop();
        }
        self.failures.locked()
            .remove(id);
        let chat = ChatSession::start(id, self.store.clone(), self.sink.clone(), spec)?;
        self.live.locked()
            .insert(id.into(), chat);
        Ok(())
    }

    pub fn status(&self, id: &str) -> SessionStatus {
        if let Some(chat) = self.live(id) {
            return chat.status();
        }
        self.failures.locked()
            .get(id)
            .map(|message| SessionStatus::Failed {
                message: message.clone(),
            })
            .unwrap_or(SessionStatus::Stopped)
    }

    /// Mark a chat failed (used when it could not even start).
    pub fn fail(&self, id: &str, message: String) {
        if let Some(chat) = self.live(id) {
            chat.fail(message);
            return;
        }
        self.failures.locked()
            .insert(id.into(), message.clone());
        emit(
            &self.sink,
            AppEvent::SessionStatus {
                id: id.into(),
                status: SessionStatus::Failed {
                    message: message.clone(),
                },
            },
        );
        emit(
            &self.sink,
            AppEvent::ChatState {
                id: id.into(),
                state: ChatState {
                    state: ChatRunState::Error,
                    message: Some(message),
                },
            },
        );
    }

    pub fn snapshot(&self, id: &str) -> Result<ChatSnapshot> {
        let state = match self.live(id) {
            Some(chat) => chat.state(),
            None => match self.status(id) {
                SessionStatus::Failed { message } => ChatState {
                    state: ChatRunState::Error,
                    message: Some(message),
                },
                _ => ChatState {
                    state: ChatRunState::Stopped,
                    message: None,
                },
            },
        };
        if matches!(state.state, ChatRunState::Stopped | ChatRunState::Error) {
            settle_inactive(&self.store, &self.sink, id);
        }
        Ok(ChatSnapshot {
            items: self.store.chat_items(id)?,
            plans: self.store.chat_plans(id)?,
            subagents: self.store.chat_subagents(id)?,
            tasks: self.live(id).map(|chat| chat.tasks()).unwrap_or_default(),
            state,
            model: self.store.chat_model(id)?,
            permission: self.store.chat_permission(id)?,
            usage: self.store.chat_usage(id)?,
            controls: self
                .live(id)
                .map(|chat| chat.controls())
                .unwrap_or_default(),
        })
    }

    pub fn send(&self, id: &str, text: &str, attachments: Vec<ChatAttachment>) -> Result<()> {
        let chat = self
            .live(id)
            .ok_or_else(|| Error::Unavailable("Start the conversation first".into()))?;
        chat.send(text, attachments)
    }

    /// A message for the running turn; a new prompt when the agent is idle.
    pub fn steer(&self, id: &str, text: &str, attachments: Vec<ChatAttachment>) -> Result<()> {
        let chat = self
            .live(id)
            .ok_or_else(|| Error::Unavailable("Start the conversation first".into()))?;
        chat.steer(text, attachments)
    }

    pub fn stop_task(&self, id: &str, task_id: &str) -> Result<bool> {
        let chat = self
            .live(id)
            .ok_or_else(|| Error::Unavailable("Start the conversation first".into()))?;
        chat.stop_task(task_id)
    }

    pub fn goal_control(&self, id: &str, action: &str) -> Result<()> {
        let chat = self
            .live(id)
            .ok_or_else(|| Error::Unavailable("Start the conversation first".into()))?;
        chat.goal_control(action)
    }

    /// Change an agent config option (model, mode, thought level, …).
    pub fn set_option(&self, id: &str, option: &str, value: &str) -> Result<()> {
        let chat = self
            .live(id)
            .ok_or_else(|| Error::Unavailable("Start the conversation first".into()))?;
        chat.set_option(option, value)
    }

    /// The provider takes the model with the next turn; the choice is
    /// persisted now, and a live agent is switched at once when it accepts
    /// the change or with the next prompt otherwise.
    pub fn set_model(
        &self,
        id: &str,
        selection: Option<String>,
        effective: Option<String>,
    ) -> Result<()> {
        if let Some(chat) = self.live(id) {
            chat.set_model(effective);
        }
        self.store.set_chat_model(id, selection.as_deref())?;
        emit(
            &self.sink,
            AppEvent::ChatModel {
                id: id.into(),
                model: selection,
            },
        );
        Ok(())
    }

    pub fn set_permission(&self, id: &str, permission: ChatPermissionMode) -> Result<()> {
        if let Some(chat) = self.live(id)
            && chat.state().state == ChatRunState::Ready
        {
            chat.set_permission(permission)?;
        }
        self.store.set_chat_permission(id, permission)?;
        emit(
            &self.sink,
            AppEvent::ChatPermission {
                id: id.into(),
                permission,
            },
        );
        Ok(())
    }

    pub fn respond(
        &self,
        id: &str,
        request_id: &str,
        decision: &str,
        answers: Value,
    ) -> Result<()> {
        self.live(id)
            .ok_or_else(|| Error::Unavailable("Conversation is not running".into()))?
            .respond(request_id, decision, answers)
    }

    pub fn respond_plan(
        &self,
        id: &str,
        plan_id: &str,
        expected_revision: i64,
        decision: PlanDecision,
        feedback: Option<String>,
    ) -> Result<PlanRespondResult> {
        let claim = self.store.claim_plan_decision(
            id,
            plan_id,
            expected_revision,
            decision,
            feedback.as_deref(),
        )?;
        let (review, previous) = match claim {
            PlanClaim::Conflict(review) => {
                emit(
                    &self.sink,
                    AppEvent::ChatPlan {
                        id: id.into(),
                        plan: review.clone(),
                    },
                );
                return Ok(PlanRespondResult {
                    outcome: PlanRespondOutcome::Conflict,
                    review,
                });
            }
            PlanClaim::Claimed { review, previous } => (review, previous),
        };
        emit(
            &self.sink,
            AppEvent::ChatPlan {
                id: id.into(),
                plan: review.clone(),
            },
        );
        if decision != PlanDecision::Defer {
            let Some(chat) = self.live(id) else {
                let _ = self
                    .store
                    .revert_plan_submission(id, plan_id, expected_revision, previous);
                if let Ok(Some(review)) = self.store.chat_plan(id, plan_id, expected_revision) {
                    emit(
                        &self.sink,
                        AppEvent::ChatPlan {
                            id: id.into(),
                            plan: review,
                        },
                    );
                }
                return Err(Error::Unavailable(
                    "Reconnect this chat before approving its plan".into(),
                ));
            };
            if decision == PlanDecision::Approve {
                chat.begin_plan_execution(plan_id.to_string(), expected_revision);
            }
            if let Err(error) = chat.respond_plan(&review, decision, feedback.as_deref()) {
                chat.clear_plan_execution();
                let _ = self
                    .store
                    .revert_plan_submission(id, plan_id, expected_revision, previous);
                if let Ok(Some(review)) = self.store.chat_plan(id, plan_id, expected_revision) {
                    emit(
                        &self.sink,
                        AppEvent::ChatPlan {
                            id: id.into(),
                            plan: review,
                        },
                    );
                }
                return Err(error);
            }
        }
        let execution = if decision == PlanDecision::Approve {
            PlanExecutionStatus::Starting
        } else {
            PlanExecutionStatus::NotStarted
        };
        let review = self
            .store
            .finish_plan_decision(id, plan_id, expected_revision, decision, execution)?
            .ok_or_else(|| Error::NotFound("plan review".into()))?;
        emit(
            &self.sink,
            AppEvent::ChatPlan {
                id: id.into(),
                plan: review.clone(),
            },
        );
        Ok(PlanRespondResult {
            outcome: PlanRespondOutcome::Applied,
            review,
        })
    }

    pub fn interrupt(&self, id: &str) -> Result<()> {
        self.live(id)
            .ok_or_else(|| Error::Unavailable("Conversation is not running".into()))?
            .interrupt()
    }

    pub fn stop(&self, id: &str) -> Result<()> {
        if let Some(chat) = self.live(id) {
            chat.stop();
        }
        Ok(())
    }

    pub fn forget(&self, id: &str) {
        let _ = self.stop(id);
        self.live.locked()
            .remove(id);
    }

    pub fn stop_all(&self) {
        let chats: Vec<_> = self
            .live.locked()
            .values()
            .cloned()
            .collect();
        for chat in chats {
            chat.stop();
        }
    }

    /// Fail chats that are waiting on a provider. Used when the host closes
    /// the machine's internet; otherwise a retrying provider can leave the
    /// chat looking busy forever.
    pub fn fail_active(&self, message: &str) {
        let chats: Vec<_> = self
            .live.locked()
            .values()
            .cloned()
            .collect();
        for chat in chats {
            if matches!(
                chat.state().state,
                ChatRunState::Starting | ChatRunState::Running | ChatRunState::Waiting
            ) {
                chat.fail(message.to_string());
            }
        }
    }
}
