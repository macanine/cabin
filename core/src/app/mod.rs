//! `App`: the facade the Tauri layer calls. It owns every subsystem and turns
//! subsystem callbacks into one stream of typed `AppEvent`s.

use crate::util::{LockExt, RwLockExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::Deserialize;

use crate::chat::ChatSupervisor;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{AgentKind, AppEvent, ChatAttachmentInput, ChatPermissionMode, ModelOverride};
use crate::monitor::Monitor;
use crate::ops::OperationManager;
use crate::sessions::Supervisor;
use crate::util::now_millis;

mod agents;
mod chat;
mod environment;
mod files;
mod machine;
mod node;
mod session;
mod setup;
mod usage;
mod workspace;

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
                    crate::fetch::http_error("Could not update model prices", error).to_string()
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

fn chat_permission_modes(agent: AgentKind) -> Vec<ChatPermissionMode> {
    use ChatPermissionMode::*;
    match agent {
        AgentKind::Claude | AgentKind::Codex | AgentKind::Antigravity => {
            vec![ApprovalRequired, AutoAcceptEdits, Auto, Plan, FullAccess]
        }
        AgentKind::Opencode => vec![ApprovalRequired, AutoAcceptEdits, FullAccess],
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
    use crate::agents;
    use crate::model::{AppSettings, OperationState, SessionStatus};
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::FakeSystem;
    use base64::Engine as _;
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
        while !app
            .terminal_snapshot(&session.id)?
            .data
            .contains("--continue")
        {
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
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );

        assert!(
            app.terminal_paste_image(&terminal.id, "image/svg+xml", &png)
                .is_err()
        );
        assert!(
            app.terminal_paste_image(&terminal.id, "image/png", "not base64!")
                .is_err()
        );
        assert!(
            app.terminal_paste_image("missing", "image/png", &png)
                .is_err()
        );
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
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(&folder)
                .args(args)
                .output()?;
            if !output.status.success() {
                return Err(Error::invalid(
                    String::from_utf8_lossy(&output.stderr).to_string(),
                ));
            }
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
        let tree =
            crate::worktree::create(&app.ops, &folder, app.ctx.paths.data(), &session.id, "HEAD")?;
        app.ctx
            .store
            .set_session_worktree(&session.id, Some(&tree))?;
        std::fs::write(Path::new(&tree.path).join("isolated.txt"), "new")?;
        assert!(
            app.session_workspace_files(&workspace.id, Some(&session.id))?
                .contains(&"isolated.txt".into())
        );
        assert!(
            !app.workspace_files(&workspace.id)?
                .contains(&"isolated.txt".into())
        );
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
        assert_eq!(
            app.attach_terminal(&terminal.id, 0, false, 80, 24)?
                .generation,
            shell.generation
        );
        assert_eq!(
            (terminal.name.as_str(), terminal.agent),
            ("Terminal 1", None)
        );
        // Stored as a terminal, listed with the other sessions.
        assert_eq!(app.snapshot()?.sessions[0].agent, None);
        assert_eq!(app.create_terminal(&workspace.id)?.name, "Terminal 2");

        app.write_session(
            &terminal.id,
            shell.generation,
            "echo \"at:$(pwd) sum:$((40+2))\"\n",
        )?;
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
    fn native_login_reuses_the_guest_workspace_and_terminal() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let home = app.ctx.paths.home();
        let binary = home.join(".local/bin/codex");
        std::fs::create_dir_all(home.join(".local/bin"))?;
        std::fs::write(&binary, "#!/bin/sh\nprintf 'cwd:%s args:%s\\n' \"$PWD\" \"$*\"\nread ignored\n")?;
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o700))?;
        let project_path = dir.path().join("project");
        let project = app.create_workspace("Project", &project_path.display().to_string())?;
        let session = app.open_agent_terminal(AgentKind::Codex, true, 80, 24)?;
        assert_ne!(session.workspace_id, project.id);
        assert_eq!(session.mode, crate::model::SessionMode::Login);
        assert!(session.status.is_live());

        let workspace = app.ctx.store.workspace(&session.workspace_id)?;
        assert_eq!(Path::new(&workspace.path), home.join("workspace"));
        let reused = app.open_agent_terminal(AgentKind::Codex, true, 80, 24)?;
        assert_eq!(reused.id, session.id);
        assert_eq!(app.ctx.store.workspaces()?.len(), 2);

        let deadline = Instant::now() + Duration::from_secs(5);
        let command = "args:login --device-auth";
        loop {
            let terminal = app.terminal_snapshot(&session.id)?;
            if terminal.data.contains(command) {
                assert!(terminal.data.contains("/home/workspace args:"), "{}", terminal.data);
                break;
            }
            assert!(Instant::now() < deadline, "{}", terminal.data);
            std::thread::sleep(Duration::from_millis(20));
        }
        let generation = app.terminal_snapshot(&session.id)?.generation;
        app.write_session(&session.id, generation, "done\n")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.session(&session.id)?.status.is_live() {
            assert!(Instant::now() < deadline, "login command did not exit");
            std::thread::sleep(Duration::from_millis(20));
        }

        let regular = app.open_agent_terminal(AgentKind::Codex, false, 80, 24)?;
        assert_eq!(regular.workspace_id, workspace.id);
        assert_eq!(regular.mode, crate::model::SessionMode::Terminal);
        assert!(app.open_agent_terminal(AgentKind::Codex, true, 80, 24).is_err());
        app.stop_session(&regular.id)?;
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
    fn opencode_catalog_references_case_sensitive_folder_ids_not_display_names() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let root = dir.path().join("w");
        let kind = AgentKind::Opencode;
        let user = agents::extensions::user_skills_dir(app.context(), kind);
        let project = root.join(agents::adapter(kind).project_skills_dir());
        for (parent, id, description) in [
            (&user, "PDF-One", "User skill"),
            (&project, "PDF-One", "Project overrides the same ID"),
            (&project, "pdf-two", "Different ID"),
            (&project, "invalid id", "Cannot be a mention"),
        ] {
            let folder = parent.join(id);
            std::fs::create_dir_all(&folder)?;
            std::fs::write(
                folder.join("SKILL.md"),
                format!("---\nname: PDF toolkit\ndescription: {description}\n---\n\nFixture\n"),
            )?;
        }
        let workspace = app.create_workspace("W", &root.display().to_string())?;
        let catalog = app.agent_catalog(&workspace.id, kind)?;
        assert_eq!(catalog.commands.len(), 2);
        let project_skill = &catalog.commands[0];
        assert_eq!(project_skill.name, "PDF toolkit");
        assert_eq!(project_skill.insert, "@PDF-One ");
        assert_eq!(project_skill.source.as_deref(), Some("project"));
        assert_eq!(
            project_skill.description.as_deref(),
            Some("Project overrides the same ID")
        );
        assert_eq!(catalog.commands[1].insert, "@pdf-two ");
        assert_eq!(catalog.commands[1].source.as_deref(), Some("project"));
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
            app.terminal_snapshot(&session.id)?
                .data
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
