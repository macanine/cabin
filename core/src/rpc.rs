//! Wire protocol between the Mac app and `cabind` in the VM.
//!
//! Newline-delimited JSON over a byte stream (a Unix socket in the guest,
//! reached from the Mac through `ssh … cabind connect`):
//!
//! - client → daemon: `{"id": 1, "method": "workspace_create", "params": {…}}`
//! - daemon → client: `{"id": 1, "result": …}` or `{"id": 1, "error": {code, message, details?}}`
//! - daemon → client: `{"event": AppEvent}` (pushed to every connected client)
//! - daemon → client, first line:
//!   `{"hello": {"version": "…", "protocol": 3, "build": "<sha256>"}}`
//!
//! `build` is the SHA-256 of the running executable (`/proc/self/exe`), so the
//! Mac compares it with the bundled binary and knows the guest runs exactly
//! the build it ships, with no separate checksum round trip over SSH.
//!
//! Method names and camelCase params match the Tauri command names the UI
//! uses, so the Mac side forwards them unchanged.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::agents::{mcp_library, skill_library};
use crate::app::{App, NewChat, SessionPatch, WorkspacePatch};
use crate::error::{Error, Result};
use crate::model::{
    AgentKind, AppSettings, ChatAttachmentInput, ChatPermissionMode, EnvironmentSpec, PlanDecision,
    SetupAction,
};

pub use crate::hello::{Handshake, PROTOCOL, VERSION, build_id, check_hello, short_build};

mod transport;
pub use transport::{connect, serve};

fn params<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|error| Error::invalid(format!("bad params: {error}")))
}

fn ok<T: serde::Serialize>(value: T) -> Result<Value> {
    Ok(serde_json::to_value(value)?)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Id {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Kind {
    kind: AgentKind,
}

/// Run one method against the app. Every guest-side command the UI knows is
/// listed here; anything else is rejected.
pub fn dispatch(app: &App, method: &str, raw: Value) -> Result<Value> {
    match method {
        "diagnostic_events" => {
            let path = app
                .context()
                .paths
                .logs()
                .join(cabin_diagnostics::EVENT_FILE);
            Ok(json!(cabin_diagnostics::recent_records(&path, 32 * 1024)))
        }
        "app_snapshot" => ok(app.snapshot()?),
        "app_startup_snapshot" => ok(app.startup_snapshot()?),
        "monitor_processes" => {
            #[derive(Deserialize)]
            struct P {
                limit: usize,
            }
            ok(app.top_processes(params::<P>(raw)?.limit)?)
        }
        "network_ipv4_ensure" => ok(app.ensure_ipv4_network()?),
        "usage_sync" => ok(app.usage_sync()?),
        "usage_sync_prices" => ok(app.usage_sync_prices()?),
        "usage_dashboard" => {
            #[derive(Deserialize)]
            struct P {
                filter: cabin_usage::Filter,
            }
            ok(app.usage_dashboard(&params::<P>(raw)?.filter)?)
        }
        "usage_save_price" => {
            #[derive(Deserialize)]
            struct P {
                price: cabin_usage::Price,
            }
            ok(app.usage_save_price(&params::<P>(raw)?.price)?)
        }
        "machine_observation" => ok(crate::monitor::machine_observation()),
        // Read last, so the sample sits as close to the reply as possible.
        "clock" => {
            let timezone = crate::environment::timezone_from_files();
            ok(crate::model::GuestClock {
                now_ms: crate::util::now_millis(),
                timezone,
            })
        }

        "workspace_create" => {
            #[derive(Deserialize)]
            struct P {
                name: String,
                path: String,
            }
            let p: P = params(raw)?;
            ok(app.create_workspace(&p.name, &p.path)?)
        }
        "workspace_update" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                patch: WorkspacePatch,
            }
            let p: P = params(raw)?;
            ok(app.update_workspace(&p.id, p.patch)?)
        }
        "workspace_duplicate" => ok(app.duplicate_workspace(&params::<Id>(raw)?.id)?),
        "workspace_delete" => ok(app.delete_workspace(&params::<Id>(raw)?.id)?),

        "session_create" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                workspace_id: String,
                agent: AgentKind,
                name: Option<String>,
                prompt: Option<String>,
                cols: u16,
                rows: u16,
            }
            let p: P = params(raw)?;
            ok(app.create_session(
                &p.workspace_id,
                p.agent,
                p.name.as_deref(),
                p.prompt.as_deref(),
                p.cols,
                p.rows,
            )?)
        }
        "terminal_paste_image" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                mime_type: String,
                data: String,
            }
            let p = params::<P>(raw)?;
            ok(json!({ "path": app.terminal_paste_image(&p.id, &p.mime_type, &p.data)? }))
        }
        "terminal_create" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                workspace_id: String,
            }
            ok(app.create_terminal(&params::<P>(raw)?.workspace_id)?)
        }
        "terminal_snapshot" => ok(app.terminal_snapshot(&params::<Id>(raw)?.id)?),
        "terminal_attach" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                generation: u32,
                retry: bool,
                cols: u16,
                rows: u16,
            }
            let p: P = params(raw)?;
            ok(app.attach_terminal(&p.id, p.generation, p.retry, p.cols, p.rows)?)
        }
        "chat_create" => ok(app.create_chat(params::<NewChat>(raw)?)?),
        "worktree_refs" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                workspace_id: String,
            }
            ok(app.worktree_refs(&params::<P>(raw)?.workspace_id)?)
        }
        "worktree_remove" => ok(app.remove_session_worktree(&params::<Id>(raw)?.id)?),
        "chat_snapshot" => ok(app.chat_snapshot(&params::<Id>(raw)?.id)?),
        "agent_models" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                workspace_id: String,
                agent: AgentKind,
            }
            let p: P = params(raw)?;
            ok(app.agent_models(&p.workspace_id, p.agent)?)
        }
        "chat_set_model" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                model: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.chat_set_model(&p.id, p.model)?)
        }
        "chat_set_permission" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                permission: ChatPermissionMode,
            }
            let p: P = params(raw)?;
            ok(app.chat_set_permission(&p.id, p.permission)?)
        }
        "chat_attachment" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                attachment_id: String,
            }
            let p: P = params(raw)?;
            ok(app.chat_attachment(&p.id, &p.attachment_id)?)
        }
        "chat_send" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                text: String,
                #[serde(default)]
                attachments: Vec<ChatAttachmentInput>,
            }
            let p: P = params(raw)?;
            ok(app.chat_send(&p.id, &p.text, p.attachments)?)
        }
        "chat_steer" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                text: String,
                #[serde(default)]
                attachments: Vec<ChatAttachmentInput>,
            }
            let p: P = params(raw)?;
            ok(app.chat_steer(&p.id, &p.text, p.attachments)?)
        }
        "chat_set_option" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                option_id: String,
                value: String,
            }
            let p: P = params(raw)?;
            ok(app.chat_set_option(&p.id, &p.option_id, &p.value)?)
        }
        "chat_stop_task" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                task_id: String,
            }
            let p: P = params(raw)?;
            ok(app.chat_stop_task(&p.id, &p.task_id)?)
        }
        "chat_goal_control" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                action: String,
            }
            let p: P = params(raw)?;
            ok(app.chat_goal_control(&p.id, &p.action)?)
        }
        "chat_interrupt" => ok(app.chat_interrupt(&params::<Id>(raw)?.id)?),
        "chat_fail_active" => {
            #[derive(Deserialize)]
            struct P {
                message: String,
            }
            let p: P = params(raw)?;
            app.chat_fail_active(&p.message);
            ok(())
        }
        "chat_retry" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                item_id: String,
            }
            let p: P = params(raw)?;
            ok(app.chat_retry(&p.id, &p.item_id)?)
        }
        "chat_respond" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                request_id: String,
                decision: String,
                answers: Value,
            }
            let p: P = params(raw)?;
            ok(app.chat_respond(&p.id, &p.request_id, &p.decision, p.answers)?)
        }
        "chat_plan_respond" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                id: String,
                plan_id: String,
                expected_revision: i64,
                decision: PlanDecision,
                feedback: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.chat_plan_respond(
                &p.id,
                &p.plan_id,
                p.expected_revision,
                p.decision,
                p.feedback,
            )?)
        }
        "session_start" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                resume: bool,
                cols: u16,
                rows: u16,
            }
            let p: P = params(raw)?;
            ok(app.start_session(&p.id, p.resume, p.cols, p.rows)?)
        }
        "session_stop" => ok(app.stop_session(&params::<Id>(raw)?.id)?),
        "session_write" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                generation: u32,
                data: String,
            }
            let p: P = params(raw)?;
            ok(app.write_session(&p.id, p.generation, &p.data)?)
        }
        "session_resize" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                generation: u32,
                cols: u16,
                rows: u16,
            }
            let p: P = params(raw)?;
            ok(app.resize_session(&p.id, p.generation, p.cols, p.rows)?)
        }
        "session_update" => {
            #[derive(Deserialize)]
            struct P {
                id: String,
                patch: SessionPatch,
            }
            let p: P = params(raw)?;
            ok(app.update_session(&p.id, p.patch)?)
        }
        "session_duplicate" => ok(app.duplicate_session(&params::<Id>(raw)?.id)?),
        "session_delete" => ok(app.delete_session(&params::<Id>(raw)?.id)?),

        "agents_refresh" => ok(app.agents()),
        "agent_chat_capabilities" => ok(app.agent_chat_capabilities()),
        "acp_runtimes" => ok(app.acp_runtimes()),
        "acp_runtime_versions" => {
            #[derive(Deserialize)]
            struct P {
                agent: AgentKind,
            }
            ok(app.acp_runtime_versions(params::<P>(raw)?.agent)?)
        }
        "acp_runtime_install" => {
            #[derive(Deserialize)]
            struct P {
                agent: AgentKind,
                version: String,
                #[serde(default)]
                select: bool,
            }
            let p: P = params(raw)?;
            ok(app.acp_runtime_install(p.agent, &p.version, p.select)?)
        }
        "acp_runtime_select" | "acp_runtime_remove" => {
            #[derive(Deserialize)]
            struct P {
                agent: AgentKind,
                version: String,
            }
            let p: P = params(raw)?;
            if method == "acp_runtime_select" {
                ok(app.acp_runtime_select(p.agent, &p.version)?)
            } else {
                ok(app.acp_runtime_remove(p.agent, &p.version)?)
            }
        }
        "agent_config" => ok(app.agent_config(params::<Kind>(raw)?.kind)?),
        "agent_backups" => ok(app.agent_backups(params::<Kind>(raw)?.kind)?),
        "agent_health" => ok(app.agent_health(params::<Kind>(raw)?.kind)),
        "agent_set_model" => {
            #[derive(Deserialize)]
            struct P {
                kind: AgentKind,
                model: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.set_agent_model(p.kind, p.model)?)
        }
        "agent_save_config" => {
            #[derive(Deserialize)]
            struct P {
                kind: AgentKind,
                text: String,
            }
            let p: P = params(raw)?;
            ok(app.save_agent_config(p.kind, p.text)?)
        }
        "agent_restore_backup" => {
            #[derive(Deserialize)]
            struct P {
                kind: AgentKind,
                path: String,
            }
            let p: P = params(raw)?;
            ok(app.restore_agent_backup(p.kind, &p.path)?)
        }
        "agent_extensions" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                kind: AgentKind,
                #[serde(default)]
                workspace_id: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.agent_extensions(p.kind, p.workspace_id.as_deref())?)
        }
        "agent_catalog" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                workspace_id: String,
                kind: AgentKind,
                #[serde(default)]
                session_id: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.session_agent_catalog(&p.workspace_id, p.kind, p.session_id.as_deref())?)
        }
        "workspace_files" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                workspace_id: String,
                #[serde(default)]
                session_id: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.session_workspace_files(&p.workspace_id, p.session_id.as_deref())?)
        }
        "fs_list" | "fs_read" | "git_status" | "git_diff" | "git_stage" | "git_unstage"
        | "git_discard" | "git_commit" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                workspace_id: String,
                #[serde(default)]
                session_id: Option<String>,
                #[serde(default)]
                path: String,
                #[serde(default)]
                paths: Vec<String>,
                #[serde(default)]
                staged: bool,
                #[serde(default)]
                message: String,
                #[serde(default)]
                all: bool,
            }
            let p: P = params(raw)?;
            let (workspace, session) = (p.workspace_id.as_str(), p.session_id.as_deref());
            match method {
                "fs_list" => ok(app.fs_list(workspace, session, &p.path)?),
                "fs_read" => ok(app.fs_read(workspace, session, &p.path)?),
                "git_status" => ok(app.git_status(workspace, session)?),
                "git_diff" => ok(app.git_diff(workspace, session, &p.path, p.staged)?),
                "git_stage" => ok(app.git_stage(workspace, session, &p.paths, true)?),
                "git_unstage" => ok(app.git_stage(workspace, session, &p.paths, false)?),
                "git_discard" => ok(app.git_discard(workspace, session, &p.paths)?),
                _ => ok(app.git_commit(workspace, session, &p.message, p.all)?),
            }
        }
        "listening_ports" => ok(app.listening_ports()),
        "mcp_library" => ok(mcp_library::list(app.context())?),
        "mcp_library_edit" => {
            #[derive(Deserialize)]
            struct Params {
                name: String,
            }
            let Params { name } = params(raw)?;
            ok(mcp_library::edit(app.context(), &name)?)
        }
        "mcp_library_export" => {
            #[derive(Deserialize)]
            struct Params {
                name: Option<String>,
            }
            let Params { name } = params(raw)?;
            ok(mcp_library::export(app.context(), name.as_deref())?)
        }
        "mcp_library_parse" => {
            #[derive(Deserialize)]
            struct Params {
                text: String,
            }
            let Params { text } = params(raw)?;
            ok(mcp_library::parse_paste(&text)?)
        }
        "mcp_library_change" => ok(app.change_mcp_library(params(raw)?)?),
        "skill_library" => ok(skill_library::list(app.context())?),
        "skill_library_read" => {
            #[derive(Deserialize)]
            struct Params {
                folder: String,
            }
            let Params { folder } = params(raw)?;
            ok(skill_library::read(app.context(), &folder)?)
        }
        "skill_library_repositories" => {
            #[derive(Deserialize)]
            struct Params {
                repositories: Vec<skill_library::SkillRepository>,
            }
            let Params { repositories } = params(raw)?;
            ok(skill_library::save_repositories(
                app.context(),
                repositories,
            )?)
        }
        "skill_library_discover" => ok(skill_library::discover(app.context(), params(raw)?)?),
        "skill_library_registry" => {
            #[derive(Deserialize)]
            struct Params {
                query: String,
                offset: usize,
            }
            let Params { query, offset } = params(raw)?;
            ok(skill_library::registry(app.context(), &query, offset)?)
        }
        "skill_library_install" => {
            #[derive(Deserialize)]
            struct Params {
                source: skill_library::SkillInstall,
                targets: Vec<AgentKind>,
                #[serde(default)]
                mode: skill_library::SkillMode,
            }
            let Params {
                source,
                targets,
                mode,
            } = params(raw)?;
            ok(app.install_library_skill(source, targets, mode)?)
        }
        "skill_library_change" => ok(app.change_skill_library(params(raw)?)?),
        "skill_library_updates" => ok(skill_library::updates(app.context())?),
        "agent_mcp_save" => {
            #[derive(Deserialize)]
            struct P {
                kind: AgentKind,
                server: crate::model::McpServer,
                #[serde(default)]
                previous: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.agent_mcp_save(p.kind, p.server, p.previous)?)
        }
        "agent_mcp_remove" => {
            #[derive(Deserialize)]
            struct P {
                kind: AgentKind,
                name: String,
            }
            let p: P = params(raw)?;
            ok(app.agent_mcp_remove(p.kind, p.name)?)
        }
        "agent_mcp_set_enabled" => {
            #[derive(Deserialize)]
            struct P {
                kind: AgentKind,
                name: String,
                enabled: bool,
            }
            let p: P = params(raw)?;
            ok(app.agent_mcp_set_enabled(p.kind, p.name, p.enabled)?)
        }
        "agent_mcp_copy" => {
            #[derive(Deserialize)]
            struct P {
                kind: AgentKind,
                name: String,
                targets: Vec<AgentKind>,
            }
            let p: P = params(raw)?;
            ok(app.agent_mcp_copy(p.kind, p.name, p.targets)?)
        }
        "agent_skill_read" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                kind: AgentKind,
                path: String,
                #[serde(default)]
                workspace_id: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.agent_skill_read(p.kind, p.path, p.workspace_id.as_deref())?)
        }
        "agent_skill_delete" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                kind: AgentKind,
                path: String,
                #[serde(default)]
                workspace_id: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.agent_skill_delete(p.kind, p.path, p.workspace_id.as_deref())?)
        }
        "agent_skill_copy" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                kind: AgentKind,
                path: String,
                targets: Vec<AgentKind>,
                #[serde(default)]
                workspace_id: Option<String>,
            }
            let p: P = params(raw)?;
            ok(app.agent_skill_copy(p.kind, p.path, p.targets, p.workspace_id.as_deref())?)
        }
        "claude_usage" => {
            #[derive(Deserialize, Default)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct P {
                account_id: Option<String>,
            }
            let p = if raw.is_null() {
                P::default()
            } else {
                params(raw)?
            };
            if p.account_id
                .as_deref()
                .is_some_and(|id| id.trim().is_empty())
            {
                return Err(Error::invalid("Claude account ID is empty"));
            }
            ok(app.claude_usage(p.account_id.as_deref())?)
        }
        "codex_accounts" => ok(app.codex_accounts()?),
        "codex_export_account" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                account_id: String,
                format: String,
            }
            let p: P = params(raw)?;
            ok(app.codex_export_account(&p.account_id, &p.format)?)
        }
        "codex_import_account" => {
            #[derive(Deserialize)]
            struct P {
                #[serde(default)]
                contents: Option<String>,
                #[serde(default = "default_true")]
                activate: bool,
            }
            let p: P = params(raw)?;
            ok(app.codex_import_account(p.contents, p.activate)?)
        }
        "codex_switch_account" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                account_id: String,
            }
            ok(app.codex_switch_account(&params::<P>(raw)?.account_id)?)
        }
        "codex_remove_account" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                account_id: String,
            }
            ok(app.codex_remove_account(&params::<P>(raw)?.account_id)?)
        }
        "codex_refresh_quota" => ok(app.codex_refresh_quota()?),
        "codex_use_reset_card" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct P {
                credit_id: String,
                #[serde(default)]
                confirmed: bool,
            }
            let p: P = params(raw)?;
            ok(app.codex_use_reset_card(&p.credit_id, p.confirmed)?)
        }

        "environment_state" => ok(app.environment()),
        "network_probe" => ok(crate::network_probe::probe(
            app.environment(),
            raw.get("checkClosedBoundary")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        )),
        "environment_review" => {
            #[derive(Deserialize)]
            struct P {
                draft: EnvironmentSpec,
            }
            ok(app.review_environment(&params::<P>(raw)?.draft))
        }
        "environment_apply" => {
            #[derive(Deserialize)]
            struct P {
                spec: EnvironmentSpec,
            }
            ok(app.apply_environment(params::<P>(raw)?.spec)?)
        }

        "setup_checks" => ok(app.setup_checks()),
        "setup_run" => {
            #[derive(Deserialize)]
            struct P {
                action: SetupAction,
            }
            ok(app.run_setup_action(&params::<P>(raw)?.action)?)
        }
        "node_state" => ok(app.node_state()),
        "node_releases" => {
            #[derive(Deserialize)]
            struct P {
                #[serde(default)]
                mirror: Option<String>,
            }
            ok(app.node_releases(params::<P>(raw)?.mirror.as_deref())?)
        }
        "node_install" => {
            #[derive(Deserialize)]
            struct P {
                request: crate::node::InstallRequest,
            }
            ok(app.node_install(params::<P>(raw)?.request)?)
        }
        "node_use" | "node_remove" => {
            #[derive(Deserialize)]
            struct P {
                version: String,
            }
            let version = params::<P>(raw)?.version;
            if method == "node_use" {
                ok(app.node_use(&version)?)
            } else {
                ok(app.node_remove(&version)?)
            }
        }
        "operations_list" => ok(app.operations()?),
        "operation_cancel" => ok(app.cancel_operation(&params::<Id>(raw)?.id)?),
        "operation_retry" => ok(app.retry_operation(&params::<Id>(raw)?.id)?),
        "settings_update" => {
            #[derive(Deserialize)]
            struct P {
                settings: AppSettings,
            }
            ok(app.update_settings(params::<P>(raw)?.settings)?)
        }
        other => Err(Error::NotFound(format!("method {other}"))),
    }
}

fn default_true() -> bool {
    true
}

fn error_value(error: &Error) -> Value {
    let mut value = json!({ "code": error.code(), "message": error.to_string() });
    if let Some(details) = error.details() {
        value["details"] = Value::String(details);
    }
    value
}

/// Handle one request line and produce the response line.
pub fn handle_line(app: &App, line: &str) -> Option<Value> {
    #[derive(Deserialize)]
    struct Request {
        id: Value,
        method: String,
        #[serde(default)]
        params: Value,
    }
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return Some(json!({
                "id": null,
                "error": { "code": "invalid", "message": format!("bad request: {error}") }
            }));
        }
    };
    let params = if request.params.is_null() {
        json!({})
    } else {
        request.params
    };
    Some(match dispatch(app, &request.method, params) {
        Ok(result) => json!({ "id": request.id, "result": result }),
        Err(error) => json!({ "id": request.id, "error": error_value(&error) }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::FakeSystem;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    pub(super) fn app(dir: &Path) -> Arc<App> {
        Arc::new(App::with_context(Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system: Arc::new(FakeSystem::default()),
        }))
    }

    #[test]
    fn claude_usage_does_not_treat_malformed_scope_as_a_legacy_request() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        for raw in [
            json!({"accountId": 42}),
            json!({"accountId": " "}),
            json!({"account_id": "old"}),
        ] {
            assert!(matches!(
                dispatch(&app, "claude_usage", raw),
                Err(Error::Invalid(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn usage_rpc_saves_prices_and_returns_dashboard() -> Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("data"))?;
        let app = app(dir.path());
        dispatch(
            &app,
            "usage_save_price",
            json!({"price": {
                "model": "test-model", "input": 2.0, "output": 4.0,
                "cacheRead": 0.1, "cacheWrite": 0.2
            }}),
        )?;
        let dashboard = dispatch(
            &app,
            "usage_dashboard",
            json!({"filter": {
                "fromMs": null, "toMs": null, "agent": null, "model": null,
                "limit": 20, "offset": 0, "hostZone": "UTC", "clockOffset": 0
            }}),
        )?;
        assert_eq!(dashboard["total"], 0);
        assert!(
            dashboard["prices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|price| price["model"] == "test-model" && price["input"] == 2.0)
        );
        Ok(())
    }

    #[test]
    fn mcp_library_retains_secrets_and_unreadable_entries() -> Result<()> {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let claude = app.context().paths.home().join(".claude.json");
        fs::create_dir_all(claude.parent().unwrap())?;
        let original = json!({"mcpServers": {
            "shared": {"command": "demo", "env": {"TOKEN": "test-secret"}, "timeout": 37},
            "unknown": {"enabled": false},
        }});
        fs::write(&claude, original.to_string())?;
        let change = |value| dispatch(&app, "mcp_library_change", value);
        assert_eq!(change(json!({"type": "import"}))?["state"], "succeeded");
        let list = dispatch(&app, "mcp_library", json!({}))?;
        assert_eq!(list["rows"][1]["bindings"][0]["state"], "unreadable");
        assert!(!list.to_string().contains("test-secret"));
        assert_eq!(
            fs::metadata(app.context().paths.data().join("mcp-library.json"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        assert_eq!(
            change(
                json!({"type": "bind", "names": ["shared"], "targets": ["claude"], "enabled": false})
            )?["state"],
            "succeeded"
        );
        let file: Value = serde_json::from_str(&fs::read_to_string(&claude)?)?;
        assert!(file["mcpServers"].get("shared").is_none());
        assert_eq!(
            file["mcpServers"]["unknown"],
            original["mcpServers"]["unknown"]
        );
        let server = dispatch(&app, "mcp_library_edit", json!({"name":"shared"}))?;
        assert!(!server.to_string().contains("test-secret"));
        assert_eq!(
            change(
                json!({"type": "save", "servers": [server], "targets": ["claude", "codex"],
            "description": "Shared server", "replace": true})
            )?["state"],
            "succeeded"
        );
        let file: Value = serde_json::from_str(&fs::read_to_string(&claude)?)?;
        assert_eq!(file["mcpServers"]["shared"]["timeout"], 37);
        assert_eq!(file["mcpServers"]["shared"]["env"]["TOKEN"], "test-secret");
        let codex = app.context().paths.home().join(".codex/config.toml");
        assert!(fs::read_to_string(&codex)?.contains("test-secret"));
        fs::write(&codex, "[mcp_servers.shared]\ncommand = 'different'\n")?;
        let before = fs::read_to_string(&claude)?;
        let failed = change(json!({"type": "remove", "names": ["shared"]}))?;
        assert_eq!(failed["state"], "failed");
        assert_eq!(fs::read_to_string(claude)?, before);
        Ok(())
    }

    #[test]
    fn skill_zip_copy_preserves_local_edits_and_restores_bindings() -> Result<()> {
        use base64::Engine;
        use std::{
            fs,
            io::{Cursor, Write},
        };

        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        let zip_error = |error: zip::result::ZipError| Error::invalid(error.to_string());
        archive
            .start_file("demo/SKILL.md", options)
            .map_err(zip_error)?;
        archive.write_all(b"---\nname: demo\ndescription: Example skill\n---\n# Demo\n")?;
        archive
            .start_file("demo/references/example.txt", options)
            .map_err(zip_error)?;
        archive.write_all(b"included reference")?;
        let contents = base64::engine::general_purpose::STANDARD
            .encode(archive.finish().map_err(zip_error)?.into_inner());
        let record = dispatch(
            &app,
            "skill_library_install",
            json!({"source": {"type":"zip", "contents":contents}, "targets":["codex"], "mode":"copy"}),
        )?;
        assert_eq!(record["state"], "succeeded", "{record}");
        let copy = app.context().paths.home().join(".codex/skills/demo");
        let reference = copy.join("references/example.txt");
        assert!(!fs::symlink_metadata(&copy)?.is_symlink());
        assert_eq!(fs::read_to_string(&reference)?, "included reference");
        let change = |value| dispatch(&app, "skill_library_change", value);
        fs::write(&reference, "local edits")?;
        assert_ne!(
            change(json!({"type": "remove", "folders": ["demo"]}))?["state"],
            "succeeded"
        );
        assert_eq!(fs::read_to_string(&reference)?, "local edits");
        fs::write(&reference, "included reference")?;
        assert_eq!(
            change(json!({"type": "remove", "folders": ["demo"]}))?["state"],
            "succeeded"
        );
        assert!(!copy.exists());
        let list = dispatch(&app, "skill_library", json!({}))?;
        let backup = &list["backups"][0]["id"];
        assert!(backup.is_string());
        assert_eq!(
            change(json!({"type": "restore", "id": backup}))?["state"],
            "succeeded"
        );
        assert_eq!(fs::read_to_string(reference)?, "included reference");
        Ok(())
    }

    #[test]
    fn dispatch_maps_methods_and_rejects_unknown_ones() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let created = dispatch(
            &app,
            "workspace_create",
            json!({"name": "api", "path": "~/api"}),
        )?;
        assert_eq!(created["name"], "api");
        let snapshot = dispatch(&app, "app_startup_snapshot", json!({}))?;
        assert_eq!(snapshot["workspaces"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            snapshot["agents"].as_array().map(Vec::len),
            Some(AgentKind::ALL.len())
        );
        assert!(
            snapshot["agents"]
                .as_array()
                .unwrap()
                .iter()
                .all(|agent| agent["cliChecked"] == false)
        );
        let before = crate::util::now_millis();
        let clock = dispatch(&app, "clock", json!({}))?;
        let now = clock["nowMs"].as_i64().unwrap_or_default();
        assert!(now >= before && now <= crate::util::now_millis());
        assert!(clock.get("timezone").is_some());
        assert!(matches!(
            dispatch(&app, "rm_rf", json!({})),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            dispatch(&app, "workspace_create", json!({"name": 1})),
            Err(Error::Invalid(_))
        ));
        Ok(())
    }

    #[test]
    fn hello_is_ready_only_for_the_bundled_build() {
        let build = "a".repeat(64);
        let hello = |build: &str, version: &str| json!({ "hello": { "version": version, "protocol": PROTOCOL, "build": build } });
        assert_eq!(
            check_hello(&hello(&build, VERSION), &build),
            Handshake::Ready(VERSION.into())
        );
        assert!(matches!(
            check_hello(&hello(&"b".repeat(64), VERSION), &build),
            Handshake::Outdated(_)
        ));
        assert!(matches!(
            check_hello(&hello(&build, "0.0.1"), &build),
            Handshake::Outdated(_)
        ));
        let legacy = json!({ "hello": { "version": VERSION, "protocol": PROTOCOL } });
        assert!(matches!(
            check_hello(&legacy, &build),
            Handshake::Outdated(_)
        ));
    }

    #[test]
    fn socket_round_trip_with_hello_responses_and_events() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let app = app(dir.path());
        let socket = dir.path().join("run/agentd.sock");
        {
            let app = app.clone();
            let socket = socket.clone();
            std::thread::spawn(move || serve(app, &socket));
        }
        let started = Instant::now();
        let stream = loop {
            if let Ok(stream) = UnixStream::connect(&socket) {
                break stream;
            }
            assert!(started.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        };
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut writer = stream.try_clone()?;
        let mut lines = BufReader::new(stream).lines();
        let hello: Value = serde_json::from_str(&lines.next().unwrap()?)?;
        assert_eq!(hello["hello"]["protocol"], PROTOCOL);
        assert_eq!(hello["hello"]["build"].as_str().map(str::len), Some(64));

        writeln!(
            writer,
            r#"{{"id":7,"method":"workspace_create","params":{{"name":"w","path":"~/w"}}}}"#
        )?;
        writeln!(writer, r#"{{"id":8,"method":"nope"}}"#)?;
        let mut seen_result = false;
        let mut seen_error = false;
        let mut seen_event = false;
        while !(seen_result && seen_error && seen_event) {
            let value: Value = serde_json::from_str(&lines.next().unwrap()?)?;
            if value["id"] == 7 {
                assert_eq!(value["result"]["name"], "w");
                seen_result = true;
            } else if value["id"] == 8 {
                assert_eq!(value["error"]["code"], "notFound");
                seen_error = true;
            } else if value["event"]["type"] == "library" {
                seen_event = true;
            }
        }
        Ok(())
    }
}
