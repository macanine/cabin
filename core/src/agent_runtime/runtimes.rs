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

use std::fs::{self, File, OpenOptions, TryLockError};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::agents;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::model::{AcpRuntime, AgentKind};
use crate::ops::{Operation, Step, StepCtx};
use crate::system::{CommandSpec, HostSystem, System};
use crate::util::LockExt;

const INSTALL_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const VIEW_TIMEOUT: Duration = Duration::from_secs(60);
static SELECTION_WRITE: Mutex<()> = Mutex::new(());

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
    PACKAGES
        .iter()
        .find(|package| package.agent == agent)
        .ok_or_else(|| {
            Error::invalid(format!(
                "{} uses the CLI installed in Agents → Setup for ACP",
                agents::adapter(agent).spec().name
            ))
        })
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

fn lock_version(dir: &Path, wait: bool, cancelled: &dyn Fn() -> bool) -> Result<File> {
    let version = dir
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::invalid("The runtime install has no version directory"))?;
    valid_version(version)?;
    let path = dir.with_file_name(format!(".{version}.install.lock"));
    if let Some(parent) = dir.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        crate::fsx::ensure_dir(parent)?;
    }
    // Keep the lock outside the install directory so rollback cannot unlink it.
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| Error::path("open runtime version lock", &path, error))?;
    if !file.metadata()?.is_file() {
        return Err(Error::Conflict(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let started = Instant::now();
    loop {
        if cancelled() {
            return Err(Error::Cancelled);
        }
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if wait && started.elapsed() < INSTALL_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(Error::Conflict(format!(
                    "Runtime {version} is being installed or changed"
                )));
            }
            Err(TryLockError::Error(error)) => {
                return Err(Error::path("lock runtime version", &path, error));
            }
        }
    }
}

fn bin_path(ctx: &Context, package: &Package, version: &str) -> PathBuf {
    version_dir(ctx, package.agent, version)
        .join("node_modules/.bin")
        .join(package.bin)
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

fn selected(ctx: &Context, package: &Package) -> String {
    version_for(
        &read_selections(&selection_file(ctx)).unwrap_or_default(),
        package,
    )
    .into()
}

fn version_for<'a>(selections: &'a serde_json::Map<String, Value>, package: &Package) -> &'a str {
    selections
        .get(package.agent.as_str())
        .and_then(Value::as_str)
        .filter(|version| valid_version(version).is_ok())
        .unwrap_or(package.default_version)
}

fn read_selections(path: &std::path::Path) -> Result<serde_json::Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(serde_json::Map::new()),
        Err(error) => Err(Error::path("read runtime choices", path, error)),
    }
}

fn installed(ctx: &Context, package: &Package) -> Vec<String> {
    let mut versions: Vec<String> = std::fs::read_dir(agent_dir(ctx, package.agent))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|version| {
            valid_version(version).is_ok() && bin_path(ctx, package, version).is_file()
        })
        .collect();
    versions.sort_by(|a, b| compare_versions(b, a));
    versions
}

fn override_path(package: &Package) -> Option<PathBuf> {
    std::env::var_os(package.override_var)
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
            selected: selected(ctx, package),
            installed: installed(ctx, package),
            override_path: override_path(package).map(|path| path.display().to_string()),
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

fn npm_install(ctx: &Context, package: &Package, version: &str) -> CommandSpec {
    let mut argv = vec![
        "npm".to_string(),
        "install".into(),
        "--prefix".into(),
        version_dir(ctx, package.agent, version)
            .display()
            .to_string(),
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
    package: &'static Package,
    version: String,
    select: bool,
    _lock: Option<File>,
    changed: bool,
}

impl Step for InstallStep {
    fn name(&self) -> String {
        format!("Install {}@{}", self.package.package, self.version)
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let dir = version_dir(&self.ctx, self.package.agent, &self.version);
        self._lock = Some(lock_version(&dir, true, &|| ctx.is_cancelled())?);
        if !bin_path(&self.ctx, self.package, &self.version).is_file() {
            crate::fsx::ensure_dir(&dir)?;
            self.changed = true;
            let command = npm_install(&self.ctx, self.package, &self.version);
            ctx.log(format!("Running {}", command.display()));
            let out = self
                .ctx
                .system
                .check_cancellable(&command, &|| ctx.is_cancelled())?;
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
        Ok(())
    }

    fn verify(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if ctx.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let bin = bin_path(&self.ctx, self.package, &self.version);
        if bin.is_file() {
            ctx.log(format!("{} is ready", bin.display()));
            if self.select {
                select_installed(&self.ctx, self.package, &self.version)?;
                ctx.log(format!("New chats use {}", self.version));
            }
            Ok(())
        } else {
            Err(Error::Internal(format!(
                "{} was not found after installing",
                self.package.bin
            )))
        }
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.changed {
            let dir = version_dir(&self.ctx, self.package.agent, &self.version);
            remove_partial(&dir)?;
            ctx.log("Removed the partial install");
        }
        Ok(())
    }
}

fn remove_partial(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::path("remove the partial install", dir, error)),
    }
}

pub(super) fn ensure_installed_until(
    binary: &Path,
    argv: &[String],
    env: &[(String, String)],
    cancelled: &dyn Fn() -> bool,
) -> Result<()> {
    let dir = argv
        .windows(2)
        .find(|args| args[0] == "--prefix")
        .map(|args| Path::new(&args[1]))
        .filter(|dir| binary.starts_with(dir))
        .ok_or_else(|| Error::Internal("The runtime installer has no version directory".into()))?;
    let _lock = lock_version(dir, true, cancelled)?;
    if binary.is_file() {
        return Ok(());
    }
    crate::fsx::ensure_dir(dir)?;
    let system = HostSystem {
        path: std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into()),
    };
    let command = CommandSpec::new(argv.iter().cloned())
        .env(env.iter().cloned())
        .timeout(INSTALL_TIMEOUT);
    let result = system.check_cancellable(&command, cancelled).and_then(|_| {
        if binary.is_file() {
            Ok(())
        } else {
            Err(Error::Unavailable(
                "The agent runtime binary is missing after installation".into(),
            ))
        }
    });
    if let Err(error) = result {
        remove_partial(dir).map_err(|cleanup| {
            Error::Internal(format!("{error}. Runtime cleanup also failed: {cleanup}"))
        })?;
        return Err(match error {
            Error::Command { message, output } => {
                let mut start = output.len().saturating_sub(600);
                while !output.is_char_boundary(start) {
                    start += 1;
                }
                let tail = output[start..].trim();
                Error::Unavailable(format!(
                    "Could not install the agent runtime: {}",
                    if tail.is_empty() { &message } else { tail }
                ))
            }
            error => error,
        });
    }
    Ok(())
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
        package,
        version: version.to_string(),
        select,
        _lock: None,
        changed: false,
    }))
}

pub fn select(ctx: &Context, agent: AgentKind, version: &str) -> Result<()> {
    let package = managed_package(agent)?;
    valid_version(version)?;
    let _lock = lock_version(&version_dir(ctx, agent, version), false, &|| false)?;
    select_installed(ctx, package, version)
}

fn select_installed(ctx: &Context, package: &Package, version: &str) -> Result<()> {
    let _guard = SELECTION_WRITE.locked();
    if !bin_path(ctx, package, version).is_file() {
        return Err(Error::Conflict(format!(
            "Install {version} before using it"
        )));
    }
    let path = selection_file(ctx);
    let mut selections = read_selections(&path)?;
    selections.insert(
        package.agent.as_str().into(),
        Value::String(version.to_string()),
    );
    crate::fsx::atomic_write_json(&path, &Value::Object(selections), None)
}

pub fn remove(ctx: &Context, agent: AgentKind, version: &str) -> Result<()> {
    let package = managed_package(agent)?;
    valid_version(version)?;
    let dir = version_dir(ctx, agent, version);
    let _lock = lock_version(&dir, false, &|| false)?;
    let _guard = SELECTION_WRITE.locked();
    if version_for(&read_selections(&selection_file(ctx))?, package) == version {
        return Err(Error::Conflict(
            "New chats use this version. Choose another one first.".into(),
        ));
    }
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
    let package = managed_package(agent)?;
    if let Some(path) = override_path(package) {
        return Ok(super::RuntimeBinary {
            path,
            install: None,
        });
    }
    let selections = read_selections(&selection_file(ctx))?;
    let version = version_for(&selections, package);
    let path = bin_path(ctx, package, version);
    // The caller rechecks the executable under the version lock. An npm
    // process may already have created its shim while still installing.
    let install = Some(npm_install(ctx, package, version).argv);
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
    use crate::model::OperationState;
    use crate::ops::OperationManager;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::FakeSystem;
    use std::path::Path;
    use std::sync::{Arc, mpsc};

    fn context(dir: &Path) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system: Arc::new(FakeSystem::default()),
        }
    }

    fn fake_install(ctx: &Context, agent: AgentKind, version: &str) {
        let bin = bin_path(ctx, managed_package(agent).unwrap(), version);
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(bin, "#!/bin/sh\n").unwrap();
    }

    #[test]
    fn selection_needs_an_install_and_the_selected_version_cannot_be_removed() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = context(dir.path());
        let codex = managed_package(AgentKind::Codex).unwrap();
        assert_eq!(selected(&ctx, codex), "2.7.0");
        assert!(
            select(&ctx, AgentKind::Codex, "2.6.1").is_err(),
            "not installed"
        );
        fake_install(&ctx, AgentKind::Codex, "2.6.1");
        fake_install(&ctx, AgentKind::Codex, "2.7.0");
        select(&ctx, AgentKind::Codex, "2.6.1").unwrap();
        assert_eq!(selected(&ctx, codex), "2.6.1");
        assert_eq!(
            selected(&ctx, managed_package(AgentKind::Claude).unwrap()),
            "0.108.1",
            "other agents keep their default"
        );
        assert_eq!(installed(&ctx, codex), ["2.7.0", "2.6.1"]);
        assert!(remove(&ctx, AgentKind::Codex, "2.6.1").is_err(), "in use");
        remove(&ctx, AgentKind::Codex, "2.7.0").unwrap();
        assert_eq!(installed(&ctx, codex), ["2.6.1"]);
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
        let runtime = binary(&ctx, AgentKind::Claude).unwrap();
        let mut argv = runtime.install.unwrap();
        argv[0] = "/does-not-exist".into();
        ensure_installed_until(&runtime.path, &argv, &[], &|| false).unwrap();
    }

    #[test]
    fn automatic_install_waits_for_explicit_verification_and_reuses_the_runtime() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut ctx = context(dir.path());
        let agent = AgentKind::Codex;
        let version = managed_package(agent)?.default_version;
        let (shim_tx, shim_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let finish_rx = Mutex::new(finish_rx);
        let system = Arc::new(FakeSystem::default());
        let install_ctx = ctx.clone();
        system.on(move |_| {
            fake_install(&install_ctx, agent, version);
            let _ = shim_tx.send(());
            Some(
                match finish_rx.locked().recv_timeout(Duration::from_secs(5)) {
                    Ok(()) => crate::system::fake::ok(""),
                    Err(error) => Err(Error::Internal(error.to_string())),
                },
            )
        });
        ctx.system = system;
        let ops = OperationManager::new(ctx.store.clone());
        let operation = install_operation(&ctx, agent, version, true)?;
        let explicit = std::thread::spawn(move || ops.run(operation));
        shim_rx.recv_timeout(Duration::from_secs(3)).unwrap();

        // A shim exists even though npm has not completed yet.
        let runtime = binary(&ctx, agent)?;
        let mut argv = runtime.install.unwrap();
        argv[0] = "/does-not-exist".into();
        let (auto_tx, auto_rx) = mpsc::channel();
        let (wait_tx, wait_rx) = mpsc::channel();
        let automatic = std::thread::spawn(move || {
            let result = ensure_installed_until(&runtime.path, &argv, &[], &|| {
                let _ = wait_tx.send(());
                false
            });
            let _ = auto_tx.send(result);
        });
        wait_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let premature = auto_rx.recv_timeout(Duration::from_millis(150));
        let _ = finish_tx.send(());
        assert!(matches!(premature, Err(mpsc::RecvTimeoutError::Timeout)));
        assert_eq!(explicit.join().unwrap()?.state, OperationState::Succeeded);
        auto_rx.recv_timeout(Duration::from_secs(3)).unwrap()?;
        automatic.join().unwrap();
        Ok(())
    }

    #[test]
    fn a_cancelled_waiter_leaves_the_active_install_alone() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let agent = AgentKind::Codex;
        let version = managed_package(agent)?.default_version;
        let destination = version_dir(&ctx, agent, version);
        let active = lock_version(&destination, false, &|| false)?;
        let runtime = binary(&ctx, agent)?;
        let argv = runtime.install.unwrap();
        fake_install(&ctx, agent, version);
        let started = Instant::now();
        let result = ensure_installed_until(&runtime.path, &argv, &[], &|| {
            started.elapsed() >= Duration::from_millis(100)
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(runtime.path.is_file());
        drop(active);
        Ok(())
    }

    #[test]
    fn a_cancelled_automatic_install_reaps_the_command_and_removes_its_shim() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let agent = AgentKind::Codex;
        let package = managed_package(agent)?;
        let version = package.default_version;
        let destination = version_dir(&ctx, agent, version);
        let binary = bin_path(&ctx, package, version);
        let argv = [
            "/bin/sh", "-c",
            "mkdir -p \"$1/node_modules/.bin\"; touch \"$1/node_modules/.bin/codex-acp\"; exec sleep 30",
            "--prefix", &destination.to_string_lossy(),
        ].map(str::to_owned);
        let started = Instant::now();
        let saw_shim = std::cell::Cell::new(false);
        let result = ensure_installed_until(&binary, &argv, &[], &|| {
            saw_shim.set(binary.is_file());
            saw_shim.get() || started.elapsed() >= Duration::from_secs(3)
        });
        assert!(matches!(result, Err(Error::Cancelled)));
        assert!(saw_shim.get());
        assert!(!destination.exists());
        let _lock = lock_version(&destination, false, &|| false)?;
        Ok(())
    }

    #[test]
    fn an_unreadable_selection_is_preserved() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path());
        let path = selection_file(&ctx);
        fake_install(&ctx, AgentKind::Codex, "2.6.1");
        let partial = r#"{"claude":"0.108.1","#;
        std::fs::write(&path, partial)?;

        assert!(select(&ctx, AgentKind::Codex, "2.6.1").is_err());
        assert!(remove(&ctx, AgentKind::Codex, "2.6.1").is_err());
        assert_eq!(std::fs::read_to_string(path)?, partial);
        Ok(())
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
