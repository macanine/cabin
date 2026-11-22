//! Python environment inside the VM.
//!
//! The minimal Ubuntu image already has `python3` (cloud-init needs it) but
//! not what tools expect around it: `venv`/`ensurepip`, `pip`, headers and a
//! compiler for native wheels. Those come from apt. On top, Astral's `uv` and
//! `uvx` go into `~/.local/bin` (many MCP servers and agent tools are started
//! with `uvx`, and `uv python install` can add other Python versions without
//! touching the system one). The uv archive is checked against the
//! `.sha256` file published with each release.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::context::Context;
use crate::error::{Error, Result};
use crate::fetch::{download_to, get_text};
use crate::fsx::ensure_dir;
use crate::model::{CheckStatus, SetupAction, SetupCheck};
use crate::ops::{Operation, Step, StepCtx};
use crate::system::{CommandSpec, privileged};

/// apt packages that make the system Python usable for development.
pub const PACKAGES: [&str; 4] = [
    "python3-venv",
    "python3-pip",
    "python3-dev",
    "build-essential",
];
/// Latest uv release assets (GitHub redirects to the newest tag).
pub const UV_RELEASES: &str = "https://github.com/astral-sh/uv/releases/latest/download";
const UV_BINARIES: [&str; 2] = ["uv", "uvx"];
const APT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

// ------------------------------------------------------------------ status

fn first_line(ctx: &Context, argv: &[&str]) -> Option<String> {
    let out = ctx
        .system
        .run(&CommandSpec::new(argv.iter().copied()).timeout(Duration::from_secs(20)))
        .ok()?;
    out.success()
        .then(|| out.stdout.lines().next().unwrap_or("").trim().to_string())
        .filter(|line| !line.is_empty())
}

fn local_bin(ctx: &Context) -> PathBuf {
    ctx.paths.home().join(".local").join("bin")
}

/// `uv-aarch64-unknown-linux-gnu`: the release asset (and its top directory).
fn uv_target() -> String {
    format!("uv-{}-unknown-linux-gnu", std::env::consts::ARCH)
}

fn uv_version(ctx: &Context) -> Option<String> {
    let uv = local_bin(ctx).join("uv");
    let program = if uv.is_file() {
        uv.display().to_string()
    } else {
        "uv".into()
    };
    first_line(ctx, &[program.as_str(), "--version"])
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PythonStatus {
    /// `Python 3.12.3`
    pub python: Option<String>,
    /// `venv` with `ensurepip` (python3-venv) works.
    pub venv: bool,
    /// `pip 24.0`
    pub pip: Option<String>,
    /// `uv 0.13.8`
    pub uv: Option<String>,
}

pub fn status(ctx: &Context) -> PythonStatus {
    let python = first_line(ctx, &["python3", "--version"]);
    let has_python = python.is_some();
    PythonStatus {
        venv: has_python
            && first_line(
                ctx,
                &["python3", "-c", "import ensurepip, venv; print('ok')"],
            )
            .is_some(),
        pip: has_python
            .then(|| first_line(ctx, &["python3", "-m", "pip", "--version"]))
            .flatten()
            // "pip 24.0 from /usr/lib/… (python 3.12)" → "pip 24.0"
            .map(|line| line.split(" from ").next().unwrap_or(&line).to_string()),
        python,
        uv: uv_version(ctx),
    }
}

/// The Setup row for Python.
pub fn check(ctx: &Context) -> SetupCheck {
    let status = status(ctx);
    let mut missing = Vec::new();
    if !status.venv {
        missing.push("venv");
    }
    if status.pip.is_none() {
        missing.push("pip");
    }
    if status.uv.is_none() {
        missing.push("uv");
    }
    let found: Vec<&str> = [
        status.python.as_deref(),
        status.pip.as_deref(),
        status.uv.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let (check_status, detail) = match &status.python {
        None => (
            CheckStatus::Missing,
            "Needed for Python projects and for MCP servers started with uvx".to_string(),
        ),
        Some(_) if missing.is_empty() => (CheckStatus::Ok, found.join(" · ")),
        Some(_) => (
            CheckStatus::Warning,
            format!("{}; missing {}", found.join(" · "), missing.join(", ")),
        ),
    };
    SetupCheck {
        id: "python".into(),
        label: "Python".into(),
        action: (check_status != CheckStatus::Ok).then_some(SetupAction::InstallPython),
        status: check_status,
        detail,
    }
}

// ------------------------------------------------------------------- steps

/// Installs the apt packages that are not installed yet (nothing to do, no
/// `apt-get update`, when all are present).
struct PackagesStep {
    ctx: Context,
    missing: Vec<String>,
}

impl PackagesStep {
    fn apt(&self, args: &[&str]) -> Result<String> {
        let argv = [
            "env",
            "DEBIAN_FRONTEND=noninteractive",
            "apt-get",
            "-o",
            "DPkg::Lock::Timeout=120",
        ]
        .iter()
        .chain(args)
        .map(|arg| arg.to_string());
        self.ctx
            .system
            .check(&privileged(argv).timeout(APT_TIMEOUT))
    }

    fn installed(&self, package: &str) -> bool {
        first_line(&self.ctx, &["dpkg-query", "-W", "-f=${Status}", package]).is_some_and(
            |status| status.ends_with("installed") && !status.contains("not-installed"),
        )
    }
}

impl Step for PackagesStep {
    fn name(&self) -> String {
        format!("Install {}", PACKAGES.join(", "))
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.missing = PACKAGES
            .iter()
            .filter(|package| !self.installed(package))
            .map(|package| package.to_string())
            .collect();
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.missing.is_empty() {
            ctx.log("All packages are already installed");
            return Ok(());
        }
        ctx.log("Refreshing the package lists");
        self.apt(&["update"])?;
        ctx.log(format!("Installing {}", self.missing.join(", ")));
        let mut install = vec!["install", "-y", "--no-install-recommends"];
        install.extend(self.missing.iter().map(String::as_str));
        let out = self.apt(&install)?;
        if let Some(line) = out.lines().rev().find(|line| !line.trim().is_empty()) {
            ctx.log(line.to_string());
        }
        Ok(())
    }

    fn verify(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let still: Vec<&str> = PACKAGES
            .iter()
            .copied()
            .filter(|p| !self.installed(p))
            .collect();
        if still.is_empty() {
            Ok(())
        } else {
            Err(Error::Internal(format!(
                "Still not installed: {}",
                still.join(", ")
            )))
        }
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        ctx.log("System packages are left installed (removing them could break other software)");
        Ok(())
    }
}

/// Downloads the uv archive and checks it against the published SHA-256.
/// Skipped when a working uv is already in `~/.local/bin` or on PATH.
struct UvDownloadStep {
    ctx: Context,
    releases: String,
    skip: bool,
}

fn uv_archive(ctx: &Context) -> PathBuf {
    ctx.paths
        .data()
        .join("downloads")
        .join(format!("{}.tar.gz", uv_target()))
}

impl Step for UvDownloadStep {
    fn name(&self) -> String {
        format!("Download {}.tar.gz", uv_target())
    }

    fn prepare(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if let Some(version) = uv_version(&self.ctx) {
            ctx.log(format!("{version} is already installed"));
            self.skip = true;
        }
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.skip {
            return Ok(());
        }
        let name = format!("{}.tar.gz", uv_target());
        let archive = uv_archive(&self.ctx);
        ensure_dir(archive.parent().unwrap_or(Path::new(".")))?;
        let url = format!("{}/{name}", self.releases);
        ctx.log(format!("Downloading {url}"));
        let bytes = download_to(&self.ctx, &url, &archive, ctx)?;
        ctx.log(format!("{:.1} MB downloaded", bytes as f64 / 1e6));

        let sums = get_text(&self.ctx, &format!("{url}.sha256"))?;
        let expected = sums
            .split_whitespace()
            .next()
            .filter(|hash| hash.len() == 64)
            .map(str::to_ascii_lowercase)
            .ok_or_else(|| Error::Internal(format!("{name}.sha256 has no checksum")))?;
        let out = self.ctx.system.check(
            &CommandSpec::new(["sha256sum", archive.display().to_string().as_str()])
                .timeout(Duration::from_secs(60)),
        )?;
        let actual = out
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if actual != expected {
            let _ = std::fs::remove_file(&archive);
            return Err(Error::Internal(format!(
                "Checksum mismatch for {name}: expected {expected}, got {actual}. The download was deleted; try again."
            )));
        }
        ctx.log(format!("sha256 {actual} matches"));
        Ok(())
    }

    fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let _ = std::fs::remove_file(uv_archive(&self.ctx));
        Ok(())
    }
}

/// Puts `uv` and `uvx` into `~/.local/bin`, keeping any previous files to
/// restore on rollback.
struct UvInstallStep {
    ctx: Context,
    skip: bool,
    backups: Vec<(PathBuf, PathBuf)>,
    created: Vec<PathBuf>,
}

impl Step for UvInstallStep {
    fn name(&self) -> String {
        format!("Install uv and uvx into {}", local_bin(&self.ctx).display())
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.skip = uv_version(&self.ctx).is_some();
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.skip {
            return Ok(());
        }
        let archive = uv_archive(&self.ctx);
        let staging = self.ctx.paths.data().join("downloads").join(".uv-staging");
        let _ = std::fs::remove_dir_all(&staging);
        ensure_dir(&staging)?;
        self.ctx.system.check(
            &CommandSpec::new([
                "tar",
                "-xzf",
                archive.display().to_string().as_str(),
                "-C",
                staging.display().to_string().as_str(),
                "--strip-components=1",
            ])
            .timeout(Duration::from_secs(120)),
        )?;
        let bin = local_bin(&self.ctx);
        ensure_dir(&bin)?;
        for name in UV_BINARIES {
            let source = staging.join(name);
            if !source.is_file() {
                return Err(Error::Internal(format!(
                    "{name} is missing from the uv archive"
                )));
            }
            let target = bin.join(name);
            if target.symlink_metadata().is_ok() {
                let backup = bin.join(format!(".{name}.cabin-backup"));
                std::fs::rename(&target, &backup).map_err(|e| Error::path("move", &target, e))?;
                self.backups.push((target.clone(), backup));
            }
            std::fs::rename(&source, &target).map_err(|e| Error::path("move", &target, e))?;
            self.created.push(target.clone());
            ctx.log(format!("Installed {}", target.display()));
        }
        let _ = std::fs::remove_dir_all(&staging);
        let _ = std::fs::remove_file(&archive);
        Ok(())
    }

    fn verify(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.skip {
            return Ok(());
        }
        for name in UV_BINARIES {
            let path = local_bin(&self.ctx).join(name);
            let version = first_line(
                &self.ctx,
                &[path.display().to_string().as_str(), "--version"],
            )
            .ok_or_else(|| Error::Internal(format!("{} does not run", path.display())))?;
            ctx.log(format!("{} → {version}", path.display()));
        }
        // The backups are no longer needed once the new binaries run.
        for (_, backup) in self.backups.drain(..) {
            let _ = std::fs::remove_file(backup);
        }
        Ok(())
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        for path in self.created.drain(..) {
            let _ = std::fs::remove_file(&path);
        }
        for (target, backup) in self.backups.drain(..) {
            std::fs::rename(&backup, &target).map_err(|e| Error::path("restore", &target, e))?;
            ctx.log(format!("Restored {}", target.display()));
        }
        Ok(())
    }
}

/// Proves the environment works: creates a throwaway venv and runs its pip.
struct SmokeTestStep {
    ctx: Context,
}

impl Step for SmokeTestStep {
    fn name(&self) -> String {
        "Create a test virtual environment".into()
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let dir = self
            .ctx
            .paths
            .data()
            .join("downloads")
            .join(".python-smoke-test");
        let _ = std::fs::remove_dir_all(&dir);
        ensure_dir(dir.parent().unwrap_or(Path::new(".")))?;
        let result = (|| {
            self.ctx.system.check(
                &CommandSpec::new(["python3", "-m", "venv", dir.display().to_string().as_str()])
                    .timeout(Duration::from_secs(120)),
            )?;
            let python = dir.join("bin").join("python");
            let pip = self.ctx.system.check(
                &CommandSpec::new([
                    python.display().to_string().as_str(),
                    "-m",
                    "pip",
                    "--version",
                ])
                .timeout(Duration::from_secs(60)),
            )?;
            ctx.log(format!(
                "venv OK: {}",
                pip.lines()
                    .next()
                    .unwrap_or("")
                    .split(" from ")
                    .next()
                    .unwrap_or("")
            ));
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&dir);
        result
    }
}

// -------------------------------------------------------------- operation

/// Install (or repair) the Python environment. Each part is skipped when it
/// is already in place, so this is also the "Repair" action.
pub fn install_operation(ctx: &Context) -> Operation {
    install_operation_from(ctx, UV_RELEASES)
}

fn install_operation_from(ctx: &Context, releases: &str) -> Operation {
    Operation::new("python.install", "Install the Python environment")
        .key("setup-apt")
        .request(serde_json::json!({}))
        .step(PackagesStep {
            ctx: ctx.clone(),
            missing: Vec::new(),
        })
        .step(UvDownloadStep {
            ctx: ctx.clone(),
            releases: releases.trim_end_matches('/').to_string(),
            skip: false,
        })
        .step(UvInstallStep {
            ctx: ctx.clone(),
            skip: false,
            backups: Vec::new(),
            created: Vec::new(),
        })
        .step(SmokeTestStep { ctx: ctx.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OperationState;
    use crate::ops::OperationManager;
    use crate::paths::AppPaths;
    use crate::store::Store;
    use crate::system::fake::{FakeSystem, fail, ok};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const SUM: &str = "74f2f5c6024d397481db228db8fc25eed420db192243f88438c118e7e013e585";

    fn context(dir: &Path, system: Arc<FakeSystem>) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system,
        }
    }

    /// Serves the uv archive and its `.sha256`; returns the base URL and
    /// the request paths seen.
    fn release_server(sum: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/download", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 2 {
                    line.clear();
                }
                let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(path.clone());
                let body = if path.ends_with(".sha256") {
                    format!("{sum}  {}.tar.gz\n", uv_target()).into_bytes()
                } else {
                    b"archive".to_vec()
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        (base, seen)
    }

    /// A fresh minimal image: python3 without venv/pip, no uv, none of the
    /// packages; apt-get "installs" them, tar unpacks uv.
    fn fresh_machine(fake: &FakeSystem) -> Arc<AtomicBool> {
        let apt_done = Arc::new(AtomicBool::new(false));
        let done = apt_done.clone();
        fake.on(move |spec| {
            let argv = &spec.argv;
            let joined = argv.join(" ");
            let apt = done.load(Ordering::SeqCst);
            if joined.contains("apt-get") {
                if joined.contains(" install ") {
                    done.store(true, Ordering::SeqCst);
                }
                return Some(ok(""));
            }
            match argv[0].as_str() {
                "dpkg-query" => Some(if apt {
                    ok("install ok installed")
                } else {
                    fail("no packages found")
                }),
                "python3" if argv.get(1).map(String::as_str) == Some("--version") => {
                    Some(ok("Python 3.12.3\n"))
                }
                "python3"
                    if argv.get(1).map(String::as_str) == Some("-c")
                        || joined.contains("-m pip") =>
                {
                    Some(if apt {
                        ok("pip 24.0 from /usr/lib/python3/dist-packages/pip (python 3.12)\n")
                    } else {
                        fail("No module named pip")
                    })
                }
                "python3" => Some(if apt {
                    ok("")
                } else {
                    fail("ensurepip is not available")
                }),
                "sha256sum" => Some(ok(&format!("{SUM}  {}\n", argv[1]))),
                "tar" => {
                    let dest =
                        PathBuf::from(&argv[argv.iter().position(|a| a == "-C").unwrap() + 1]);
                    for name in UV_BINARIES {
                        std::fs::write(dest.join(name), b"").unwrap();
                    }
                    Some(ok(""))
                }
                program if program.ends_with("/bin/uv") || program.ends_with("/bin/uvx") => {
                    Some(ok(&format!(
                        "{} 0.13.8\n",
                        Path::new(program).file_name()?.to_str()?
                    )))
                }
                "uv" => Some(fail("not found")),
                program if program.ends_with("/bin/python") => {
                    Some(ok("pip 24.0 from /tmp/venv (python 3.12)\n"))
                }
                _ => None,
            }
        });
        apt_done
    }

    #[test]
    fn fresh_machine_gets_packages_uv_and_a_working_venv() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        fresh_machine(&fake);
        let ctx = context(dir.path(), fake.clone());
        assert_eq!(check(&ctx).action, Some(SetupAction::InstallPython));
        let (base, requests) = release_server(SUM);

        let record =
            OperationManager::new(ctx.store.clone()).run(install_operation_from(&ctx, &base))?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        let calls = fake.calls();
        let install = calls
            .iter()
            .find(|call| call.contains("apt-get") && call.contains(" install "))
            .expect("apt-get install");
        for package in PACKAGES {
            assert!(install.contains(package), "{install}");
        }
        assert!(
            calls.iter().any(|call| call.contains("python3 -m venv")),
            "{calls:?}"
        );
        let bin = dir.path().join("home/.local/bin");
        assert!(bin.join("uv").is_file() && bin.join("uvx").is_file());
        assert!(
            !uv_archive(&ctx).exists(),
            "the archive is removed after installing"
        );
        let requests = requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|path| path.ends_with(&format!("{}.tar.gz.sha256", uv_target())))
        );

        let row = check(&ctx);
        assert_eq!(row.status, CheckStatus::Ok, "{row:?}");
        assert_eq!(row.detail, "Python 3.12.3 · pip 24.0 · uv 0.13.8");
        Ok(())
    }

    #[test]
    fn checksum_mismatch_leaves_local_bin_untouched() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        fresh_machine(&fake);
        let ctx = context(dir.path(), fake.clone());
        let bin = dir.path().join("home/.local/bin");
        std::fs::create_dir_all(&bin)?;
        // A broken uv the user had: kept as is when the install fails.
        std::fs::write(bin.join("uvx"), b"old")?;
        fake.on(|spec| {
            (spec.argv[0].ends_with("/bin/uv") || spec.argv[0].ends_with("/bin/uvx"))
                .then(|| fail("broken"))
        });
        let (base, _requests) =
            release_server("0000000000000000000000000000000000000000000000000000000000000000");

        let record =
            OperationManager::new(ctx.store.clone()).run(install_operation_from(&ctx, &base))?;
        assert_eq!(record.state, OperationState::RolledBack, "{record:?}");
        assert!(
            record
                .error
                .unwrap_or_default()
                .contains("Checksum mismatch")
        );
        assert_eq!(std::fs::read(bin.join("uvx"))?, b"old");
        assert!(!bin.join("uv").exists());
        assert!(!fake.calls().iter().any(|call| call.starts_with("tar ")));
        Ok(())
    }

    #[test]
    fn complete_environment_needs_no_changes() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let apt_done = fresh_machine(&fake);
        apt_done.store(true, Ordering::SeqCst);
        fake.on(|spec| (spec.argv[0] == "uv").then(|| ok("uv 0.12.0\n")));
        let ctx = context(dir.path(), fake.clone());
        assert_eq!(check(&ctx).status, CheckStatus::Ok);

        let record = OperationManager::new(ctx.store.clone())
            .run(install_operation_from(&ctx, "http://127.0.0.1:9"))?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        let calls = fake.calls();
        assert!(
            !calls.iter().any(|call| call.contains("apt-get")),
            "{calls:?}"
        );
        assert!(
            !calls.iter().any(|call| call.starts_with("tar ")),
            "{calls:?}"
        );
        Ok(())
    }
}
