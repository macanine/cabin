use std::path::{Path, PathBuf};

use crate::agents;
use crate::environment;
use crate::error::{Error, Result};
use crate::model::{
    AgentConfigText, AgentKind, AgentModel, AgentStatus, AppEvent, BackupEntry, CodexAccounts,
    CodexResetResult, HealthReport, OperationRecord, SessionMode,
};
use crate::ops::Operation;

use super::App;

impl App {
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

    pub(super) fn run_and_refresh_agents(&self, operation: Operation) -> Result<OperationRecord> {
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

    pub(super) fn session_workspace_root(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
    ) -> Result<PathBuf> {
        let workspace = self.ctx.store.workspace(workspace_id)?;
        if let Some(id) = session_id {
            let session = self.ctx.store.session(id)?;
            if session.workspace_id != workspace_id {
                return Err(Error::invalid("Session belongs to another workspace"));
            }
            if let Some(tree) = session.worktree {
                return Ok(PathBuf::from(tree.path));
            }
        }
        Ok(PathBuf::from(workspace.path))
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

    pub fn session_workspace_files(
        &self,
        workspace_id: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<String>> {
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
        &self,
        workspace_id: &str,
        kind: AgentKind,
        session_id: Option<&str>,
    ) -> Result<crate::model::AgentCatalog> {
        let root = self.session_workspace_root(workspace_id, session_id)?;
        let extensions = agents::extensions::list(&self.ctx, kind, Some(&root));
        let adapter = agents::adapter(kind);
        // A fresh chat can offer disk skills before ACP reports live commands.
        // Preserve the first scope when entries insert the same reference;
        // their display names need not be unique (e.g. OpenCode's folder IDs).
        let mut seen = std::collections::HashSet::new();
        let mut commands: Vec<crate::model::AgentCommand> = extensions
            .skills
            .iter()
            .filter_map(|skill| {
                let insert = adapter.skill_insert(skill)?;
                if !seen.insert(insert.clone()) {
                    return None;
                }
                Some(crate::model::AgentCommand {
                    name: skill.name.clone(),
                    description: Some(skill.description.clone()).filter(|d| !d.is_empty()),
                    hint: None,
                    kind: "skill".into(),
                    source: Some(skill.scope.clone()),
                    insert,
                })
            })
            .collect();
        commands.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.insert.cmp(&b.insert)));
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

    pub fn change_mcp_library(
        &self,
        change: agents::mcp_library::McpChange,
    ) -> Result<OperationRecord> {
        self.ops
            .run(agents::mcp_library::operation(&self.ctx, change))
    }

    pub fn install_library_skill(
        &self,
        source: agents::skill_library::SkillInstall,
        targets: Vec<AgentKind>,
        mode: agents::skill_library::SkillMode,
    ) -> Result<OperationRecord> {
        self.ops.run(agents::skill_library::install_operation(
            &self.ctx, source, targets, mode,
        )?)
    }

    pub fn change_skill_library(
        &self,
        change: agents::skill_library::SkillChange,
    ) -> Result<OperationRecord> {
        self.ops
            .run(agents::skill_library::change_operation(&self.ctx, change))
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
}
