//! Setup: host checks and journaled installs.
//!
//! Ubuntu is already installed in the VM, so "installing" means getting it
//! ready for agents: base packages through the configured privilege mechanism,
//! Node.js from the official builds (see `node`), and agent CLIs into `~/.local`
//! through npm or a native release.

use std::time::Duration;

use crate::agents::{self, find_binary};
use crate::context::Context;
use crate::environment::child_env;
use crate::error::{Error, Result};
use crate::model::{AgentKind, CheckStatus, NodeSource, SetupAction, SetupCheck};
use crate::node;
use crate::ops::{Operation, Step, StepCtx};
use crate::system::{CommandSpec, privileged};

/// Node.js is not among them: Ubuntu's package is too old (see `node`).
/// `locales` brings `locale-gen` and the locale sources the minimal cloud
/// image leaves out; new guests get it at provisioning, older ones here.
pub const BASE_PACKAGES: [&str; 3] = ["git", "curl", "locales"];
const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

fn command_version(ctx: &Context, program: &str) -> Option<String> {
    let out = ctx
        .system
        .run(&CommandSpec::new([program, "--version"]).timeout(Duration::from_secs(10)))
        .ok()?;
    out.success()
        .then(|| out.stdout.lines().next().unwrap_or("").trim().to_string())
}

fn os_release() -> Option<(String, String)> {
    let text = std::fs::read_to_string("/etc/os-release").ok()?;
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{name}=")))
            .map(|value| value.trim_matches('"').to_string())
    };
    Some((field("ID")?, field("PRETTY_NAME").unwrap_or_default()))
}

fn check(id: &str, label: &str, status: CheckStatus, detail: impl Into<String>) -> SetupCheck {
    SetupCheck {
        id: id.into(),
        label: label.into(),
        status,
        detail: detail.into(),
        action: None,
    }
}

pub fn checks(ctx: &Context) -> Vec<SetupCheck> {
    let mut list = Vec::new();

    list.push(match os_release() {
        Some((id, pretty)) if id == "ubuntu" => {
            check("os", "Operating system", CheckStatus::Ok, pretty)
        }
        Some((_, pretty)) => check(
            "os",
            "Operating system",
            CheckStatus::Warning,
            format!("{pretty}. Cabin targets Ubuntu; most features still work."),
        ),
        None => check(
            "os",
            "Operating system",
            CheckStatus::Warning,
            "Could not read /etc/os-release",
        ),
    });

    let arch = std::env::consts::ARCH;
    list.push(check(
        "arch",
        "Architecture",
        if arch == "aarch64" {
            CheckStatus::Ok
        } else {
            CheckStatus::Warning
        },
        if arch == "aarch64" {
            "ARM64".to_string()
        } else {
            format!("{arch}; built for ARM64 Ubuntu on Apple Silicon, but should work")
        },
    ));

    let git = command_version(ctx, "git");
    let mut base = match node::active(ctx) {
        Some(runtime) => {
            let major = node::major(&runtime.version).unwrap_or(0);
            let npm = runtime
                .npm
                .as_deref()
                .map(|v| format!(", npm {v}"))
                .unwrap_or_default();
            let origin = match runtime.source {
                NodeSource::Managed => "managed by cabin".to_string(),
                NodeSource::System => "Ubuntu package".to_string(),
                NodeSource::Other => runtime.path.clone(),
            };
            if major < node::MINIMUM_MAJOR {
                check(
                    "node",
                    "Node.js",
                    CheckStatus::Warning,
                    format!(
                        "node {} ({origin}) is end-of-life. Agent CLIs that use npm need Node.js {} or newer. Install Node.js {} LTS.",
                        runtime.version,
                        node::MINIMUM_MAJOR,
                        node::RECOMMENDED_MAJOR
                    ),
                )
            } else if runtime.npm.is_none() {
                check(
                    "node",
                    "Node.js",
                    CheckStatus::Warning,
                    format!(
                        "node {} ({origin}) has no npm; agent installs need it",
                        runtime.version
                    ),
                )
            } else {
                check(
                    "node",
                    "Node.js",
                    CheckStatus::Ok,
                    format!("node {} ({origin}){npm}", runtime.version),
                )
            }
        }
        None => check(
            "node",
            "Node.js",
            CheckStatus::Missing,
            format!(
                "Needed to install agent CLIs that use npm. Install Node.js {} LTS.",
                node::RECOMMENDED_MAJOR
            ),
        ),
    };
    if base.status != CheckStatus::Ok {
        base.action = Some(SetupAction::InstallNode);
    }
    list.push(base);

    let mut git_check = match git {
        Some(version) => check("git", "Git", CheckStatus::Ok, version),
        None => check(
            "git",
            "Git",
            CheckStatus::Missing,
            "Agents use git to track changes",
        ),
    };
    if git_check.status != CheckStatus::Ok {
        git_check.action = Some(SetupAction::InstallBasePackages);
    }
    list.push(git_check);
    list.push(if crate::environment::LocaleFiles::system().can_generate() {
        check(
            "locales",
            "Language packs",
            CheckStatus::Ok,
            "Any supported language can be applied",
        )
    } else {
        let mut locales = check(
            "locales",
            "Language packs",
            CheckStatus::Missing,
            "Only the built-in C language is available. Language packs come with the base tools once the machine is online.",
        );
        locales.action = Some(SetupAction::InstallBasePackages);
        locales
    });
    list.push(crate::python::check(ctx));

    let has = |program: &str| {
        ctx.paths
            .child_path()
            .split(':')
            .any(|dir| std::path::Path::new(dir).join(program).is_file())
    };
    let privilege = privileged(["true"]);
    let mut required = vec!["timedatectl", "localectl"];
    if let Some(program) = privilege.argv.first().map(String::as_str)
        && matches!(program, "sudo" | "pkexec")
    {
        required.insert(0, program);
    }
    let missing: Vec<&str> = required.iter().copied().filter(|p| !has(p)).collect();
    list.push(if missing.is_empty() {
        check(
            "polkit",
            "System settings access",
            CheckStatus::Ok,
            format!("{} are available", required.join(", ")),
        )
    } else {
        check(
            "polkit",
            "System settings access",
            CheckStatus::Warning,
            format!(
                "Missing {}; timezone, language and package installs will not work",
                missing.join(", ")
            ),
        )
    });

    let disks = sysinfo::Disks::new_with_refreshed_list();
    let home = ctx.paths.home();
    let free = disks
        .list()
        .iter()
        .filter(|disk| home.starts_with(disk.mount_point()))
        .max_by_key(|disk| disk.mount_point().as_os_str().len())
        .map(|disk| disk.available_space());
    list.push(match free {
        Some(bytes) if bytes >= 2 * 1024 * 1024 * 1024 => check(
            "disk",
            "Free disk space",
            CheckStatus::Ok,
            format!("{:.1} GB free in your home folder", bytes as f64 / 1e9),
        ),
        Some(bytes) => check(
            "disk",
            "Free disk space",
            CheckStatus::Warning,
            format!(
                "Only {:.1} GB free; agents and packages need at least 2 GB",
                bytes as f64 / 1e9
            ),
        ),
        None => check(
            "disk",
            "Free disk space",
            CheckStatus::Warning,
            "Could not read disk usage",
        ),
    });

    let local_bin = home.join(".local/bin").display().to_string();
    let on_path = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|dir| dir == local_bin);
    list.push(if on_path {
        check("path", "~/.local/bin on PATH", CheckStatus::Ok, "Agents installed here work in any terminal")
    } else {
        check(
            "path",
            "~/.local/bin on PATH",
            CheckStatus::Warning,
            "Sessions in this app find agents anyway; add ~/.local/bin to PATH to use them in other terminals",
        )
    });

    for kind in AgentKind::ALL {
        let adapter = agents::adapter(kind);
        let spec = adapter.spec();
        let status = agents::status(ctx, kind);
        let mut item = match (status.installed, status.version) {
            (true, Some(version)) => check(
                &format!("agent-{}", kind.as_str()),
                spec.name,
                CheckStatus::Ok,
                version,
            ),
            (true, None) => check(
                &format!("agent-{}", kind.as_str()),
                spec.name,
                CheckStatus::Warning,
                "Installed, but `--version` failed",
            ),
            (false, _) => check(
                &format!("agent-{}", kind.as_str()),
                spec.name,
                CheckStatus::Missing,
                if adapter.requires_node() {
                    format!("npm package {}", spec.package)
                } else {
                    format!("Native CLI {}", spec.binary)
                },
            ),
        };
        if item.status != CheckStatus::Ok {
            item.action = Some(SetupAction::InstallAgent { agent: kind });
        }
        list.push(item);
    }
    list
}

// ------------------------------------------------------------------ steps

struct AptStep {
    ctx: Context,
    args: Vec<String>,
    label: String,
}

impl Step for AptStep {
    fn name(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let mut argv = vec![
            "env".to_string(),
            "DEBIAN_FRONTEND=noninteractive".into(),
            "apt-get".into(),
        ];
        argv.extend(self.args.iter().cloned());
        let command = privileged(argv).timeout(INSTALL_TIMEOUT);
        ctx.log(format!("Running {}", command.display()));
        let out = self.ctx.system.check(&command)?;
        for line in out
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            ctx.log(line.to_string());
        }
        Ok(())
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        ctx.log("System packages are left installed (removing them could break other software)");
        Ok(())
    }
}

pub fn install_base_operation(ctx: &Context) -> Operation {
    let mut install = vec!["install".to_string(), "-y".into()];
    install.extend(BASE_PACKAGES.iter().map(|p| p.to_string()));
    Operation::new("setup.installBase", "Install base tools")
        .key("setup-apt")
        .request(serde_json::json!({}))
        .step(AptStep {
            ctx: ctx.clone(),
            args: vec!["update".into()],
            label: "Refresh package lists".into(),
        })
        .step(AptStep {
            ctx: ctx.clone(),
            args: install,
            label: format!("Install {}", BASE_PACKAGES.join(", ")),
        })
}

struct NpmInstallStep {
    ctx: Context,
    kind: AgentKind,
    was_installed: bool,
}

impl NpmInstallStep {
    fn npm(&self, action: &str) -> CommandSpec {
        let spec = agents::adapter(self.kind).spec();
        let prefix = self.ctx.paths.home().join(".local").display().to_string();
        CommandSpec::new([
            "npm",
            action,
            "-g",
            "--prefix",
            prefix.as_str(),
            spec.package,
        ])
        .env(child_env(&self.ctx))
        .timeout(INSTALL_TIMEOUT)
    }
}

impl Step for NpmInstallStep {
    fn name(&self) -> String {
        format!("Install {}", agents::adapter(self.kind).spec().package)
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        if command_version(&self.ctx, "npm").is_none() {
            return Err(Error::Unavailable(
                "npm is not installed. Install Node.js first (Setup → Node.js).".into(),
            ));
        }
        self.was_installed = find_binary(&self.ctx, self.kind).is_some();
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let command = self.npm("install");
        ctx.log(format!("Running {}", command.display()));
        let out = self.ctx.system.check(&command)?;
        for line in out
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            ctx.log(line.to_string());
        }
        Ok(())
    }

    fn verify(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let status = agents::status(&self.ctx, self.kind);
        match (status.binary, status.version) {
            (Some(binary), Some(version)) => {
                ctx.log(format!("{binary} → {version}"));
                Ok(())
            }
            (Some(binary), None) => Err(Error::Internal(format!(
                "{binary} was installed but does not run"
            ))),
            (None, _) => Err(Error::Internal(format!(
                "`{}` was not found after installing",
                agents::adapter(self.kind).spec().binary
            ))),
        }
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.was_installed {
            ctx.log("It was installed before; leaving it in place");
            return Ok(());
        }
        self.ctx.system.check(&self.npm("uninstall"))?;
        ctx.log("Removed the partial install");
        Ok(())
    }
}

pub fn install_agent_operation(ctx: &Context, kind: AgentKind) -> Operation {
    if kind == AgentKind::Antigravity {
        return agents::antigravity::install_operation(ctx);
    }
    let spec = agents::adapter(kind).spec();
    Operation::new("setup.installAgent", format!("Install {}", spec.name))
        .key(format!("setup-agent-{}", kind.as_str()))
        .request(serde_json::json!({ "agent": kind }))
        .step(NpmInstallStep {
            ctx: ctx.clone(),
            kind,
            was_installed: false,
        })
}

pub fn action_operation(ctx: &Context, action: &SetupAction) -> Result<Operation> {
    Ok(match action {
        SetupAction::InstallBasePackages => install_base_operation(ctx),
        SetupAction::InstallNode => node::install_latest_lts_operation(ctx)?,
        SetupAction::InstallPython => crate::python::install_operation(ctx),
        SetupAction::InstallAgent { agent } => install_agent_operation(ctx, *agent),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OperationState;
    use crate::ops::OperationManager;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::{FakeSystem, fail, ok};
    use std::sync::Arc;

    fn context(dir: &std::path::Path, system: Arc<FakeSystem>) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system,
        }
    }

    #[test]
    fn base_packages_use_the_configured_privilege_command() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let ctx = context(dir.path(), fake.clone());
        let record = OperationManager::new(ctx.store.clone()).run(install_base_operation(&ctx))?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        let prefix = privileged(["env", "DEBIAN_FRONTEND=noninteractive", "apt-get"]).display();
        assert_eq!(
            fake.calls(),
            vec![
                format!("{prefix} update"),
                format!("{prefix} install -y git curl locales"),
            ]
        );
        Ok(())
    }

    #[test]
    fn failed_verify_uninstalls_a_fresh_install() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        fake.on(|spec| match spec.argv.first().map(String::as_str) {
            Some("npm") => Some(ok("10.0.0")),
            _ => None,
        });
        let ctx = context(dir.path(), fake.clone());
        let ops = OperationManager::new(ctx.store.clone());
        // npm "succeeds" but never creates the binary, so verify fails.
        let record = ops.run(install_agent_operation(&ctx, AgentKind::Pi))?;
        assert_eq!(record.state, OperationState::RolledBack, "{record:?}");
        let calls = fake.calls();
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("npm install -g --prefix"))
        );
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("npm uninstall -g --prefix"))
        );
        Ok(())
    }

    #[test]
    fn missing_npm_fails_before_changing_anything() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        fake.on(|spec| (spec.argv[0] == "npm").then(|| fail("not found")));
        let ctx = context(dir.path(), fake.clone());
        let record = OperationManager::new(ctx.store.clone())
            .run(install_agent_operation(&ctx, AgentKind::Claude))?;
        assert_eq!(record.state, OperationState::Failed);
        assert!(!fake.calls().iter().any(|c| c.starts_with("npm install")));
        Ok(())
    }
}
