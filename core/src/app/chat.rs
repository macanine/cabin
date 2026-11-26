use base64::Engine as _;

use crate::agents;
use crate::error::{Error, Result};
use crate::model::{
    AgentKind, AppEvent, ChatAttachmentData, ChatAttachmentInput, ChatPermissionMode, ChatSnapshot,
    PlanDecision, PlanRespondResult, SessionMode,
};

use super::{App, chat_permission_modes};

impl App {
    /// Per-agent chat capabilities for the UI (permission picker).
    pub fn agent_chat_capabilities(&self) -> Vec<crate::model::AgentChatCapabilities> {
        AgentKind::ALL
            .into_iter()
            .map(|agent| crate::model::AgentChatCapabilities {
                agent,
                permission_modes: chat_permission_modes(agent),
            })
            .collect()
    }

    pub fn chat_snapshot(&self, id: &str) -> Result<ChatSnapshot> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        self.chats.snapshot(id)
    }

    pub fn chat_stop_task(&self, id: &str, task_id: &str) -> Result<bool> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        self.chats.stop_task(id, task_id)
    }

    pub fn chat_goal_control(&self, id: &str, action: &str) -> Result<()> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        if !matches!(action, "pause" | "clear") {
            return Err(Error::invalid("Unknown goal action"));
        }
        self.chats.goal_control(id, action)
    }

    pub fn chat_set_model(&self, id: &str, model: Option<String>) -> Result<()> {
        let session = self.ctx.store.session(id)?;
        if session.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        let agent = session
            .agent
            .ok_or_else(|| Error::invalid("Chat requires an agent"))?;
        let model = model
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if model
            .as_ref()
            .is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(Error::invalid("Invalid model name"));
        }
        let workspace = self.ctx.store.workspace(&session.workspace_id)?;
        let effective = model
            .clone()
            .or_else(|| workspace.model_for(agent).map(str::to_string));
        self.chats.set_model(id, model, effective)
    }

    pub fn chat_set_permission(&self, id: &str, permission: ChatPermissionMode) -> Result<()> {
        let session = self.ctx.store.session(id)?;
        if session.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        let agent = session
            .agent
            .ok_or_else(|| Error::invalid("Not an agent chat"))?;
        if !chat_permission_modes(agent).contains(&permission) {
            return Err(Error::invalid(format!(
                "{} does not support that chat permission",
                agents::adapter(agent).spec().name
            )));
        }
        self.chats.set_permission(id, permission)
    }

    pub fn chat_attachment(&self, id: &str, attachment_id: &str) -> Result<ChatAttachmentData> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        let file = self
            .ctx
            .store
            .chat_items(id)?
            .into_iter()
            .flat_map(|item| item.attachments)
            .find(|file| file.id == attachment_id)
            .ok_or_else(|| Error::NotFound("attachment".into()))?;
        let root = std::fs::canonicalize(self.attachment_dir(id)?)
            .map_err(|error| Error::io("read attachments", error))?;
        let path = std::fs::canonicalize(&file.path)
            .map_err(|error| Error::io("read attachment", error))?;
        if !path.starts_with(root) {
            return Err(Error::invalid("Invalid attachment location"));
        }
        let meta = std::fs::metadata(&path).map_err(|error| Error::path("read", &path, error))?;
        if !meta.is_file() || meta.len() > 8 * 1024 * 1024 {
            return Err(Error::invalid("Invalid attachment"));
        }
        let bytes = std::fs::read(&path).map_err(|error| Error::path("read", &path, error))?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(Error::invalid("Invalid attachment"));
        }
        Ok(ChatAttachmentData {
            mime_type: file.mime_type,
            base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }

    /// Send a message into the running turn (steering).
    pub fn chat_steer(
        &self,
        id: &str,
        text: &str,
        attachments: Vec<ChatAttachmentInput>,
    ) -> Result<()> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(Error::invalid("Message is empty"));
        }
        if !self.chats.status(id).is_live() {
            return self.chat_send(id, text, attachments);
        }
        let files = self.save_chat_attachments(id, attachments)?;
        let result = self.chats.steer(id, text, files.clone());
        if result.is_err() {
            for file in files {
                let _ = std::fs::remove_file(file.path);
            }
        }
        result
    }

    pub fn chat_set_option(&self, id: &str, option: &str, value: &str) -> Result<()> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        self.chats.set_option(id, option, value)
    }

    pub fn chat_send(
        &self,
        id: &str,
        text: &str,
        attachments: Vec<ChatAttachmentInput>,
    ) -> Result<()> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(Error::invalid("Message is empty"));
        }
        let files = self.save_chat_attachments(id, attachments)?;
        // The conversation outlives its agent process (an app restart, a
        // machine reboot, an agent crash): sending reconnects it.
        if !self.chats.status(id).is_live() {
            return self.resume_chat_with(id, text, files);
        }
        let result = self.chats.send(id, text, files.clone());
        if matches!(result, Err(Error::Conflict(_) | Error::Unavailable(_))) {
            for file in files {
                let _ = std::fs::remove_file(file.path);
            }
        }
        result
    }

    /// Send an earlier prompt again, with the attachments it already saved.
    pub fn chat_retry(&self, id: &str, item_id: &str) -> Result<()> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        let item = self
            .ctx
            .store
            .chat_items(id)?
            .into_iter()
            .find(|item| item.id == item_id && item.kind == crate::model::ChatItemKind::UserMessage)
            .ok_or_else(|| Error::NotFound("message".into()))?;
        let files: Vec<_> = item
            .attachments
            .into_iter()
            .filter(|file| std::path::Path::new(&file.path).is_file())
            .collect();
        if !self.chats.status(id).is_live() {
            return self.resume_chat_with(id, &item.text, files);
        }
        self.chats.send(id, &item.text, files)
    }

    /// Start the chat's agent again, on its saved native conversation when
    /// there is one, and deliver `text` as the first prompt. Like an ordinary
    /// agent app, there is no separate "start" step: a stopped chat is idle.
    fn resume_chat_with(
        &self,
        id: &str,
        text: &str,
        files: Vec<crate::model::ChatAttachment>,
    ) -> Result<()> {
        use crate::model::{ChatItemKind, ChatItemStatus};
        // A prompt left pending by an earlier failed start is not sent
        // implicitly along with this one; it can be retried from its turn.
        for mut item in self.ctx.store.chat_items(id)? {
            if item.kind == ChatItemKind::UserMessage && item.status == ChatItemStatus::Pending {
                item.status = ChatItemStatus::Failed;
                item.detail = "Not sent: the agent was not running".into();
                if let Ok(item) = self.ctx.store.put_chat_item(id, item) {
                    self.emit(AppEvent::ChatItem {
                        id: id.into(),
                        item,
                    });
                }
            }
        }
        let item = self
            .ctx
            .store
            .put_chat_item(id, crate::chat::pending_message(text, files))?;
        self.emit(AppEvent::ChatItem {
            id: id.into(),
            item: item.clone(),
        });
        // Without a saved conversation ID the agent starts a new native
        // conversation; the history stays visible here.
        let resume = self.ctx.store.chat_thread(id)?.is_some();
        if let Err(error) = self.launch(id, resume, None, 100, 30) {
            let mut item = item;
            item.status = ChatItemStatus::Failed;
            item.detail = error.to_string();
            if let Ok(item) = self.ctx.store.put_chat_item(id, item) {
                self.emit(AppEvent::ChatItem {
                    id: id.into(),
                    item,
                });
            }
            if !self.chats.status(id).is_live() {
                self.chats.fail(id, error.to_string());
            }
            return Err(error);
        }
        Ok(())
    }

    pub fn chat_respond(
        &self,
        id: &str,
        request_id: &str,
        decision: &str,
        answers: serde_json::Value,
    ) -> Result<()> {
        self.chats.respond(id, request_id, decision, answers)
    }

    pub fn chat_plan_respond(
        &self,
        id: &str,
        plan_id: &str,
        expected_revision: i64,
        decision: PlanDecision,
        feedback: Option<String>,
    ) -> Result<PlanRespondResult> {
        if self.ctx.store.session(id)?.mode != SessionMode::Ui {
            return Err(Error::invalid("Not a chat session"));
        }
        self.chats
            .respond_plan(id, plan_id, expected_revision, decision, feedback)
    }

    pub fn chat_interrupt(&self, id: &str) -> Result<()> {
        self.chats.interrupt(id)
    }

    pub fn chat_fail_active(&self, message: &str) {
        self.chats.fail_active(message);
    }
}
