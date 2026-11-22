//! The ACP runtime of each agent: which npm package speaks ACP for it, the
//! versions installed side by side under `<data>/acp/<agent>/<version>`, and
//! the one new chats use.
//!
//! Claude, Codex and Pi run through an adapter that drives the agent CLI from
//! Setup (`CLAUDE_CODE_EXECUTABLE`, `CODEX_PATH`, `PI_ACP_PI_COMMAND`), so the
//! user's own install, login and config apply and only one agent CLI exists.
//! The adapters' bundled CLIs are npm optional dependencies and are skipped
//! (`--omit=optional`). OpenCode speaks ACP itself (`opencode2 acp`).
//! Antigravity runs `agy acp` from Setup without a separate runtime.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;

use crate::agents;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{AcpRuntime, AgentKind};
use crate::ops::{Operation, Step, StepCtx};
use crate::system::CommandSpec;

const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const VIEW_TIMEOUT: Duration = Duration::from_secs(60);

pub struct Package {
    pub agent: AgentKind,
    pub package: &'static str,
    pub bin: &'static str,
    pub license: &'static str,
    /// Tested with this release; used until the user picks another.
    pub default_version: &'static str,
    /// Skip optional dependencies (the adapters' bundled agent CLIs).
    pub omit_optional: bool,
    /// The adapter runs the agent CLI from Setup.
    pub uses_agent_cli: bool,
    /// Environment variable that points at a binary instead of the managed install.
    pub override_var: &'static str,
}

pub const PACKAGES: [Package; 4] = [
    Package {
        agent: AgentKind::Claude,
        package: "@agentclientprotocol/claude-agent-acp",
        bin: "claude-agent-acp",
        license: "Apache-2.0",
        default_version: "0.108.1",
        omit_optional: true,
        uses_agent_cli: true,
        override_var: "CABIN_CLAUDE_ACP_BIN",
    },
    Package {
        agent: AgentKind::Codex,
        package: "@agentclientprotocol/codex-acp",
        bin: "codex-acp",
        license: "Apache-2.0",
        default_version: "2.7.0",
        omit_optional: true,
        uses_agent_cli: true,
        override_var: "CABIN_CODEX_ACP_BIN",
    },
    Package {
        agent: AgentKind::Opencode,
        package: "@opencode/cli",
        bin: "opencode2",
        license: "MIT",
        default_version: "2.3.2",
        // The CLI's own platform binary is an optional dependency.
        omit_optional: false,
        uses_agent_cli: false,
        override_var: "CABIN_OPENCODE2_BIN",
    },
    Package {
        agent: AgentKind::Pi,
        package: "pi-acp",
        bin: "pi-acp",
        license: "MIT",
        default_version: "0.0.34",
        omit_optional: true,
        uses_agent_cli: true,
        override_var: "CABIN_PI_ACP_BIN",
    },
];

fn managed_package(agent: AgentKind) -> Result<&'static Package> {
    PACKAGES.iter().find(|package| package.agent == agent).ok_or_else(|| {
        Error::invalid(format!(
            "{} uses the CLI installed in Agents → Setup for ACP",
            agents::adapter(agent).spec().name
        ))
    })
}

fn package(agent: AgentKind) -> &'static Package {
    managed_package(agent).expect("checked managed ACP runtime")
}

fn root(ctx: &Context) -> PathBuf {
    ctx.paths.data().join("acp")
}

fn agent_dir(ctx: &Context, agent: AgentKind) -> PathBuf {
    root(ctx).join(agent.as_str())
}

fn version_dir(ctx: &Context, agent: AgentKind, version: &str) -> PathBuf {
    agent_dir(ctx, agent).join(version)
}

fn bin_path(ctx: &Context, agent: AgentKind, version: &str) -> PathBuf {
    version_dir(ctx, agent, version)
        .join("node_modules/.bin")
        .join(package(agent).bin)
}

fn selection_file(ctx: &Context) -> PathBuf {
    root(ctx).join("selected.json")
}

/// A version as npm publishes it: digits, dots and pre-release tags only, so
/// it is safe as a directory name and an npm spec.
pub fn valid_version(version: &str) -> Result<()> {
    let ok = !version.is_empty()
        && version.len() <= 64
        && version
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'));
    if ok {
        Ok(())
    } else {
        Err(Error::invalid(format!(
            "{version:?} is not a package version"
        )))
    }
}

fn selected(ctx: &Context, agent: AgentKind) -> String {
    std::fs::read_to_string(selection_file(ctx))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value[agent.as_str()].as_str().map(str::to_string))
        .filter(|version| valid_version(version).is_ok())
        .unwrap_or_else(|| package(agent).default_version.to_string())
}

fn write_selected(ctx: &Context, agent: AgentKind, version: &str) -> Result<()> {
    let path = selection_file(ctx);
    let mut value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    value[agent.as_str()] = Value::String(version.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| Error::io("create the runtime folder", error))?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&value)?)
        .map_err(|error| Error::io("save the runtime choice", error))?;
    std::fs::rename(&tmp, &path).map_err(|error| Error::io("save the runtime choice", error))
}

fn installed(ctx: &Context, agent: AgentKind) -> Vec<String> {
    let mut versions: Vec<String> = std::fs::read_dir(agent_dir(ctx, agent))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|version| valid_version(version).is_ok() && bin_path(ctx, agent, version).is_file())
        .collect();
    versions.sort_by(|a, b| compare_versions(b, a));
    versions
}

fn override_path(agent: AgentKind) -> Option<PathBuf> {
    std::env::var_os(package(agent).override_var)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

pub fn state(ctx: &Context) -> Vec<AcpRuntime> {
    PACKAGES
        .iter()
        .map(|package| AcpRuntime {
            agent: package.agent,
            package: package.package.into(),
            license: package.license.into(),
            default_version: package.default_version.into(),
            selected: selected(ctx, package.agent),
            installed: installed(ctx, package.agent),
            override_path: override_path(package.agent).map(|path| path.display().to_string()),
            uses_agent_cli: package.uses_agent_cli,
        })
        .collect()
}

/// Published versions, newest first. Pre-releases are included; the UI
/// marks them.
pub fn versions(ctx: &Context, agent: AgentKind) -> Result<Vec<String>> {
    let package = managed_package(agent)?;
    let spec = CommandSpec::new(["npm", "view", package.package, "versions", "--json"])
        .env(crate::environment::child_env(ctx))
        .timeout(VIEW_TIMEOUT);
    let out = ctx.system.check(&spec)?;
    let value: Value = serde_json::from_str(out.trim()).map_err(|error| {
        Error::Unavailable(format!("npm returned an unreadable version list: {error}"))
    })?;
    let mut versions: Vec<String> = match value {
        Value::Array(items) => items
            .into_iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        Value::String(one) => vec![one],
        _ => vec![],
    };
    versions.retain(|version| valid_version(version).is_ok());
    versions.sort_by(|a, b| compare_versions(b, a));
    Ok(versions)
}

/// Semver order, with a pre-release below its release.
pub fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(version: &str) -> (Vec<u64>, Option<&str>) {
        let (core, pre) = match version.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (version.split('+').next().unwrap_or(version), None),
        };
        (
            core.split('.')
                .map(|part| part.parse().unwrap_or(0))
                .collect(),
            pre,
        )
    }
    let (core_a, pre_a) = parts(a);
    let (core_b, pre_b) = parts(b);
    core_a.cmp(&core_b).then_with(|| match (pre_a, pre_b) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(a), Some(b)) => {
            let split = |pre: &str| pre.split('.').map(str::to_string).collect::<Vec<_>>();
            let (a, b) = (split(a), split(b));
            for (x, y) in a.iter().zip(&b) {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(x), Ok(y)) => x.cmp(&y),
                    _ => x.cmp(y),
                };
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
            a.len().cmp(&b.len())
        }
    })
}

fn npm_install(ctx: &Context, agent: AgentKind, version: &str) -> CommandSpec {
    let package = package(agent);
    let mut argv = vec![
        "npm".to_string(),
        "install".into(),
        "--prefix".into(),
        version_dir(ctx, agent, version).display().to_string(),
        "--no-audit".into(),
        "--no-fund".into(),
        "--no-save".into(),
    ];
    if package.omit_optional {
        argv.push("--omit=optional".into());
    }
    argv.push(format!("{}@{version}", package.package));
    CommandSpec::new(argv)
        .env(crate::environment::child_env(ctx))
        .timeout(INSTALL_TIMEOUT)
}

struct InstallStep {
    ctx: Context,
    agent: AgentKind,
    version: String,
    select: bool,
    existed: bool,
    previous: Option<String>,
}

impl Step for InstallStep {
    fn name(&self) -> String {
        format!("Install {}@{}", package(self.agent).package, self.version)
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        valid_version(&self.version)?;
        self.existed = bin_path(&self.ctx, self.agent, &self.version).is_file();
        self.previous = Some(selected(&self.ctx, self.agent));
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if !self.existed {
            std::fs::create_dir_all(version_dir(&self.ctx, self.agent, &self.version))
                .map_err(|error| Error::io("create the runtime folder", error))?;
            let command = npm_install(&self.ctx, self.agent, &self.version);
            ctx.log(format!("Running {}", command.display()));
            let out = self.ctx.system.check(&command)?;
            for line in out
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                ctx.log(line.to_string());
            }
        } else {
            ctx.log("Already installed");
        }
        if self.select {
            write_selected(&self.ctx, self.agent, &self.version)?;
            ctx.log(format!("New chats use {}", self.version));
        }
        Ok(())
    }

    fn verify(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let bin = bin_path(&self.ctx, self.agent, &self.version);
        if bin.is_file() {
            ctx.log(format!("{} is ready", bin.display()));
            Ok(())
        } else {
            Err(Error::Internal(format!(
                "{} was not found after installing",
                package(self.agent).bin
            )))
        }
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.select
            && let Some(previous) = &self.previous
        {
            write_selected(&self.ctx, self.agent, previous)?;
        }
        if !self.existed {
            let _ = std::fs::remove_dir_all(version_dir(&self.ctx, self.agent, &self.version));
            ctx.log("Removed the partial install");
        }
        Ok(())
    }
}

pub fn install_operation(
    ctx: &Context,
    agent: AgentKind,
    version: &str,
    select: bool,
) -> Result<Operation> {
    valid_version(version)?;
    let package = managed_package(agent)?;
    Ok(Operation::new(
        "acp.install",
        format!("Install {} {version}", package.package),
    )
    .key(format!("acp-install-{}", agent.as_str()))
    .request(serde_json::json!({"agent": agent, "version": version, "select": select}))
    .step(InstallStep {
        ctx: ctx.clone(),
        agent,
        version: version.to_string(),
        select,
        existed: false,
        previous: None,
    }))
}

pub fn select(ctx: &Context, agent: AgentKind, version: &str) -> Result<()> {
    managed_package(agent)?;
    valid_version(version)?;
    if !bin_path(ctx, agent, version).is_file() {
        return Err(Error::Conflict(format!(
            "Install {version} before using it"
        )));
    }
    write_selected(ctx, agent, version)
}

pub fn remove(ctx: &Context, agent: AgentKind, version: &str) -> Result<()> {
    managed_package(agent)?;
    valid_version(version)?;
    if selected(ctx, agent) == version {
        return Err(Error::Conflict(
            "New chats use this version. Choose another one first.".into(),
        ));
    }
    let dir = version_dir(ctx, agent, version);
    if !dir.starts_with(agent_dir(ctx, agent)) || !dir.is_dir() {
        return Err(Error::NotFound("runtime version".into()));
    }
    std::fs::remove_dir_all(&dir).map_err(|error| Error::io("remove the runtime version", error))
}

fn required_cli(ctx: &Context, agent: AgentKind) -> Result<PathBuf> {
    agents::find_binary(ctx, agent).ok_or_else(|| {
        Error::Unavailable(format!(
            "{} is not installed. Install it from Agents → Setup.",
            agents::adapter(agent).spec().name
        ))
    })
}

/// A new chat runs the native CLI, an override, or the selected package.
pub fn binary(ctx: &Context, agent: AgentKind) -> Result<super::RuntimeBinary> {
    if agent == AgentKind::Antigravity {
        return Ok(super::RuntimeBinary {
            path: required_cli(ctx, agent)?,
            install: None,
        });
    }
    if let Some(path) = override_path(agent) {
        return Ok(super::RuntimeBinary {
            path,
            install: None,
        });
    }
    let version = selected(ctx, agent);
    let path = bin_path(ctx, agent, &version);
    let install = (!path.is_file()).then(|| npm_install(ctx, agent, &version).argv);
    Ok(super::RuntimeBinary { path, install })
}

/// Environment the adapter needs to run the agent CLI from Setup.
pub fn agent_env(ctx: &Context, agent: AgentKind) -> Result<Vec<(String, String)>> {
    let cli = || required_cli(ctx, agent).map(|path| path.display().to_string());
    Ok(match agent {
        AgentKind::Claude => vec![("CLAUDE_CODE_EXECUTABLE".into(), cli()?)],
        AgentKind::Codex => vec![
            ("CODEX_PATH".into(), cli()?),
            // Sign-in happens in Settings; a browser cannot open inside the VM.
            ("NO_BROWSER".into(), "1".into()),
        ],
        AgentKind::Pi => vec![
            ("PI_ACP_PI_COMMAND".into(), cli()?),
            // Attached files keep their text.
            ("PI_ACP_ENABLE_EMBEDDED_CONTEXT".into(), "true".into()),
        ],
        AgentKind::Opencode => vec![],
        AgentKind::Antigravity => agents::adapter(agent).launch_env(),
    })
}

/// The agent CLI used for model discovery: OpenCode's runtime is the CLI.
pub fn cli_path(ctx: &Context, agent: AgentKind) -> Option<PathBuf> {
    match agent {
        AgentKind::Opencode => binary(ctx, agent).ok().map(|runtime| runtime.path),
        other => agents::find_binary(ctx, other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_sort_like_semver() {
        let mut versions = vec![
            "2.6.1-preview.4",
            "2.7.0",
            "2.6.1",
            "2.10.0",
            "2.7.1-preview.1",
            "2.6.1-preview.10",
        ];
        versions.sort_by(|a, b| compare_versions(b, a));
        assert_eq!(
            versions,
            [
                "2.10.0",
                "2.7.1-preview.1",
                "2.7.0",
                "2.6.1",
                "2.6.1-preview.10",
                "2.6.1-preview.4"
            ]
        );
    }

    #[test]
    fn only_plain_versions_are_accepted() {
        assert!(valid_version("0.108.1").is_ok());
        assert!(valid_version("2.7.1-preview.3").is_ok());
        for bad in ["", "latest", "../x", "1.0 ; rm", "1/2", "^1.0.0"] {
            assert!(valid_version(bad).is_err(), "{bad}");
        }
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::FakeSystem;
    use std::path::Path;
    use std::sync::Arc;

    fn context(dir: &Path) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system: Arc::new(FakeSystem::default()),
        }
    }

    fn fake_install(ctx: &Context, agent: AgentKind, version: &str) {
        let bin = bin_path(ctx, agent, version);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(bin, "#!/bin/sh\n").unwrap();
    }

    #[test]
    fn selection_needs_an_install_and_the_selected_version_cannot_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        assert_eq!(selected(&ctx, AgentKind::Codex), "2.7.0");
        assert!(
            select(&ctx, AgentKind::Codex, "2.6.1").is_err(),
            "not installed"
        );
        fake_install(&ctx, AgentKind::Codex, "2.6.1");
        fake_install(&ctx, AgentKind::Codex, "2.7.0");
        select(&ctx, AgentKind::Codex, "2.6.1").unwrap();
        assert_eq!(selected(&ctx, AgentKind::Codex), "2.6.1");
        assert_eq!(
            selected(&ctx, AgentKind::Claude),
            "0.108.1",
            "other agents keep their default"
        );
        assert_eq!(installed(&ctx, AgentKind::Codex), ["2.7.0", "2.6.1"]);
        assert!(remove(&ctx, AgentKind::Codex, "2.6.1").is_err(), "in use");
        remove(&ctx, AgentKind::Codex, "2.7.0").unwrap();
        assert_eq!(installed(&ctx, AgentKind::Codex), ["2.6.1"]);
        assert!(remove(&ctx, AgentKind::Codex, "../..").is_err());
    }

    #[test]
    fn a_missing_runtime_installs_without_the_bundled_cli() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let claude = binary(&ctx, AgentKind::Claude).unwrap();
        let argv = claude.install.expect("installs on first use");
        assert!(argv.contains(&"--omit=optional".to_string()));
        assert_eq!(
            argv.last().unwrap(),
            "@agentclientprotocol/claude-agent-acp@0.108.1"
        );
        let opencode = binary(&ctx, AgentKind::Opencode).unwrap().install.unwrap();
        assert!(
            !opencode.contains(&"--omit=optional".to_string()),
            "OpenCode needs its platform binary"
        );
        fake_install(&ctx, AgentKind::Claude, "0.108.1");
        assert!(binary(&ctx, AgentKind::Claude).unwrap().install.is_none());
    }

    #[test]
    fn antigravity_uses_its_cli_without_a_managed_acp_package() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let agent = AgentKind::Antigravity;
        let cli = ctx.paths.home().join(".local/bin/agy");
        std::fs::create_dir_all(cli.parent().unwrap())?;
        std::fs::write(&cli, "#!/bin/sh\nexit 81\n")?;
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755))?;
        let runtime = binary(&ctx, agent)?;
        assert_eq!(runtime.path, cli);
        assert!(runtime.install.is_none());
        assert!(state(&ctx).iter().all(|runtime| runtime.agent != agent));
        assert!(versions(&ctx, agent).is_err());
        assert!(install_operation(&ctx, agent, "1.5.14", true).is_err());
        assert!(select(&ctx, agent, "1.5.14").is_err());
        assert!(remove(&ctx, agent, "1.5.14").is_err());
        Ok(())
    }
}
