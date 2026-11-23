//! `App`: the facade the Tauri layer calls. It owns every subsystem and turns
//! subsystem callbacks into one stream of typed `AppEvent`s.

use crate::util::{LockExt, RwLockExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use base64::Engine as _;
use serde::Deserialize;

use crate::agents::{self, LaunchOptions};
use crate::chat::ChatSupervisor;
use crate::context::Context;
use crate::environment;
use crate::error::{Error, Result};
use crate::model::{
    AgentConfigText, AgentKind, AgentModel, AgentStatus, AppEvent, AppSettings, AppSnapshot,
    BackupEntry, ChatAttachment, ChatAttachmentData, ChatAttachmentInput, ChatPermissionMode,
    ChatSnapshot, CodexAccounts, CodexResetResult, EnvironmentReview, EnvironmentSpec,
    EnvironmentState, HealthReport, ModelOverride, NodeRelease, NodeState, OperationRecord,
    OperationState, PlanDecision, PlanRespondResult, Session, SessionMode, SetupAction, SetupCheck,
    TerminalSnapshot, Workspace,
};
use crate::monitor::Monitor;
use crate::node;
use crate::ops::{Operation, OperationManager};
use crate::paths::AppPaths;
use crate::sessions::{LaunchSpec, SessionEvent, Supervisor};
use crate::setup;
use crate::store::Store;
use crate::system::HostSystem;
use crate::util::{clean_name, new_id, now_millis};

type Sink = Arc<dyn Fn(AppEvent) + Send + Sync>;

fn sync_model_prices(
    ctx: &Context,
    force: bool,
) -> Result<Vec<cabin_usage::pricing::SourceStatus>> {
    use std::io::Read;
    use std::time::Duration;
    static SYNC: Mutex<()> = Mutex::new(());
    let _guard = SYNC.locked();
    let mut usage = cabin_usage::Usage::open(&ctx.paths.data().join("usage.sqlite3"))
        .map_err(|error| Error::Internal(format!("open usage database: {error}")))?;
    usage
        .sync_prices(now_millis(), force, |url| {
            const MAX_BYTES: u64 = 24 * 1024 * 1024;
            let response = crate::fetch::http(ctx, Duration::from_secs(30))
                .map_err(|error| error.to_string())?
                .get(url)
                .call()
                .map_err(|error| {
                    crate::fetch::http_error("Could not update model prices", error)
                        .to_string()
                })?;
            let mut data = Vec::new();
            response
                .into_reader()
                .take(MAX_BYTES + 1)
                .read_to_end(&mut data)
                .map_err(|error| error.to_string())?;
            if data.len() as u64 > MAX_BYTES {
                return Err("price catalog exceeds 24 MiB".into());
            }
            String::from_utf8(data).map_err(|error| error.to_string())
        })
        .map_err(|error| Error::Internal(format!("sync model prices: {error}")))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePatch {
    #[cfg_attr(test, ts(optional))]
    pub name: Option<String>,
    #[cfg_attr(test, ts(optional))]
    pub path: Option<String>,
    #[cfg_attr(test, ts(optional))]
    pub model_overrides: Option<Vec<ModelOverride>>,
    #[cfg_attr(test, ts(optional))]
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SessionPatch {
    #[cfg_attr(test, ts(optional))]
    pub name: Option<String>,
    #[cfg_attr(test, ts(optional))]
    pub archived: Option<bool>,
    #[cfg_attr(test, ts(optional))]
    pub starred: Option<bool>,
}

/// A short session title from the first line of a prompt.
fn title_from_prompt(prompt: &str) -> Option<String> {
    let line = prompt
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let mut title: String = line.chars().filter(|c| !c.is_control()).take(60).collect();
    if line.chars().count() > 60 {
        title = format!("{}…", title.trim_end());
    }
    Some(title)
}

pub struct App {
    ctx: Context,
    ops: Arc<OperationManager>,
    sessions: Arc<Supervisor>,
    terminal_lifecycle: Mutex<()>,
    chats: Arc<ChatSupervisor>,
    sink: Arc<RwLock<Option<Sink>>>,
    monitor: Mutex<Option<Monitor>>,
}

/// A new chat, as the UI asks for it (`chat_create`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewChat {
    pub workspace_id: String,
    pub agent: AgentKind,
    pub prompt: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub attachments: Vec<ChatAttachmentInput>,
    #[serde(default)]
    pub permission: ChatPermissionMode,
    /// Start in a new git worktree from this ref.
    #[serde(default)]
    pub worktree_base: Option<String>,
}

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

fn emit_to(sink: &RwLock<Option<Sink>>, event: AppEvent) {
    let sink = sink.read_lock().clone();
    if let Some(sink) = sink {
        sink(event);
    }
}

fn expand_home(raw: &str, home: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        home.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(raw)
    }
}

/// Largest image a terminal paste may carry.
const PASTE_IMAGE_MAX: usize = 20 * 1024 * 1024;

/// The file extension for a pasted image, or why it cannot be saved.
fn pasted_image_extension(mime_type: &str, encoded_len: usize) -> Result<&'static str> {
    if encoded_len > PASTE_IMAGE_MAX / 3 * 4 + 8 {
        return Err(Error::invalid("The pasted image is larger than 20 MB"));
    }
    match mime_type {
        "image/png" => Ok("png"),
        "image/jpeg" => Ok("jpg"),
        "image/gif" => Ok("gif"),
        "image/webp" => Ok("webp"),
        _ => Err(Error::invalid("Paste a PNG, JPEG, GIF or WebP image")),
    }
}

impl App {
    pub fn usage_sync(&self) -> Result<cabin_usage::SyncResult> {
        static SYNC: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = SYNC.locked();
        let mut usage = cabin_usage::Usage::open(&self.ctx.paths.data().join("usage.sqlite3"))
            .map_err(|error| Error::Internal(format!("open usage database: {error}")))?;
        usage.sync(self.ctx.paths.home())
            .map_err(|error| Error::Internal(format!("sync usage: {error}")))
    }

    pub fn usage_dashboard(&self, filter: &cabin_usage::Filter) -> Result<cabin_usage::Dashboard> {
        let usage = cabin_usage::Usage::open(&self.ctx.paths.data().join("usage.sqlite3"))
            .map_err(|error| Error::Internal(format!("open usage database: {error}")))?;
        usage.dashboard(filter).map_err(|error| Error::Internal(format!("read usage: {error}")))
    }

    pub fn usage_sync_prices(&self) -> Result<Vec<cabin_usage::pricing::SourceStatus>> {
        sync_model_prices(&self.ctx, true)
    }

    pub fn usage_save_price(&self, price: &cabin_usage::Price) -> Result<()> {
        let usage = cabin_usage::Usage::open(&self.ctx.paths.data().join("usage.sqlite3"))
            .map_err(|error| Error::Internal(format!("open usage database: {error}")))?;
        usage.save_price(price).map_err(|error| Error::invalid(format!("save price: {error}")))
    }

    pub fn open(paths: AppPaths) -> Result<Self> {
        crate::fsx::ensure_dir(paths.data())?;
        let legacy_runtime = paths.data().join("agent-host");
        if legacy_runtime.exists() {
            let _ = std::fs::remove_dir_all(legacy_runtime);
        }
        let store = Arc::new(Store::open(&paths.database())?);
        store.mark_interrupted()?;
        let ctx = Context {
            system: Arc::new(HostSystem {
                path: paths.child_path(),
            }),
            paths,
            store: store.clone(),
        };
        Ok(Self::with_context(ctx))
    }

    pub fn with_context(ctx: Context) -> Self {
        let sink: Arc<RwLock<Option<Sink>>> = Arc::new(RwLock::new(None));
        let ops = Arc::new(OperationManager::new(ctx.store.clone()));
        {
            let sink = sink.clone();
            ops.set_sink(move |record| {
                emit_to(
                    &sink,
                    AppEvent::Operation {
                        record: record.clone(),
                    },
                )
            });
        }
        let sessions = Arc::new(Supervisor::new(ctx.paths.data().join("terminal-history")));
        let chats = Arc::new(ChatSupervisor::new(ctx.store.clone()));
        {
            let sink = sink.clone();
            chats.set_sink(move |event| emit_to(&sink, event));
        }
        {
            let sink = sink.clone();
            sessions.set_sink(move |event| {
                emit_to(
                    &sink,
                    match event {
                        SessionEvent::Output { id, generation, offset, data } => {
                            AppEvent::SessionOutput { id, generation, offset, data }
                        }
                        SessionEvent::Status { id, generation, status } => {
                            AppEvent::TerminalStatus { id, generation, status }
                        }
                    },
                )
            });
        }
        Self {
            ctx,
            ops,
            sessions,
            terminal_lifecycle: Mutex::new(()),
            chats,
            sink,
            monitor: Mutex::new(None),
        }
    }

    /// Start background services: the monitor and the model price refresh.
    pub fn start_services(&self) {
        let pricing_ctx = self.ctx.clone();
        std::thread::spawn(move || loop {
            if sync_model_prices(&pricing_ctx, false).is_err() {
                cabin_diagnostics::event(cabin_diagnostics::Level::Warn, "usage", "price_refresh_failed", format_args!("Could not refresh model prices"));
            }
            std::thread::sleep(std::time::Duration::from_secs(30 * 60));
        });
        let store = self.ctx.store.clone();
        let sink = self.sink.clone();
        let monitor = Monitor::start(
            move || {
                store
                    .setting::<AppSettings>("app")
                    .ok()
                    .flatten()
                    .unwrap_or_default()
                    .latency_target
            },
            move |sample| emit_to(&sink, AppEvent::Monitor { sample }),
        );
        *self.monitor.locked() = Some(monitor);
    }

    /// The busiest processes, for the performance details (on request only).
    pub fn top_processes(&self, limit: usize) -> Result<crate::model::ProcessList> {
        let monitor = self.monitor.locked();
        let monitor = monitor
            .as_ref()
            .ok_or_else(|| Error::Internal("The monitor is not running".into()))?;
        Ok(monitor.top_processes(limit.clamp(1, 50)))
    }

    pub fn shutdown(&self) {
        self.sessions.stop_all();
        self.chats.stop_all();
        self.monitor.locked()
            .take();
    }

    pub fn set_event_sink(&self, sink: impl Fn(AppEvent) + Send + Sync + 'static) {
        *self.sink.write_lock() = Some(Arc::new(sink));
    }

    fn emit(&self, event: AppEvent) {
        emit_to(&self.sink, event);
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    // -------------------------------------------------------------- snapshot

    pub fn snapshot(&self) -> Result<AppSnapshot> {
        let _ = agents::codex_accounts(&self.ctx);
        self.snapshot_with_agents(agents::all_status(&self.ctx))
    }

    pub fn startup_snapshot(&self) -> Result<AppSnapshot> {
        self.snapshot_with_agents(agents::inventory(&self.ctx))
    }

    pub fn ensure_ipv4_network(&self) -> Result<()> {
        crate::guest_ipv4::ensure(&self.ctx, &self.ops)
    }

    fn snapshot_with_agents(&self, agents: Vec<AgentStatus>) -> Result<AppSnapshot> {
        let (workspaces, sessions) = self.library()?;
        Ok(AppSnapshot {
            workspaces,
            sessions,
            agents,
            environment: environment::state(&self.ctx),
            // The Mac app owns and fills in the catalog and the route.
            proxy: Default::default(),
            network: Default::default(),
            operations: self.ops.recent(50)?,
            settings: self.ctx.settings(),
            data_dir: self.ctx.paths.data().display().to_string(),
            hostname: std::fs::read_to_string("/etc/hostname")
                .map(|name| name.trim().to_string())
                .ok()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "This machine".into()),
        })
    }

    fn library(&self) -> Result<(Vec<Workspace>, Vec<Session>)> {
        let workspaces = self.ctx.store.workspaces()?;
        let mut sessions = self.ctx.store.sessions()?;
        for session in &mut sessions {
            session.status = if session.mode == SessionMode::Ui {
                self.chats.status(&session.id)
            } else {
                self.sessions.status(&session.id)
            };
        }
        Ok((workspaces, sessions))
    }

    fn emit_library(&self) -> Result<()> {
        let (workspaces, sessions) = self.library()?;
        self.emit(AppEvent::Library {
            workspaces,
            sessions,
        });
        Ok(())
    }

    // ------------------------------------------------------------ workspaces

    fn resolve_folder(&self, raw: &str) -> Result<String> {
        if raw.trim().is_empty() {
            return Err(Error::invalid("Choose a folder for the workspace"));
        }
        let path = expand_home(raw, self.ctx.paths.home());
        if !path.is_absolute() {
            return Err(Error::invalid("Use an absolute folder path"));
        }
        if path.exists() && !path.is_dir() {
            return Err(Error::invalid(format!(
                "{} is a file, not a folder",
                path.display()
            )));
        }
        crate::fsx::ensure_dir(&path)?;
        Ok(path.display().to_string())
    }

    pub fn create_workspace(&self, name: &str, path: &str) -> Result<Workspace> {
        let now = now_millis();
        let workspace = Workspace {
            id: new_id("ws"),
            name: clean_name(name, "workspace")?,
            path: self.resolve_folder(path)?,
            model_overrides: Vec::new(),
            archived: false,
            created_at: now,
            updated_at: now,
        };
        self.ctx.store.insert_workspace(&workspace)?;
        self.emit_library()?;
        Ok(workspace)
    }

    pub fn update_workspace(&self, id: &str, patch: WorkspacePatch) -> Result<Workspace> {
        let mut workspace = self.ctx.store.workspace(id)?;
        if let Some(name) = patch.name {
            workspace.name = clean_name(&name, "workspace")?;
        }
        if let Some(path) = patch.path {
            workspace.path = self.resolve_folder(&path)?;
        }
        if let Some(models) = patch.model_overrides {
            workspace.model_overrides = models
                .into_iter()
                .filter(|entry| !entry.model.trim().is_empty())
                .collect();
        }
        if let Some(archived) = patch.archived {
            workspace.archived = archived;
        }
        self.ctx.store.update_workspace(&workspace)?;
        self.emit_library()?;
        self.ctx.store.workspace(id)
    }

    pub fn duplicate_workspace(&self, id: &str) -> Result<Workspace> {
        let source = self.ctx.store.workspace(id)?;
        let now = now_millis();
        let copy = Workspace {
            id: new_id("ws"),
            name: format!("{} copy", source.name),
            created_at: now,
            updated_at: now,
            archived: false,
            ..source
        };
        self.ctx.store.insert_workspace(&copy)?;
        self.emit_library()?;
        Ok(copy)
    }

    pub fn delete_workspace(&self, id: &str) -> Result<()> {
        let _guard = self.lock_terminal_lifecycle();
        let session_ids = self.ctx.store.sessions()?.into_iter()
            .filter(|session| session.workspace_id == id)
            .map(|session| session.id)
            .collect::<Vec<_>>();
        for id in &session_ids {
            self.sessions.forget(id)?;
            self.chats.forget(id);
        }
        self.ctx.store.delete_workspace(id)?;
        for id in session_ids {
            self.remove_chat_attachments(&id);
        }
        self.emit_library()
    }

    // -------------------------------------------------------------- sessions

    fn attachment_dir(&self, id: &str) -> Result<PathBuf> {
        if id.is_empty()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::invalid("Invalid session identifier"));
        }
        Ok(self.ctx.paths.data().join("attachments").join(id))
    }

    fn save_chat_attachments(
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
        let path = self.attachment_dir(id)?.join(format!("{}.{extension}", new_id("paste")));
        crate::fsx::atomic_write(&path, &bytes, Some(0o600))?;
        Ok(path.display().to_string())
    }

    fn remove_chat_attachments(&self, id: &str) {
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
            return Err(Error::invalid(format!("{} does not support that chat permission", agents::adapter(agent).spec().name)));
        }
        let now = now_millis();
        let id = new_id("ses");
        let worktree = worktree_base.map(|base| crate::worktree::create(
            &self.ops, Path::new(&workspace.path), self.ctx.paths.data(), &id, base,
        )).transpose()?;
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
                    let removed = session.worktree.as_ref().is_none_or(|tree| crate::worktree::remove(tree, self.ctx.paths.data()).is_ok());
                    if removed { let _ = self.ctx.store.delete_session(&session.id); }
                    else { let _ = self.emit_library(); }
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

    fn lock_terminal_lifecycle(&self) -> std::sync::MutexGuard<'_, ()> {
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
                argv: vec![crate::sessions::login_shell().display().to_string(), "-l".into()],
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
                cwd: PathBuf::from(session.worktree.as_ref().map_or(workspace.path.as_str(), |tree| tree.path.as_str())),
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

    fn launch(
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

    pub fn agent_models(&self, workspace_id: &str, agent: AgentKind) -> Result<Vec<AgentModel>> {
        let workspace = self.ctx.store.workspace(workspace_id)?;
        let mut env = environment::child_env(&self.ctx);
        env.push(("PATH".into(), self.ctx.paths.child_path()));
        // OpenCode's runtime is its CLI; the others are asked through their own CLI.
        let cli = if agent == AgentKind::Opencode {
            let binary = crate::agent_runtime::runtime_binary(&self.ctx, agent)?;
            crate::agent_runtime::ensure_installed(&binary, &env)?;
            binary.path
        } else {
            crate::agent_runtime::runtimes::cli_path(&self.ctx, agent).ok_or_else(|| {
                Error::Unavailable(format!(
                    "{} is not installed. Install it from Settings → Agents.",
                    agents::adapter(agent).spec().name
                ))
            })?
        };
        crate::agent_runtime::discover::models(agent, &cli, Path::new(&workspace.path), env)
    }

    // ------------------------------------------------------------ ACP runtimes

    pub fn acp_runtimes(&self) -> Vec<crate::model::AcpRuntime> {
        crate::agent_runtime::runtimes::state(&self.ctx)
    }

    pub fn acp_runtime_versions(&self, agent: AgentKind) -> Result<Vec<String>> {
        crate::agent_runtime::runtimes::versions(&self.ctx, agent)
    }

    pub fn acp_runtime_install(
        &self,
        agent: AgentKind,
        version: &str,
        select: bool,
    ) -> Result<OperationRecord> {
        self.ops
            .run(crate::agent_runtime::runtimes::install_operation(
                &self.ctx, agent, version, select,
            )?)
    }

    pub fn acp_runtime_select(&self, agent: AgentKind, version: &str) -> Result<()> {
        crate::agent_runtime::runtimes::select(&self.ctx, agent, version)
    }

    pub fn acp_runtime_remove(&self, agent: AgentKind, version: &str) -> Result<()> {
        crate::agent_runtime::runtimes::remove(&self.ctx, agent, version)
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
        let tree = session.worktree.as_ref().ok_or_else(|| Error::invalid("This session has no worktree"))?;
        if self.chats.status(id).is_live() || self.sessions.status(id).is_live() {
            return Err(Error::Conflict("Stop this session before removing its worktree".into()));
        }
        if self.ctx.store.sessions()?.iter().any(|other| other.id != id && other.worktree.as_ref().is_some_and(|candidate| candidate.root == tree.root)) {
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

    // ---------------------------------------------------------------- agents

    pub fn agents(&self) -> Vec<AgentStatus> {
        let _ = agents::codex_accounts(&self.ctx);
        let agents = agents::all_status(&self.ctx);
        self.emit(AppEvent::Agents {
            agents: agents.clone(),
        });
        agents
    }

    pub fn agent_config(&self, kind: AgentKind) -> Result<AgentConfigText> {
        agents::config_text(&self.ctx, kind)
    }

    pub fn agent_backups(&self, kind: AgentKind) -> Result<Vec<BackupEntry>> {
        agents::backups(&self.ctx, kind)
    }

    pub fn agent_health(&self, kind: AgentKind) -> HealthReport {
        agents::health(&self.ctx, kind)
    }

    fn run_and_refresh_agents(&self, operation: Operation) -> Result<OperationRecord> {
        let record = self.ops.run(operation)?;
        self.agents();
        Ok(record)
    }

    pub fn set_agent_model(
        &self,
        kind: AgentKind,
        model: Option<String>,
    ) -> Result<OperationRecord> {
        self.run_and_refresh_agents(agents::set_model_operation(&self.ctx, kind, model))
    }

    pub fn save_agent_config(&self, kind: AgentKind, text: String) -> Result<OperationRecord> {
        self.run_and_refresh_agents(agents::save_config_operation(&self.ctx, kind, text))
    }

    pub fn restore_agent_backup(&self, kind: AgentKind, path: &str) -> Result<OperationRecord> {
        self.run_and_refresh_agents(agents::restore_backup_operation(&self.ctx, kind, path)?)
    }

    fn workspace_root(&self, workspace_id: Option<&str>) -> Result<Option<PathBuf>> {
        workspace_id
            .map(|id| Ok(PathBuf::from(self.ctx.store.workspace(id)?.path)))
            .transpose()
    }

    fn session_workspace_root(&self, workspace_id: &str, session_id: Option<&str>) -> Result<PathBuf> {
        let workspace = self.ctx.store.workspace(workspace_id)?;
        if let Some(id) = session_id {
            let session = self.ctx.store.session(id)?;
            if session.workspace_id != workspace_id { return Err(Error::invalid("Session belongs to another workspace")); }
            if let Some(tree) = session.worktree { return Ok(PathBuf::from(tree.path)); }
        }
        Ok(PathBuf::from(workspace.path))
    }

    // --- Workbench panel: files, git changes, dev-server ports -------------

    fn workbench_root(&self, workspace_id: &str, session_id: Option<&str>) -> Result<PathBuf> {
        let root = self.session_workspace_root(workspace_id, session_id)?;
        if !root.is_dir() {
            return Err(Error::invalid("The workspace folder does not exist"));
        }
        Ok(root)
    }

    pub fn fs_list(&self, workspace_id: &str, session_id: Option<&str>, path: &str) -> Result<crate::model::FsListing> {
        crate::workbench::files::list(&self.workbench_root(workspace_id, session_id)?, path)
    }

    pub fn fs_read(&self, workspace_id: &str, session_id: Option<&str>, path: &str) -> Result<crate::model::FsFile> {
        crate::workbench::files::read(&self.workbench_root(workspace_id, session_id)?, path)
    }

    pub fn git_status(&self, workspace_id: &str, session_id: Option<&str>) -> Result<crate::model::GitStatus> {
        crate::workbench::git::status(&self.workbench_root(workspace_id, session_id)?)
    }

    pub fn git_diff(&self, workspace_id: &str, session_id: Option<&str>, path: &str, staged: bool) -> Result<crate::model::GitFileDiff> {
        crate::workbench::git::diff(&self.workbench_root(workspace_id, session_id)?, path, staged)
    }

    pub fn git_stage(&self, workspace_id: &str, session_id: Option<&str>, paths: &[String], stage: bool) -> Result<crate::model::GitStatus> {
        let root = self.workbench_root(workspace_id, session_id)?;
        if stage {
            crate::workbench::git::stage(&root, paths)?;
        } else {
            crate::workbench::git::unstage(&root, paths)?;
        }
        crate::workbench::git::status(&root)
    }

    pub fn git_discard(&self, workspace_id: &str, session_id: Option<&str>, paths: &[String]) -> Result<crate::model::GitStatus> {
        let root = self.workbench_root(workspace_id, session_id)?;
        crate::workbench::git::discard(&root, paths)?;
        crate::workbench::git::status(&root)
    }

    pub fn git_commit(&self, workspace_id: &str, session_id: Option<&str>, message: &str, all: bool) -> Result<crate::model::GitCommitResult> {
        crate::workbench::git::commit(&self.workbench_root(workspace_id, session_id)?, message, all)
    }

    pub fn listening_ports(&self) -> Vec<crate::model::ListeningPort> {
        crate::workbench::ports::listening()
    }

    /// MCP servers and skills an agent keeps on disk; project skills too when
    /// a workspace is given.
    pub fn agent_extensions(
        &self,
        kind: AgentKind,
        workspace_id: Option<&str>,
    ) -> Result<crate::model::AgentExtensions> {
        let root = self.workspace_root(workspace_id)?;
        Ok(agents::extensions::list(&self.ctx, kind, root.as_deref()))
    }

    /// A bounded, sorted list of files under the workspace, for `@` mentions
    /// in the composer. Hidden folders and dependency/build trees are skipped.
    pub fn workspace_files(&self, workspace_id: &str) -> Result<Vec<String>> {
        self.session_workspace_files(workspace_id, None)
    }

    pub fn session_workspace_files(&self, workspace_id: &str, session_id: Option<&str>) -> Result<Vec<String>> {
        const LIMIT: usize = 800;
        const SKIP: &[&str] = &[
            "node_modules",
            "target",
            "dist",
            "build",
            "out",
            ".git",
            ".next",
            ".turbo",
            ".venv",
            "venv",
            "__pycache__",
            "Pods",
            ".gradle",
        ];
        let root = self.session_workspace_root(workspace_id, session_id)?;
        if !root.is_dir() {
            return Err(Error::invalid("The workspace folder does not exist"));
        }
        let mut files = Vec::new();
        let mut dirs = vec![root.clone()];
        while let Some(dir) = dirs.pop() {
            if files.len() >= LIMIT {
                break;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = entries.filter_map(std::result::Result::ok).collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                if files.len() >= LIMIT {
                    break;
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_symlink() {
                    continue;
                }
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') {
                    continue;
                }
                if kind.is_dir() {
                    if !SKIP.contains(&name.as_ref()) {
                        dirs.push(entry.path());
                    }
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                if let Ok(relative) = entry.path().strip_prefix(&root) {
                    files.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        files.sort();
        Ok(files)
    }

    /// What the composer offers for an agent in a workspace: its skills (user and project folders)
    /// and its configured MCP servers, read from disk. There is no live query of the agent, so MCP
    /// servers report `configured` or `disabled`, and the agent's built-in slash commands are not listed.
    pub fn agent_catalog(
        &self,
        workspace_id: &str,
        kind: AgentKind,
    ) -> Result<crate::model::AgentCatalog> {
        self.session_agent_catalog(workspace_id, kind, None)
    }

    pub fn session_agent_catalog(
        &self, workspace_id: &str, kind: AgentKind, session_id: Option<&str>,
    ) -> Result<crate::model::AgentCatalog> {
        let root = self.session_workspace_root(workspace_id, session_id)?;
        let extensions = agents::extensions::list(&self.ctx, kind, Some(&root));
        let adapter = agents::adapter(kind);
        // Disk skills are always listed, with each agent's own invocation
        // syntax. The live command list wins when the agent advertises the same
        // skill itself (see `mergeCommands`), so a fresh chat still has the
        // downloaded skills before the agent has reported its command list.
        let mut commands: Vec<crate::model::AgentCommand> = extensions
            .skills
            .iter()
            .map(|skill| crate::model::AgentCommand {
                name: skill.name.clone(),
                description: Some(skill.description.clone()).filter(|d| !d.is_empty()),
                hint: None,
                kind: "skill".into(),
                source: Some(skill.scope.clone()),
                insert: adapter.skill_insert(&skill.name),
            })
            .collect();
        commands.sort_by(|a, b| a.name.cmp(&b.name));
        commands.dedup_by(|a, b| a.name == b.name);
        let mcp = extensions
            .mcp_servers
            .iter()
            .map(|server| crate::model::McpStatus {
                name: server.name.clone(),
                status: if server.enabled {
                    "configured"
                } else {
                    "disabled"
                }
                .into(),
                detail: None,
                tools: None,
            })
            .collect();
        Ok(crate::model::AgentCatalog {
            commands,
            mcp,
            error: extensions.mcp_error,
        })
    }

    pub fn change_mcp_library(&self, change: agents::mcp_library::McpChange) -> Result<OperationRecord> {
        self.ops.run(agents::mcp_library::operation(&self.ctx, change))
    }

    pub fn install_library_skill(
        &self,
        source: agents::skill_library::SkillInstall,
        targets: Vec<AgentKind>,
        mode: agents::skill_library::SkillMode,
    ) -> Result<OperationRecord> {
        self.ops.run(agents::skill_library::install_operation(&self.ctx, source, targets, mode)?)
    }

    pub fn change_skill_library(&self, change: agents::skill_library::SkillChange) -> Result<OperationRecord> {
        self.ops.run(agents::skill_library::change_operation(&self.ctx, change))
    }

    pub fn agent_mcp_save(
        &self,
        kind: AgentKind,
        server: crate::model::McpServer,
        previous: Option<String>,
    ) -> Result<OperationRecord> {
        self.ops.run(agents::extensions::save_mcp_operation(
            &self.ctx, kind, server, previous,
        )?)
    }

    pub fn agent_mcp_remove(&self, kind: AgentKind, name: String) -> Result<OperationRecord> {
        self.ops.run(agents::extensions::remove_mcp_operation(
            &self.ctx, kind, name,
        )?)
    }

    pub fn agent_mcp_set_enabled(
        &self,
        kind: AgentKind,
        name: String,
        enabled: bool,
    ) -> Result<OperationRecord> {
        self.ops.run(agents::extensions::set_mcp_enabled_operation(
            &self.ctx, kind, name, enabled,
        )?)
    }

    pub fn agent_mcp_copy(
        &self,
        kind: AgentKind,
        name: String,
        targets: Vec<AgentKind>,
    ) -> Result<OperationRecord> {
        self.ops.run(agents::extensions::copy_mcp_operation(
            &self.ctx, kind, name, targets,
        )?)
    }

    pub fn agent_skill_read(
        &self,
        kind: AgentKind,
        path: String,
        workspace_id: Option<&str>,
    ) -> Result<crate::model::SkillFile> {
        let root = self.workspace_root(workspace_id)?;
        agents::extensions::read_skill(&self.ctx, kind, &path, root.as_deref())
    }

    pub fn agent_skill_delete(
        &self,
        kind: AgentKind,
        path: String,
        workspace_id: Option<&str>,
    ) -> Result<OperationRecord> {
        let root = self.workspace_root(workspace_id)?;
        self.ops.run(agents::extensions::delete_skill_operation(
            &self.ctx,
            kind,
            path,
            root.as_deref(),
        )?)
    }

    pub fn agent_skill_copy(
        &self,
        kind: AgentKind,
        path: String,
        targets: Vec<AgentKind>,
        workspace_id: Option<&str>,
    ) -> Result<OperationRecord> {
        let root = self.workspace_root(workspace_id)?;
        self.ops.run(agents::extensions::copy_skill_operation(
            &self.ctx,
            kind,
            path,
            targets,
            root.as_deref(),
        )?)
    }

    fn ensure_codex_idle(&self) -> Result<()> {
        for session in self.ctx.store.sessions()? {
            if session.agent != Some(AgentKind::Codex) {
                continue;
            }
            let status = if session.mode == SessionMode::Ui {
                self.chats.status(&session.id)
            } else {
                self.sessions.status(&session.id)
            };
            if status.is_live() {
                return Err(Error::Conflict(format!(
                    "Stop Codex session “{}” before switching accounts",
                    session.name
                )));
            }
        }
        Ok(())
    }

    pub fn claude_usage(&self, account_id: Option<&str>) -> Result<crate::model::ClaudeUsage> {
        agents::claude_usage(&self.ctx, account_id)
    }

    pub fn codex_accounts(&self) -> Result<CodexAccounts> {
        agents::codex_accounts(&self.ctx)
    }

    pub fn codex_export_account(&self, id: &str, format: &str) -> Result<String> {
        agents::codex_export_account(&self.ctx, id, format)
    }

    pub fn codex_import_account(
        &self,
        contents: Option<String>,
        activate: bool,
    ) -> Result<CodexAccounts> {
        if activate {
            self.ensure_codex_idle()?;
        }
        let accounts = agents::codex_import_account(&self.ctx, contents, activate)?;
        self.agents();
        Ok(accounts)
    }

    pub fn codex_switch_account(&self, id: &str) -> Result<CodexAccounts> {
        self.ensure_codex_idle()?;
        let accounts = agents::codex_switch_account(&self.ctx, id)?;
        self.agents();
        Ok(accounts)
    }

    pub fn codex_remove_account(&self, id: &str) -> Result<CodexAccounts> {
        agents::codex_remove_account(&self.ctx, id)
    }

    pub fn codex_refresh_quota(&self) -> Result<CodexAccounts> {
        agents::codex_refresh_quota(&self.ctx)
    }

    pub fn codex_use_reset_card(
        &self,
        credit_id: &str,
        confirmed: bool,
    ) -> Result<CodexResetResult> {
        agents::codex_use_reset_card(&self.ctx, credit_id, confirmed)
    }

    // ----------------------------------------------------------- environment

    pub fn environment(&self) -> EnvironmentState {
        environment::state(&self.ctx)
    }

    pub fn review_environment(&self, draft: &EnvironmentSpec) -> EnvironmentReview {
        environment::review(&self.ctx, draft)
    }

    pub fn apply_environment(&self, spec: EnvironmentSpec) -> Result<OperationRecord> {
        let record = self
            .ops
            .run(environment::apply_operation(&self.ctx, spec)?)?;
        self.emit(AppEvent::Environment {
            state: self.environment(),
        });
        Ok(record)
    }

    // ----------------------------------------------------------------- setup

    pub fn setup_checks(&self) -> Vec<SetupCheck> {
        setup::checks(&self.ctx)
    }

    pub fn run_setup_action(&self, action: &SetupAction) -> Result<OperationRecord> {
        self.run_and_refresh_agents(setup::action_operation(&self.ctx, action)?)
    }

    // ------------------------------------------------------------------ node

    pub fn node_state(&self) -> NodeState {
        node::state(&self.ctx)
    }

    pub fn node_releases(&self, mirror: Option<&str>) -> Result<Vec<NodeRelease>> {
        node::releases(&self.ctx, mirror)
    }

    pub fn node_install(&self, request: node::InstallRequest) -> Result<OperationRecord> {
        self.run_and_refresh_agents(node::install_operation(&self.ctx, request)?)
    }

    pub fn node_use(&self, version: &str) -> Result<OperationRecord> {
        self.run_and_refresh_agents(node::use_operation(&self.ctx, version)?)
    }

    pub fn node_remove(&self, version: &str) -> Result<OperationRecord> {
        self.ops.run(node::remove_operation(&self.ctx, version)?)
    }

    // ------------------------------------------------------------ operations

    pub fn operations(&self) -> Result<Vec<OperationRecord>> {
        self.ops.recent(50)
    }

    pub fn cancel_operation(&self, id: &str) -> Result<()> {
        self.ops.cancel(id)
    }

    /// Re-run a finished operation from its stored request.
    pub fn retry_operation(&self, id: &str) -> Result<OperationRecord> {
        let previous = self
            .ops
            .recent(200)?
            .into_iter()
            .find(|record| record.id == id)
            .ok_or_else(|| Error::NotFound("operation".into()))?;
        if !previous.state.is_finished() || previous.state == OperationState::Succeeded {
            return Err(Error::Conflict(
                "Only failed operations can be retried".into(),
            ));
        }
        let (kind, request) = self.ctx.store.operation_request(id)?;
        let request =
            request.ok_or_else(|| Error::Conflict("This operation cannot be retried".into()))?;
        #[derive(Deserialize)]
        struct AgentRequest {
            agent: AgentKind,
            #[serde(default)]
            model: Option<String>,
        }
        match kind.as_str() {
            "environment.apply" => self.apply_environment(serde_json::from_value(request)?),
            "agent.setModel" => {
                let request: AgentRequest = serde_json::from_value(request)?;
                self.set_agent_model(request.agent, request.model)
            }
            "setup.installAgent" => {
                let request: AgentRequest = serde_json::from_value(request)?;
                self.run_setup_action(&SetupAction::InstallAgent {
                    agent: request.agent,
                })
            }
            "acp.install" => {
                #[derive(Deserialize)]
                struct InstallRequest {
                    agent: AgentKind,
                    version: String,
                    #[serde(default)]
                    select: bool,
                }
                let request: InstallRequest = serde_json::from_value(request)?;
                self.acp_runtime_install(request.agent, &request.version, request.select)
            }
            "setup.installBase" => self.run_setup_action(&SetupAction::InstallBasePackages),
            "python.install" => self.run_setup_action(&SetupAction::InstallPython),
            "node.install" => self.node_install(serde_json::from_value(request)?),
            "node.use" | "node.remove" => {
                #[derive(Deserialize)]
                struct VersionRequest {
                    version: String,
                }
                let request: VersionRequest = serde_json::from_value(request)?;
                if kind == "node.use" {
                    self.node_use(&request.version)
                } else {
                    self.node_remove(&request.version)
                }
            }
            other => Err(Error::Conflict(format!(
                "Operations of kind {other} cannot be retried"
            ))),
        }
    }

    // -------------------------------------------------------------- settings

    pub fn update_settings(&self, settings: AppSettings) -> Result<AppSettings> {
        for (label, target) in [
            ("latency target", &settings.latency_target),
            ("connectivity check target", &settings.check_target),
        ] {
            let valid = target
                .rsplit_once(':')
                .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
            if !valid {
                return Err(Error::invalid(format!(
                    "The {label} must look like host:port"
                )));
            }
        }
        self.ctx.store.set_setting("app", &settings)?;
        Ok(settings)
    }
}

fn chat_permission_modes(agent: AgentKind) -> Vec<ChatPermissionMode> {
    use ChatPermissionMode::*;
    match agent {
        AgentKind::Claude | AgentKind::Codex => {
            vec![ApprovalRequired, AutoAcceptEdits, Auto, Plan, FullAccess]
        }
        AgentKind::Opencode => vec![ApprovalRequired, AutoAcceptEdits, FullAccess],
        AgentKind::Antigravity => vec![ApprovalRequired, AutoAcceptEdits, Plan],
        // Pi has no permission system: it never asks.
        AgentKind::Pi => vec![],
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::SessionStatus;
    use crate::system::fake::FakeSystem;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn app(dir: &Path) -> App {
        App::with_context(Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system: Arc::new(FakeSystem::default()),
        })
    }

    #[test]
    fn plan_mode_is_offered_only_by_native_plan_transports() {
        assert!(chat_permission_modes(AgentKind::Claude).contains(&ChatPermissionMode::Plan));
        assert!(chat_permission_modes(AgentKind::Codex).contains(&ChatPermissionMode::Plan));
        assert!(chat_permission_modes(AgentKind::Antigravity).contains(&ChatPermissionMode::Plan));
        assert!(!chat_permission_modes(AgentKind::Opencode).contains(&ChatPermissionMode::Plan));
        assert!(!chat_permission_modes(AgentKind::Pi).contains(&ChatPermissionMode::Plan));
    }

    #[test]
    fn legacy_node_runtime_cache_is_removed_on_open() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let paths = AppPaths::new(dir.path().join("data"), dir.path().join("home"));
        let bundle = paths.data().join("agent-host/deadbeef/agent-host.mjs");
        std::fs::create_dir_all(bundle.parent().unwrap())?;
        std::fs::write(&bundle, "old sidecar")?;
        let _app = App::open(paths.clone())?;
        assert!(!paths.data().join("agent-host").exists());
        Ok(())
    }

    #[test]
    fn workspace_and_session_lifecycle_with_a_fake_agent() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let events = Arc::new(Mutex::new(Vec::<AppEvent>::new()));
        let seen = events.clone();
        app.set_event_sink(move |event| seen.lock().unwrap().push(event));

        let bin = dir.path().join("home/.local/bin");
        std::fs::create_dir_all(&bin)?;
        let fake = bin.join(agents::adapter(AgentKind::Claude).spec().binary);
        std::fs::write(
            &fake,
            "#!/bin/sh\necho \"args:$*\"\necho \"proxy:$HTTPS_PROXY\"\n",
        )?;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;

        let workspace = app.create_workspace("API", "~/code/api")?;
        assert!(dir.path().join("home/code/api").is_dir());
        app.update_workspace(
            &workspace.id,
            WorkspacePatch {
                model_overrides: Some(vec![ModelOverride {
                    agent: AgentKind::Claude,
                    model: "opus".into(),
                }]),
                ..Default::default()
            },
        )?;
        let session = app.create_session(&workspace.id, AgentKind::Claude, None, None, 80, 24)?;
        assert_eq!(session.name, "Claude Code 1");

        let started = Instant::now();
        while !matches!(
            app.session(&session.id)?.status,
            SessionStatus::Exited { .. }
        ) {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(20));
        }
        let started = Instant::now();
        while !app.terminal_snapshot(&session.id)?.data.contains("proxy:") {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = app.terminal_snapshot(&session.id)?.data;
        assert!(output.contains("args:--model opus"), "{output}");
        // One global proxy (off here): sessions get no proxy of their own.
        assert!(
            output.lines().any(|line| line.trim() == "proxy:"),
            "{output}"
        );

        app.start_session(&session.id, true, 80, 24)?;
        let started = Instant::now();
        while !app.terminal_snapshot(&session.id)?.data.contains("--continue") {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(20));
        }

        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(e, AppEvent::Library { .. })));
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AppEvent::SessionOutput { .. }))
        );
        drop(events);

        app.delete_workspace(&workspace.id)?;
        assert!(app.snapshot()?.sessions.is_empty());
        Ok(())
    }

    #[test]
    fn a_pasted_image_is_saved_privately_with_the_session() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let folder = dir.path().join("repo");
        std::fs::create_dir_all(&folder)?;
        let workspace = app.create_workspace("W", &folder.display().to_string())?;
        let terminal = app.create_terminal(&workspace.id)?;
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nimage");
        let path = app.terminal_paste_image(&terminal.id, "image/png", &png)?;
        assert!(path.ends_with(".png") && !path.contains(' '));
        assert_eq!(std::fs::read(&path)?, b"\x89PNG\r\n\x1a\nimage");
        assert_eq!(std::fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);

        assert!(app.terminal_paste_image(&terminal.id, "image/svg+xml", &png).is_err());
        assert!(app.terminal_paste_image(&terminal.id, "image/png", "not base64!").is_err());
        assert!(app.terminal_paste_image("missing", "image/png", &png).is_err());
        assert!(pasted_image_extension("image/png", PASTE_IMAGE_MAX * 2).is_err());

        app.delete_session(&terminal.id)?;
        assert!(!Path::new(&path).exists());
        Ok(())
    }

    #[test]
    fn worktree_session_uses_isolated_files_and_duplicate_uses_workspace() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let folder = dir.path().join("repo");
        std::fs::create_dir_all(&folder)?;
        let git = |args: &[&str]| -> Result<()> {
            let output = std::process::Command::new("git").arg("-C").arg(&folder).args(args).output()?;
            if !output.status.success() { return Err(Error::invalid(String::from_utf8_lossy(&output.stderr).to_string())); }
            Ok(())
        };
        git(&["init"])?;
        git(&["config", "user.name", "Test"])?;
        git(&["config", "user.email", "test@example.invalid"])?;
        std::fs::write(folder.join("tracked.txt"), "base")?;
        git(&["add", "."])?;
        git(&["-c", "commit.gpgsign=false", "commit", "-m", "initial"])?;
        let workspace = app.create_workspace("W", &folder.display().to_string())?;
        let session = app.create_terminal(&workspace.id)?;
        let tree = crate::worktree::create(&app.ops, &folder, app.ctx.paths.data(), &session.id, "HEAD")?;
        app.ctx.store.set_session_worktree(&session.id, Some(&tree))?;
        std::fs::write(Path::new(&tree.path).join("isolated.txt"), "new")?;
        assert!(app.session_workspace_files(&workspace.id, Some(&session.id))?.contains(&"isolated.txt".into()));
        assert!(!app.workspace_files(&workspace.id)?.contains(&"isolated.txt".into()));
        let copy = app.duplicate_session(&session.id)?;
        assert!(copy.worktree.is_none());
        assert!(app.remove_session_worktree(&session.id).is_err());
        std::fs::remove_file(Path::new(&tree.path).join("isolated.txt"))?;
        assert!(app.remove_session_worktree(&session.id)?.worktree.is_none());
        assert!(folder.join("tracked.txt").exists());
        Ok(())
    }

    #[test]
    fn terminal_runs_a_shell_in_the_workspace() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let folder = dir.path().join("w");
        let workspace = app.create_workspace("W", &folder.display().to_string())?;
        let terminal = app.create_terminal(&workspace.id)?;
        assert_eq!(terminal.status, SessionStatus::Stopped);
        let shell = app.attach_terminal(&terminal.id, 0, false, 80, 24)?;
        assert!(matches!(shell.status, SessionStatus::Running { .. }));
        assert_eq!(app.attach_terminal(&terminal.id, 0, false, 80, 24)?.generation, shell.generation);
        assert_eq!(
            (terminal.name.as_str(), terminal.agent),
            ("Terminal 1", None)
        );
        // Stored as a terminal, listed with the other sessions.
        assert_eq!(app.snapshot()?.sessions[0].agent, None);
        assert_eq!(
            app.create_terminal(&workspace.id)?.name,
            "Terminal 2"
        );

        app.write_session(&terminal.id, shell.generation, "echo \"at:$(pwd) sum:$((40+2))\"\n")?;
        let started = Instant::now();
        while !app.terminal_snapshot(&terminal.id)?.data.contains("sum:42") {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{}",
                app.terminal_snapshot(&terminal.id)?.data
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = app.terminal_snapshot(&terminal.id)?.data;
        let canonical = std::fs::canonicalize(&folder)?;
        assert!(
            output.contains(&format!("at:{}", folder.display()))
                || output.contains(&format!("at:{}", canonical.display())),
            "{output}"
        );
        app.stop_session(&terminal.id)?;
        Ok(())
    }

    #[test]
    fn the_catalog_lists_skills_from_disk_with_each_agents_own_insert_text() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        std::fs::create_dir_all(dir.path().join("w"))?;
        let workspace = app.create_workspace("W", &dir.path().join("w").display().to_string())?;
        for kind in [AgentKind::Codex, AgentKind::Pi, AgentKind::Claude] {
            let folder = agents::extensions::user_skills_dir(app.context(), kind).join("pdf-tools");
            std::fs::create_dir_all(&folder)?;
            std::fs::write(
                folder.join("SKILL.md"),
                "---\nname: pdf-tools\ndescription: Read PDFs\n---\n\nUse pdftotext.\n",
            )?;
        }
        let insert = |kind| -> Result<String> {
            let catalog = app.agent_catalog(&workspace.id, kind)?;
            assert_eq!(catalog.error, None);
            let skill = catalog
                .commands
                .iter()
                .find(|c| c.name == "pdf-tools")
                .expect("skill listed");
            assert_eq!(skill.kind, "skill");
            assert_eq!(skill.description.as_deref(), Some("Read PDFs"));
            Ok(skill.insert.clone())
        };
        assert_eq!(insert(AgentKind::Codex)?, "$pdf-tools ");
        assert_eq!(insert(AgentKind::Pi)?, "/skill:pdf-tools ");
        // Claude advertises the same skills as commands once its session runs;
        // the composer merges the two lists by token, so the disk row is the
        // fallback a fresh chat can already show.
        assert_eq!(insert(AgentKind::Claude)?, "/pdf-tools ");
        Ok(())
    }

    #[test]
    fn missing_agent_reports_a_failed_session() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let workspace = app.create_workspace("W", &dir.path().join("w").display().to_string())?;
        let session =
            app.create_session(&workspace.id, AgentKind::Pi, Some("try"), None, 80, 24)?;
        assert!(matches!(session.status, SessionStatus::Failed { .. }));
        let error = app.start_session(&session.id, false, 80, 24).unwrap_err();
        assert_eq!(error.code(), "unavailable");
        Ok(())
    }

    #[test]
    fn first_prompt_names_the_session_and_reaches_the_agent() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let bin = dir.path().join("home/.local/bin");
        std::fs::create_dir_all(&bin)?;
        let fake = bin.join(agents::adapter(AgentKind::Claude).spec().binary);
        std::fs::write(
            &fake,
            "#!/bin/sh\nfor a in \"$@\"; do echo \"arg:[$a]\"; done\n",
        )?;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
        let workspace = app.create_workspace("W", "~/w")?;
        let prompt = "Fix the flaky login test\nthen run the suite";
        let session =
            app.create_session(&workspace.id, AgentKind::Claude, None, Some(prompt), 80, 24)?;
        assert_eq!(session.name, "Fix the flaky login test");
        let started = Instant::now();
        while !app
            .terminal_snapshot(&session.id)?
            .data
            .contains("run the suite")
        {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "{}",
                app.terminal_snapshot(&session.id)?.data
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            app.terminal_snapshot(&session.id)?.data
                .contains("arg:[Fix the flaky login test")
        );

        let starred = app.update_session(
            &session.id,
            SessionPatch {
                starred: Some(true),
                ..Default::default()
            },
        )?;
        assert!(starred.starred);
        Ok(())
    }

    #[test]
    fn retry_rebuilds_the_operation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        // An invalid existing config makes the first attempt fail.
        let path = agents::config_path(&app.ctx, AgentKind::Claude);
        crate::fsx::atomic_write(&path, b"not json", None)?;
        let failed = app.set_agent_model(AgentKind::Claude, Some("opus".into()))?;
        assert_eq!(failed.state, OperationState::Failed);
        crate::fsx::atomic_write(&path, b"{}", None)?;
        let retried = app.retry_operation(&failed.id)?;
        assert_eq!(retried.state, OperationState::Succeeded);
        Ok(())
    }

    #[test]
    fn settings_are_validated() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let settings = AppSettings {
            latency_target: "nope".into(),
            ..AppSettings::default()
        };
        assert!(app.update_settings(settings).is_err());
        Ok(())
    }
}
