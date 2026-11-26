use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::agents::{self, LaunchOptions};
use crate::environment;
use crate::error::{Error, Result};
use crate::model::{
    AgentKind, ChatAttachment, ChatAttachmentInput, ChatPermissionMode, Session, SessionMode,
    TerminalSnapshot,
};
use crate::sessions::LaunchSpec;
use crate::util::{LockExt, clean_name, new_id, now_millis};

use super::{
    App, NewChat, PASTE_IMAGE_MAX, SessionPatch, chat_permission_modes, pasted_image_extension,
    title_from_prompt,
};

struct NewSession<'a> {
    workspace_id: &'a str,
    agent: AgentKind,
    name: Option<&'a str>,
    prompt: Option<&'a str>,
    cols: u16,
    rows: u16,
    mode: SessionMode,
    model: Option<&'a str>,
    attachments: Vec<ChatAttachmentInput>,
    permission: ChatPermissionMode,
    worktree_base: Option<&'a str>,
}

impl App {
    pub(super) fn attachment_dir(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::invalid("Invalid session identifier"));
        }
        Ok(self.ctx.paths.data().join("attachments").join(id))
    }

    pub(super) fn save_chat_attachments(
        &self,
        id: &str,
        inputs: Vec<ChatAttachmentInput>,
    ) -> Result<Vec<ChatAttachment>> {
        const MAX_FILE: usize = 8 * 1024 * 1024;
        if inputs.len() > 4 {
            return Err(Error::invalid("Attach at most four files per message"));
        }
        let dir = self.attachment_dir(id)?;
        let mut decoded = Vec::new();
        for input in inputs {
            let name = Path::new(&input.name)
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| Error::invalid("Invalid attachment name"))?
                .chars()
                .take(180)
                .collect::<String>();
            if input.base64.len() > MAX_FILE * 4 / 3 + 8 {
                return Err(Error::invalid(format!("{name} is larger than 8 MB")));
            }
            if input.mime_type.len() > 120
                || !input
                    .mime_type
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'+' | b'.'))
            {
                return Err(Error::invalid("Invalid attachment type"));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&input.base64)
                .map_err(|_| Error::invalid("Invalid attachment data"))?;
            if bytes.len() > MAX_FILE {
                return Err(Error::invalid(format!("{name} is larger than 8 MB")));
            }
            decoded.push((
                name,
                if input.mime_type.is_empty() {
                    "application/octet-stream".into()
                } else {
                    input.mime_type
                },
                bytes,
            ));
        }
        let mut files = Vec::new();
        for (name, mime_type, bytes) in decoded {
            let file_id = new_id("att");
            let safe_name: String = name
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            let path = dir.join(format!("{file_id}-{safe_name}"));
            crate::fsx::atomic_write(&path, &bytes, Some(0o600))?;
            files.push(ChatAttachment {
                id: file_id,
                name,
                mime_type,
                path: path.display().to_string(),
            });
        }
        Ok(files)
    }

    /// An image pasted into a terminal: saved with the session's other
    /// attachments (removed with the session) and returned as a path the
    /// terminal types, which Claude Code and the other agents attach.
    pub fn terminal_paste_image(&self, id: &str, mime_type: &str, data: &str) -> Result<String> {
        let extension = pasted_image_extension(mime_type, data.len())?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|_| Error::invalid("The pasted image could not be read"))?;
        if bytes.len() > PASTE_IMAGE_MAX {
            return Err(Error::invalid("The pasted image is larger than 20 MB"));
        }
        self.ctx.store.session(id)?;
        let path = self
            .attachment_dir(id)?
            .join(format!("{}.{extension}", new_id("paste")));
        crate::fsx::atomic_write(&path, &bytes, Some(0o600))?;
        Ok(path.display().to_string())
    }

    pub(super) fn remove_chat_attachments(&self, id: &str) {
        if let Ok(dir) = self.attachment_dir(id) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    pub fn create_session(
        &self,
        workspace_id: &str,
        agent: AgentKind,
        name: Option<&str>,
        prompt: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> Result<Session> {
        self.create_session_mode(NewSession {
            workspace_id,
            agent,
            name,
            prompt,
            cols,
            rows,
            mode: SessionMode::Terminal,
            model: None,
            attachments: vec![],
            permission: ChatPermissionMode::default(),
            worktree_base: None,
        })
    }

    pub fn create_chat(&self, chat: NewChat) -> Result<Session> {
        self.create_session_mode(NewSession {
            workspace_id: &chat.workspace_id,
            agent: chat.agent,
            name: None,
            prompt: chat.prompt.as_deref(),
            cols: 100,
            rows: 30,
            mode: SessionMode::Ui,
            model: chat.model.as_deref(),
            attachments: chat.attachments,
            permission: chat.permission,
            worktree_base: chat.worktree_base.as_deref(),
        })
    }

    fn create_session_mode(&self, request: NewSession<'_>) -> Result<Session> {
        let NewSession {
            workspace_id,
            agent,
            name,
            prompt,
            cols,
            rows,
            mode,
            model,
            attachments,
            permission,
            worktree_base,
        } = request;
        let workspace = self.ctx.store.workspace(workspace_id)?;
        let prompt = prompt.map(str::trim).filter(|prompt| !prompt.is_empty());
        let name = match name.map(str::trim).filter(|name| !name.is_empty()) {
            Some(name) => clean_name(name, "session")?,
            None if prompt.is_some() => prompt
                .and_then(title_from_prompt)
                .unwrap_or_else(|| agents::adapter(agent).spec().name.to_string()),
            None => {
                let count = self
                    .ctx
                    .store
                    .sessions()?
                    .iter()
                    .filter(|s| s.workspace_id == workspace.id && s.agent == Some(agent))
                    .count();
                format!("{} {}", agents::adapter(agent).spec().name, count + 1)
            }
        };
        if mode == SessionMode::Ui && !chat_permission_modes(agent).contains(&permission) {
            return Err(Error::invalid(format!(
                "{} does not support that chat permission",
                agents::adapter(agent).spec().name
            )));
        }
        let now = now_millis();
        let id = new_id("ses");
        let worktree = worktree_base
            .map(|base| {
                crate::worktree::create(
                    &self.ops,
                    Path::new(&workspace.path),
                    self.ctx.paths.data(),
                    &id,
                    base,
                )
            })
            .transpose()?;
        let session = Session {
            id,
            workspace_id: workspace.id.clone(),
            name,
            worktree,
            agent: Some(agent),
            mode,
            archived: false,
            starred: false,
            created_at: now,
            updated_at: now,
            last_started_at: None,
            status: crate::model::SessionStatus::Stopped,
        };
        if let Err(error) = self.ctx.store.insert_session(&session) {
            if let Some(worktree) = &session.worktree {
                let _ = crate::worktree::remove(worktree, self.ctx.paths.data());
            }
            return Err(error);
        }
        if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
            self.ctx.store.set_chat_model(&session.id, Some(model))?;
        }
        if mode == SessionMode::Ui {
            self.ctx
                .store
                .set_chat_permission(&session.id, permission)?;
            let files = match self.save_chat_attachments(&session.id, attachments) {
                Ok(files) => files,
                Err(error) => {
                    self.remove_chat_attachments(&session.id);
                    let removed = session.worktree.as_ref().is_none_or(|tree| {
                        crate::worktree::remove(tree, self.ctx.paths.data()).is_ok()
                    });
                    if removed {
                        let _ = self.ctx.store.delete_session(&session.id);
                    } else {
                        let _ = self.emit_library();
                    }
                    return Err(error);
                }
            };
            if prompt.is_some() || !files.is_empty() {
                self.ctx.store.put_chat_item(
                    &session.id,
                    crate::chat::pending_message(prompt.unwrap_or(""), files),
                )?;
            }
        }
        self.emit_library()?;
        // A failed start leaves the session in place with a Failed status;
        // the next message sent reconnects the agent (`resume_chat_with`).
        if let Err(error) = self.launch(&session.id, false, prompt, cols, rows)
            && mode == SessionMode::Ui
        {
            self.chats.fail(&session.id, error.to_string());
        }
        self.session(&session.id)
    }

    /// A plain terminal: the user's login shell in the workspace folder,
    /// listed and managed like any other session.
    pub fn create_terminal(&self, workspace_id: &str) -> Result<Session> {
        let _guard = self.lock_terminal_lifecycle();
        let workspace = self.ctx.store.workspace(workspace_id)?;
        let count = self
            .ctx
            .store
            .sessions()?
            .iter()
            .filter(|s| s.workspace_id == workspace.id && s.agent.is_none())
            .count();
        let now = now_millis();
        let session = Session {
            id: new_id("ses"),
            workspace_id: workspace.id.clone(),
            name: format!("Terminal {}", count + 1),
            worktree: None,
            agent: None,
            mode: SessionMode::Terminal,
            archived: false,
            starred: false,
            created_at: now,
            updated_at: now,
            last_started_at: None,
            status: crate::model::SessionStatus::Stopped,
        };
        self.ctx.store.insert_session(&session)?;
        self.emit_library()?;
        Ok(session)
    }

    pub(super) fn lock_terminal_lifecycle(&self) -> std::sync::MutexGuard<'_, ()> {
        self.terminal_lifecycle.locked()
    }

    pub fn terminal_snapshot(&self, id: &str) -> Result<TerminalSnapshot> {
        let _guard = self.lock_terminal_lifecycle();
        let session = self.ctx.store.session(id)?;
        if session.mode != SessionMode::Terminal {
            return Err(Error::invalid("This session does not have a terminal"));
        }
        Ok(self.sessions.snapshot(id, session.agent.is_none()))
    }

    pub fn attach_terminal(
        &self,
        id: &str,
        generation: u32,
        retry: bool,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSnapshot> {
        let _guard = self.lock_terminal_lifecycle();
        let mut session = self.ctx.store.session(id)?;
        self.attach_shell(&mut session, generation, retry, cols, rows)
    }

    fn attach_shell(
        &self,
        session: &mut Session,
        generation: u32,
        retry: bool,
        cols: u16,
        rows: u16,
    ) -> Result<TerminalSnapshot> {
        if session.mode != SessionMode::Terminal || session.agent.is_some() {
            return Err(Error::invalid("This session is not a plain terminal"));
        }
        if session.archived {
            return Ok(self.sessions.snapshot(&session.id, true));
        }
        let (snapshot, started) = self.sessions.attach(&session.id, generation, retry, || {
            let workspace = self.ctx.store.workspace(&session.workspace_id)?;
            let mut env = environment::child_env(&self.ctx);
            env.push(("PATH".into(), self.ctx.paths.child_path()));
            Ok(LaunchSpec {
                argv: vec![
                    crate::sessions::login_shell().display().to_string(),
                    "-l".into(),
                ],
                cwd: PathBuf::from(workspace.path),
                env,
                cols,
                rows,
            })
        });
        if started {
            session.last_started_at = Some(now_millis());
            self.ctx.store.update_session(session)?;
            self.emit_library()?;
        }
        Ok(snapshot)
    }

    pub fn session(&self, id: &str) -> Result<Session> {
        let mut session = self.ctx.store.session(id)?;
        session.status = if session.mode == SessionMode::Ui {
            self.chats.status(id)
        } else {
            self.sessions.status(id)
        };
        Ok(session)
    }

    pub fn start_session(&self, id: &str, resume: bool, cols: u16, rows: u16) -> Result<Session> {
        let result = self.launch(id, resume, None, cols, rows);
        if let Err(error) = &result
            && self
                .ctx
                .store
                .session(id)
                .is_ok_and(|s| s.mode == SessionMode::Ui)
            && !self.chats.status(id).is_live()
        {
            self.chats.fail(id, error.to_string());
        }
        result
    }

    /// Start a chat on its native provider process.
    fn launch_chat(
        &self,
        session: &mut Session,
        workspace: &crate::model::Workspace,
        agent: AgentKind,
        resume: bool,
        prompt: Option<&str>,
    ) -> Result<Session> {
        let id = session.id.clone();
        let binary = crate::agent_runtime::runtime_binary(&self.ctx, agent)?;
        let mut env = environment::child_env(&self.ctx);
        env.push(("PATH".into(), self.ctx.paths.child_path()));
        env.extend(crate::agent_runtime::runtimes::agent_env(&self.ctx, agent)?);
        let native_ref = if resume {
            self.ctx.store.chat_thread(&id)?
        } else {
            None
        };
        let history = self.ctx.store.chat_items(&id)?;
        if resume
            && native_ref.is_none()
            && history
                .iter()
                .any(|item| item.status != crate::model::ChatItemStatus::Pending)
        {
            return Err(Error::Unavailable("The agent did not provide a conversation ID, so this history cannot be resumed. Create a new chat to start again.".into()));
        }
        let pending = history.iter().rev().find(|item| {
            item.kind == crate::model::ChatItemKind::UserMessage
                && item.status == crate::model::ChatItemStatus::Pending
        });
        let selection = self.ctx.store.chat_model(&id)?;
        let model = selection.or_else(|| workspace.model_for(agent).map(str::to_string));
        self.chats.start(
            &id,
            crate::agent_runtime::session::RuntimeSpec {
                provider: agent.as_str().to_string(),
                binary: binary.path,
                cwd: PathBuf::from(
                    session
                        .worktree
                        .as_ref()
                        .map_or(workspace.path.as_str(), |tree| tree.path.as_str()),
                ),
                model,
                permission: self.ctx.store.chat_permission(&id)?,
                native_ref,
                prompt: pending
                    .map(|i| i.text.clone())
                    .or_else(|| prompt.map(str::to_string)),
                prompt_id: pending.map(|i| i.id.clone()),
                attachments: pending.map(|i| i.attachments.clone()).unwrap_or_default(),
                env,
                install: binary.install,
            },
        )?;
        session.last_started_at = Some(now_millis());
        self.ctx.store.update_session(session)?;
        self.emit_library()?;
        self.session(&id)
    }

    pub(super) fn launch(
        &self,
        id: &str,
        resume: bool,
        prompt: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> Result<Session> {
        let mut session = self.ctx.store.session(id)?;
        let workspace = self.ctx.store.workspace(&session.workspace_id)?;
        if session.mode == SessionMode::Ui {
            let _guard = self.lock_terminal_lifecycle();
            session = self.ctx.store.session(id)?;
            let agent = session
                .agent
                .ok_or_else(|| Error::invalid("Chat requires an agent"))?;
            return self.launch_chat(&mut session, &workspace, agent, resume, prompt);
        }
        let _guard = self.lock_terminal_lifecycle();
        // Refresh the row under the lifecycle lock so a concurrent rename,
        // archive or deletion cannot be overwritten by the start timestamp.
        let mut session = self.ctx.store.session(id)?;
        let Some(agent) = session.agent else {
            return Err(Error::invalid("Open the terminal to attach its shell"));
        };
        self.sessions.start(id, || {
            let adapter = agents::adapter(agent);
            let binary = agents::find_binary(&self.ctx, agent).ok_or_else(|| {
                Error::Unavailable(format!(
                    "{} is not installed. Install it from Setup, then start the session again.",
                    adapter.spec().name
                ))
            })?;
            let mut argv = vec![binary.display().to_string()];
            argv.extend(adapter.launch_args(&LaunchOptions {
                model: workspace.model_for(agent).map(str::to_string),
                resume,
                prompt: prompt.map(str::to_string),
            }));
            let mut env = environment::child_env(&self.ctx);
            env.push(("PATH".into(), self.ctx.paths.child_path()));
            env.extend(adapter.launch_env());
            Ok(LaunchSpec {
                argv,
                cwd: PathBuf::from(&workspace.path),
                env,
                cols,
                rows,
            })
        })?;
        session.last_started_at = Some(now_millis());
        self.ctx.store.update_session(&session)?;
        self.emit_library()?;
        self.session(id)
    }

    pub fn stop_session(&self, id: &str) -> Result<()> {
        if self.ctx.store.session(id)?.mode == SessionMode::Ui {
            self.chats.stop(id)
        } else {
            self.sessions.stop(id)
        }
    }

    pub fn write_session(&self, id: &str, generation: u32, data: &str) -> Result<()> {
        self.sessions.write(id, generation, data)
    }

    pub fn resize_session(&self, id: &str, generation: u32, cols: u16, rows: u16) -> Result<()> {
        self.sessions.resize(id, generation, cols, rows)
    }

    pub fn update_session(&self, id: &str, patch: SessionPatch) -> Result<Session> {
        let _guard = self.lock_terminal_lifecycle();
        let mut session = self.ctx.store.session(id)?;
        if let Some(name) = patch.name {
            session.name = clean_name(&name, "session")?;
        }
        if let Some(archived) = patch.archived {
            session.archived = archived;
        }
        if let Some(starred) = patch.starred {
            session.starred = starred;
        }
        self.ctx.store.update_session(&session)?;
        if patch.archived == Some(true) {
            self.sessions.deactivate(id);
            self.chats.forget(id);
        }
        self.emit_library()?;
        self.session(id)
    }

    pub fn duplicate_session(&self, id: &str) -> Result<Session> {
        let _guard = self.lock_terminal_lifecycle();
        let source = self.ctx.store.session(id)?;
        let now = now_millis();
        let copy = Session {
            id: new_id("ses"),
            name: format!("{} copy", source.name),
            worktree: None,
            archived: false,
            starred: false,
            created_at: now,
            updated_at: now,
            last_started_at: None,
            ..source
        };
        self.ctx.store.insert_session(&copy)?;
        if copy.mode == SessionMode::Ui {
            if let Some(model) = self.ctx.store.chat_model(id)? {
                self.ctx.store.set_chat_model(&copy.id, Some(&model))?;
            }
            let permission = self.ctx.store.chat_permission(id)?;
            self.ctx.store.set_chat_permission(&copy.id, permission)?;
        }
        self.emit_library()?;
        self.session(&copy.id)
    }

    pub fn worktree_refs(&self, workspace_id: &str) -> Result<crate::worktree::WorktreeRefs> {
        let workspace = self.ctx.store.workspace(workspace_id)?;
        crate::worktree::refs(Path::new(&workspace.path))
    }

    pub fn remove_session_worktree(&self, id: &str) -> Result<Session> {
        let _guard = self.lock_terminal_lifecycle();
        let session = self.ctx.store.session(id)?;
        let tree = session
            .worktree
            .as_ref()
            .ok_or_else(|| Error::invalid("This session has no worktree"))?;
        if self.chats.status(id).is_live() || self.sessions.status(id).is_live() {
            return Err(Error::Conflict(
                "Stop this session before removing its worktree".into(),
            ));
        }
        if self.ctx.store.sessions()?.iter().any(|other| {
            other.id != id
                && other
                    .worktree
                    .as_ref()
                    .is_some_and(|candidate| candidate.root == tree.root)
        }) {
            return Err(Error::Conflict("Another session uses this worktree".into()));
        }
        crate::worktree::remove_managed(&self.ops, tree, self.ctx.paths.data())?;
        self.ctx.store.set_session_worktree(id, None)?;
        self.emit_library()?;
        self.session(id)
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        let _guard = self.lock_terminal_lifecycle();
        self.sessions.forget(id)?;
        self.chats.forget(id);
        self.ctx.store.delete_session(id)?;
        self.remove_chat_attachments(id);
        self.emit_library()
    }
}
