//! CLI agent adapters and shared agent configuration logic.
//!
//! Agent configs stay in their native files. Every write goes through an
//! operation with a backup, an atomic replace, a re-read check and rollback.

pub(crate) mod antigravity;
mod claude;
mod codex;
pub mod extensions;
pub mod mcp_library;
mod opencode;
pub mod skill_library;
mod skill_sources;
mod pi;

pub(crate) use claude::usage as claude_usage;
pub(crate) use codex::{
    accounts as codex_accounts, export_account as codex_export_account,
    import_account as codex_import_account,
    refresh_quota as codex_refresh_quota, remove_account as codex_remove_account,
    switch_account as codex_switch_account, use_reset_card as codex_use_reset_card,
};

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::context::Context;
use crate::error::{Error, Result};
use crate::fsx;
use crate::model::{AgentConfigText, AgentKind, AgentStatus, BackupEntry, ClaudeLogin, HealthReport};
use crate::ops::{Operation, Step, StepCtx};
use crate::system::CommandSpec;

/// Static facts about one agent CLI.
pub struct AgentSpec {
    pub kind: AgentKind,
    pub name: &'static str,
    pub binary: &'static str,
    /// CLI package or distribution shown in Setup.
    pub package: &'static str,
    /// Config file relative to the user's home directory.
    pub config: &'static str,
    pub model_key: &'static str,
    /// Credential files relative to home, used as a fallback when the CLI
    /// cannot report its own login status.
    pub auth_files: &'static [&'static str],
    /// Environment variables that authenticate the CLI.
    pub auth_env: &'static [&'static str],
}

/// Options for building a session command line.
#[derive(Debug, Clone, Default)]
pub struct LaunchOptions {
    pub model: Option<String>,
    /// Continue the most recent conversation in the working directory.
    pub resume: bool,
    /// First task for a new conversation (ignored when resuming).
    pub prompt: Option<String>,
}

/// A prompt passed as a positional argument must not look like a flag.
pub fn positional(prompt: &str) -> String {
    if prompt.starts_with('-') {
        format!(" {prompt}")
    } else {
        prompt.to_string()
    }
}

pub trait AgentAdapter: Send + Sync {
    fn spec(&self) -> &'static AgentSpec;

    fn requires_node(&self) -> bool {
        true
    }

    fn launch_env(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Text the composer inserts for a skill (agents differ in how a skill is invoked).
    fn skill_insert(&self, name: &str) -> String {
        format!("/{name} ")
    }

    fn read_model(&self, config: &Value) -> Option<String> {
        config
            .get(self.spec().model_key)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn write_model(&self, config: &mut Value, model: Option<&str>) -> Result<()> {
        let object = config
            .as_object_mut()
            .ok_or_else(|| Error::invalid("The config file is not a JSON object"))?;
        match model {
            Some(model) => {
                object.insert(self.spec().model_key.into(), Value::String(model.into()));
            }
            None => {
                object.remove(self.spec().model_key);
            }
        }
        Ok(())
    }

    /// Arguments after the binary for an interactive session.
    fn launch_args(&self, options: &LaunchOptions) -> Vec<String> {
        let mut args = Vec::new();
        if let Some(model) = options.model.as_deref().filter(|m| !m.is_empty()) {
            args.push("--model".into());
            args.push(model.into());
        }
        if options.resume {
            args.push("--continue".into());
        } else if let Some(prompt) = options
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            args.extend(self.prompt_args(prompt));
        }
        args
    }

    /// Arguments that start an interactive session with a first prompt.
    fn prompt_args(&self, prompt: &str) -> Vec<String> {
        vec![positional(prompt)]
    }

    /// Where and how the agent keeps its own MCP servers; `None` when it has
    /// no built-in MCP support.
    fn mcp(&self) -> Option<&'static dyn extensions::McpCodec> {
        None
    }

    /// The agent's user skill folder (`<dir>/<name>/SKILL.md`).
    fn skills_dir(&self, ctx: &Context) -> PathBuf;

    /// The skill folder inside a workspace, relative to its root.
    fn project_skills_dir(&self) -> &'static str;

    /// Folders to scan in order of precedence. Installs use `skills_dir`.
    fn skill_dirs(&self, ctx: &Context, workspace: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
        let mut dirs = vec![(self.skills_dir(ctx), "user")];
        if let Some(root) = workspace {
            dirs.push((root.join(self.project_skills_dir()), "project"));
        }
        dirs
    }
}

pub fn adapter(kind: AgentKind) -> &'static dyn AgentAdapter {
    match kind {
        AgentKind::Claude => &claude::Claude,
        AgentKind::Codex => &codex::Codex,
        AgentKind::Opencode => &opencode::OpenCode,
        AgentKind::Pi => &pi::Pi,
        AgentKind::Antigravity => &antigravity::Antigravity,
    }
}

pub fn config_path(ctx: &Context, kind: AgentKind) -> PathBuf {
    ctx.paths.home().join(adapter(kind).spec().config)
}

fn backup_dir(ctx: &Context, kind: AgentKind) -> PathBuf {
    let root = ctx.paths.backups().join("agents");
    let dir = root.join(kind.as_str());
    // Claude Code's backups were kept under its earlier id, `primary`.
    if kind == AgentKind::Claude && !dir.exists() {
        let _ = std::fs::rename(root.join("primary"), &dir);
    }
    dir
}

/// Locate the agent binary on `PATH` and the usual per-user bin dirs.
pub fn find_binary(ctx: &Context, kind: AgentKind) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let name = adapter(kind).spec().binary;
    ctx.paths.child_path().split(':').find_map(|dir| {
        let candidate = Path::new(dir).join(name);
        let meta = std::fs::metadata(&candidate).ok()?;
        (meta.is_file() && meta.permissions().mode() & 0o111 != 0).then_some(candidate)
    })
}

fn version(ctx: &Context, binary: &Path, adapter: &dyn AgentAdapter) -> Option<String> {
    let spec = CommandSpec::new([binary.display().to_string(), "--version".into()])
        .env(adapter.launch_env())
        .timeout(Duration::from_secs(15));
    let out = ctx.system.run(&spec).ok()?;
    out.success()
        .then(|| out.stdout.lines().next().unwrap_or("").trim().to_string())
        .filter(|line| !line.is_empty())
}

/// Parse a config file. A missing file is an empty object.
fn read_config(path: &Path, kind: AgentKind) -> Result<(bool, Value)> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok((true, Value::Object(Default::default()))),
        Ok(text) if kind == AgentKind::Codex => {
            let doc = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
                Error::invalid(format!("{} is not valid TOML ({error})", path.display()))
            })?;
            let mut value = serde_json::Map::new();
            if let Some(model) = doc.get("model").and_then(|item| item.as_str()) {
                value.insert("model".into(), Value::String(model.into()));
            }
            Ok((true, Value::Object(value)))
        }
        Ok(text) => {
            let value: Value = serde_json::from_str(&text).map_err(|error| {
                Error::invalid(format!(
                    "{} is not plain JSON ({error}); edit it by hand or restore a backup",
                    path.display()
                ))
            })?;
            Ok((true, value))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((false, Value::Object(Default::default())))
        }
        Err(error) => Err(Error::path("read", path, error)),
    }
}

fn file_or_env_auth(ctx: &Context, spec: &AgentSpec) -> (bool, String) {
    for file in spec.auth_files {
        if ctx.paths.home().join(file).is_file() {
            return (true, format!("Signed in (~/{file})"));
        }
    }
    for var in spec.auth_env {
        if std::env::var(var).is_ok_and(|value| !value.is_empty()) {
            return (true, format!("{var} is set"));
        }
    }
    (
        false,
        format!(
            "Not signed in. Start a session and follow the login prompt{}.",
            spec.auth_env
                .first()
                .map(|var| format!(", or set {var}"))
                .unwrap_or_default()
        ),
    )
}

fn auth(ctx: &Context, spec: &AgentSpec, binary: Option<&Path>) -> (bool, String) {
    match (spec.kind, binary) {
        (AgentKind::Codex, Some(binary)) => codex::auth_status(ctx, binary),
        (AgentKind::Opencode, Some(binary)) => opencode::auth_status(ctx, binary),
        (AgentKind::Antigravity, _) => antigravity::auth(ctx, binary),
        _ => file_or_env_auth(ctx, spec),
    }
}

pub fn status(ctx: &Context, kind: AgentKind) -> AgentStatus {
    read_status(ctx, kind, true)
}

/// Installed agents and their configured models are available without a CLI launch.
pub fn inventory(ctx: &Context) -> Vec<AgentStatus> {
    AgentKind::ALL
        .into_iter()
        .map(|kind| read_status(ctx, kind, false))
        .collect()
}

fn read_status(ctx: &Context, kind: AgentKind, cli_checked: bool) -> AgentStatus {
    let adapter = adapter(kind);
    let spec = adapter.spec();
    let path = config_path(ctx, kind);
    let binary = find_binary(ctx, kind);
    let version = binary
        .as_deref()
        .filter(|_| cli_checked)
        .and_then(|bin| version(ctx, bin, adapter));
    let (config_exists, model, config_error) = match read_config(&path, kind) {
        Ok((exists, value)) => (exists, adapter.read_model(&value), None),
        Err(error) => (true, None, Some(error.to_string())),
    };
    let (authenticated, auth_detail, claude_login) = if !cli_checked {
        (false, String::new(), None)
    } else if kind == AgentKind::Claude {
        let (login, detail) = claude::inspect_account(ctx, binary.as_deref());
        (matches!(login, ClaudeLogin::Active { .. }), detail.into(), Some(login))
    } else {
        let (authenticated, detail) = auth(ctx, spec, binary.as_deref());
        (authenticated, detail, None)
    };
    AgentStatus {
        kind,
        name: spec.name.into(),
        package: spec.package.into(),
        requires_node: adapter.requires_node(),
        installed: binary.is_some(),
        binary: binary.map(|path| path.display().to_string()),
        cli_checked,
        version,
        config_path: path.display().to_string(),
        config_exists,
        config_error,
        model_key: spec.model_key.into(),
        model,
        authenticated,
        auth_detail,
        claude_login,
    }
}

/// Status of every agent, probed in parallel (`--version` can be slow).
pub fn all_status(ctx: &Context) -> Vec<AgentStatus> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = AgentKind::ALL
            .iter()
            .map(|kind| scope.spawn(move || status(ctx, *kind)))
            .collect();
        handles
            .into_iter()
            .zip(AgentKind::ALL)
            .map(|(handle, kind)| handle.join().unwrap_or_else(|_| status(ctx, kind)))
            .collect()
    })
}

pub fn config_text(ctx: &Context, kind: AgentKind) -> Result<AgentConfigText> {
    let path = config_path(ctx, kind);
    let (exists, text) = match std::fs::read_to_string(&path) {
        Ok(text) => (true, text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (
            false,
            if kind == AgentKind::Codex {
                String::new()
            } else {
                "{\n}\n".into()
            },
        ),
        Err(error) => return Err(Error::path("read", &path, error)),
    };
    Ok(AgentConfigText {
        kind,
        path: path.display().to_string(),
        exists,
        text,
    })
}

pub fn backups(ctx: &Context, kind: AgentKind) -> Result<Vec<BackupEntry>> {
    let dir = backup_dir(ctx, kind);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::path("list", &dir, error)),
    };
    let mut list: Vec<BackupEntry> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let created_at = name.split_once('-')?.0.parse().ok()?;
            Some(BackupEntry {
                path: entry.path().display().to_string(),
                size: entry.metadata().ok()?.len(),
                name,
                created_at,
            })
        })
        .collect();
    list.sort_by_key(|entry| std::cmp::Reverse(entry.created_at));
    Ok(list)
}

pub fn health(ctx: &Context, kind: AgentKind) -> HealthReport {
    let status = status(ctx, kind);
    let mut lines = Vec::new();
    let mut ok = true;
    match (&status.binary, &status.version) {
        (Some(binary), Some(version)) => lines.push(format!("✓ {binary} runs ({version})")),
        (Some(binary), None) => {
            ok = false;
            lines.push(format!("✗ {binary} exists but `--version` failed"));
        }
        (None, _) => {
            ok = false;
            lines.push(format!(
                "✗ `{}` not found; install it from Setup",
                adapter(kind).spec().binary
            ));
        }
    }
    match &status.config_error {
        Some(error) => {
            ok = false;
            lines.push(format!("✗ {error}"));
        }
        None if status.config_exists => lines.push(format!(
            "✓ {} is valid {}",
            status.config_path,
            if kind == AgentKind::Codex {
                "TOML"
            } else {
                "JSON"
            }
        )),
        None => lines.push(format!(
            "• {} does not exist yet (defaults apply)",
            status.config_path
        )),
    }
    lines.push(format!(
        "{} {}",
        if status.authenticated { "✓" } else { "•" },
        status.auth_detail
    ));
    HealthReport { ok, lines }
}

// ------------------------------------------------------------ config writes

type Transform = Box<dyn FnMut(Option<&str>) -> Result<String> + Send>;

/// Replace a config file atomically with a backup and post-write check.
struct ConfigWrite {
    path: PathBuf,
    kind: AgentKind,
    backup_dir: PathBuf,
    transform: Transform,
    content: Option<String>,
    backup: Option<PathBuf>,
}

impl Step for ConfigWrite {
    fn name(&self) -> String {
        format!("Write {}", self.path.display())
    }

    fn prepare(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let current = match std::fs::read_to_string(&self.path) {
            Ok(text) => Some(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::path("read", &self.path, error)),
        };
        let content = (self.transform)(current.as_deref())?;
        if self.kind == AgentKind::Codex {
            content.parse::<toml_edit::DocumentMut>().map_err(|error| {
                Error::invalid(format!("The new config is not valid TOML: {error}"))
            })?;
        } else {
            serde_json::from_str::<Value>(&content).map_err(|error| {
                Error::invalid(format!("The new config is not valid JSON: {error}"))
            })?;
        }
        self.backup = fsx::backup(&self.path, &self.backup_dir)?;
        match &self.backup {
            Some(backup) => ctx.log(format!("Backed up to {}", backup.display())),
            None => ctx.log("No existing file; nothing to back up"),
        }
        self.content = Some(content);
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let content = self.content.as_deref().unwrap_or_default();
        fsx::atomic_write(&self.path, content.as_bytes(), None)?;
        ctx.log(format!("Wrote {} bytes", content.len()));
        Ok(())
    }

    fn verify(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let written = std::fs::read_to_string(&self.path)
            .map_err(|error| Error::path("read back", &self.path, error))?;
        if Some(written.as_str()) != self.content.as_deref() {
            return Err(Error::Internal(
                "The file changed while it was being written".into(),
            ));
        }
        Ok(())
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        fsx::restore(&self.path, self.backup.as_deref())?;
        ctx.log("Restored the previous file");
        Ok(())
    }
}

fn config_write(ctx: &Context, kind: AgentKind, transform: Transform) -> ConfigWrite {
    ConfigWrite {
        path: config_path(ctx, kind),
        kind,
        backup_dir: backup_dir(ctx, kind),
        transform,
        content: None,
        backup: None,
    }
}

fn pretty(value: &Value) -> Result<String> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(text)
}

/// Set (or clear) the agent's default model in its native config.
pub fn set_model_operation(ctx: &Context, kind: AgentKind, model: Option<String>) -> Operation {
    let adapter = adapter(kind);
    let model = model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let title = format!(
        "{}: {}",
        adapter.spec().name,
        match &model {
            Some(model) => format!("set model to {model}"),
            None => "clear model".into(),
        }
    );
    let request = serde_json::json!({ "agent": kind, "model": model });
    let transform: Transform = Box::new(move |current| {
        if kind == AgentKind::Codex {
            let mut doc = current
                .unwrap_or("")
                .parse::<toml_edit::DocumentMut>()
                .map_err(|error| {
                    Error::invalid(format!("The existing config is not valid TOML ({error})"))
                })?;
            match model.as_deref() {
                Some(model) => doc["model"] = toml_edit::value(model),
                None => {
                    doc.remove("model");
                }
            }
            return Ok(doc.to_string());
        }
        let mut value = match current {
            Some(text) if !text.trim().is_empty() => serde_json::from_str::<Value>(text)
                .map_err(|error| Error::invalid(format!("The existing config is not plain JSON ({error}); fix it in the editor first")))?,
            _ => Value::Object(Default::default()),
        };
        adapter.write_model(&mut value, model.as_deref())?;
        pretty(&value)
    });
    Operation::new("agent.setModel", title)
        .key(format!("agent-config-{}", kind.as_str()))
        .request(request)
        .step(config_write(ctx, kind, transform))
}

/// Replace the whole config file with `text` (must be valid JSON).
pub fn save_config_operation(ctx: &Context, kind: AgentKind, text: String) -> Operation {
    let name = adapter(kind).spec().name;
    let transform: Transform = Box::new(move |_| {
        let mut text = text.clone();
        if !text.ends_with('\n') {
            text.push('\n');
        }
        Ok(text)
    });
    Operation::new("agent.saveConfig", format!("{name}: save config"))
        .key(format!("agent-config-{}", kind.as_str()))
        .step(config_write(ctx, kind, transform))
}

pub fn restore_backup_operation(ctx: &Context, kind: AgentKind, backup: &str) -> Result<Operation> {
    let backup = PathBuf::from(backup);
    let dir = backup_dir(ctx, kind);
    if backup.parent() != Some(dir.as_path()) || !backup.is_file() {
        return Err(Error::NotFound("backup".into()));
    }
    let label = backup
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let transform: Transform = Box::new(move |_| {
        std::fs::read_to_string(&backup).map_err(|error| Error::path("read", &backup, error))
    });
    Ok(Operation::new(
        "agent.restoreBackup",
        format!("{}: restore {label}", adapter(kind).spec().name),
    )
    .key(format!("agent-config-{}", kind.as_str()))
    .step(config_write(ctx, kind, transform)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OperationState;
    use crate::ops::OperationManager;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::FakeSystem;
    use std::sync::Arc;

    fn context(dir: &Path) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system: Arc::new(FakeSystem::default()),
        }
    }

    #[test]
    fn data_from_the_earlier_primary_id_still_loads() -> Result<()> {
        assert_eq!(AgentKind::parse("primary")?, AgentKind::Claude);
        assert_eq!(AgentKind::parse("claude")?, AgentKind::Claude);
        assert_eq!(serde_json::to_string(&AgentKind::Claude)?, "\"claude\"");
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let old = ctx.paths.backups().join("agents/primary");
        std::fs::create_dir_all(&old)?;
        std::fs::write(old.join("1-settings.json"), "{}")?;
        assert_eq!(backups(&ctx, AgentKind::Claude)?.len(), 1);
        assert!(!old.exists());
        Ok(())
    }

    #[test]
    fn set_model_keeps_other_keys_and_backs_up() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let path = config_path(&ctx, AgentKind::Claude);
        fsx::atomic_write(&path, br#"{"theme":"dark"}"#, None)?;
        let ops = OperationManager::new(ctx.store.clone());
        let record = ops.run(set_model_operation(
            &ctx,
            AgentKind::Claude,
            Some("opus".into()),
        ))?;
        assert_eq!(record.state, OperationState::Succeeded);
        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        assert_eq!(value["model"], "opus");
        assert_eq!(value["theme"], "dark");
        assert_eq!(backups(&ctx, AgentKind::Claude)?.len(), 1);
        assert_eq!(
            status(&ctx, AgentKind::Claude).model.as_deref(),
            Some("opus")
        );
        Ok(())
    }

    #[test]
    fn codex_model_updates_toml_without_losing_settings() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let path = config_path(&ctx, AgentKind::Codex);
        fsx::atomic_write(
            &path,
            b"# keep this\nmodel = 'old'\n[features]\nfoo = true\n",
            None,
        )?;
        let ops = OperationManager::new(ctx.store.clone());
        assert_eq!(
            ops.run(set_model_operation(
                &ctx,
                AgentKind::Codex,
                Some("new".into())
            ))?
            .state,
            OperationState::Succeeded
        );
        let text = std::fs::read_to_string(&path)?;
        assert!(text.contains("# keep this") && text.contains("foo = true"));
        assert_eq!(status(&ctx, AgentKind::Codex).model.as_deref(), Some("new"));
        assert_eq!(backups(&ctx, AgentKind::Codex)?.len(), 1);
        Ok(())
    }

    #[test]
    fn invalid_existing_config_is_not_overwritten() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let path = config_path(&ctx, AgentKind::Opencode);
        fsx::atomic_write(&path, b"{ // jsonc\n}", None)?;
        let ops = OperationManager::new(ctx.store.clone());
        let record = ops.run(set_model_operation(
            &ctx,
            AgentKind::Opencode,
            Some("x/y".into()),
        ))?;
        assert_eq!(record.state, OperationState::Failed);
        assert_eq!(std::fs::read_to_string(&path)?, "{ // jsonc\n}");
        Ok(())
    }

    #[test]
    fn restore_backup_round_trip() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let ops = OperationManager::new(ctx.store.clone());
        let path = config_path(&ctx, AgentKind::Pi);
        fsx::atomic_write(&path, b"{\"defaultModel\":\"a\"}", None)?;
        ops.run(set_model_operation(&ctx, AgentKind::Pi, Some("b".into())))?;
        let backup = backups(&ctx, AgentKind::Pi)?.remove(0);
        let record = ops.run(restore_backup_operation(&ctx, AgentKind::Pi, &backup.path)?)?;
        assert_eq!(record.state, OperationState::Succeeded);
        assert_eq!(status(&ctx, AgentKind::Pi).model.as_deref(), Some("a"));
        assert!(restore_backup_operation(&ctx, AgentKind::Pi, "/etc/passwd").is_err());
        Ok(())
    }

    #[test]
    fn launch_args_use_flags_not_shell_strings() {
        let args = adapter(AgentKind::Claude).launch_args(&LaunchOptions {
            model: Some("sonnet; rm -rf /".into()),
            resume: true,
            prompt: Some("ignored when resuming".into()),
        });
        assert_eq!(args, ["--model", "sonnet; rm -rf /", "--continue"]);
    }

    #[test]
    fn first_prompt_is_passed_per_agent() {
        let options = LaunchOptions {
            prompt: Some("-fix the tests".into()),
            ..LaunchOptions::default()
        };
        assert_eq!(
            adapter(AgentKind::Claude).launch_args(&options),
            [" -fix the tests"]
        );
        assert_eq!(
            adapter(AgentKind::Pi).launch_args(&options),
            [" -fix the tests"]
        );
        assert_eq!(
            adapter(AgentKind::Opencode).launch_args(&options),
            ["--prompt", "-fix the tests"]
        );
    }
}
