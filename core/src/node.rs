//! Node.js version management inside the VM.
//!
//! Ubuntu's apt `nodejs` is too old for the agent CLIs (24.04 ships 18), so
//! Cabin installs the official builds from nodejs.org (or a mirror) into
//! `/opt/node/<version>/` (`AppPaths::node_root`), verifies them against
//! `SHASUMS256.txt`, and points `/opt/node/current` at the selected one.
//! `/opt/node` is created once with sudo and owned by the VM user, so every
//! later change runs unprivileged. Sessions get `/opt/node/current/bin` first
//! on PATH, and `~/.local/bin/{node,npm,…}` link to it so other terminals use
//! the same version. Switching versions only flips the `current` symlink;
//! agent CLIs live in `~/.local` and keep working.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::error::{Error, Result};
use crate::fetch::{download_to, get_text};
use crate::fsx::ensure_dir;
use crate::model::{NodeInstalled, NodeRelease, NodeRuntime, NodeSource, NodeState};
use crate::ops::{Operation, Step, StepCtx};
use crate::system::{CommandSpec, privileged};

/// Agent CLIs need at least this major; older lines are end-of-life.
pub const MINIMUM_MAJOR: u32 = 22;
/// The Active LTS line, used when the release index cannot be read.
pub const RECOMMENDED_MAJOR: u32 = 26;
pub const DEFAULT_MIRROR: &str = "https://nodejs.org/dist";
/// Executables linked into `~/.local/bin`.
const LINKED: [&str; 4] = ["node", "npm", "npx", "corepack"];
const SETTINGS_KEY: &str = "node";
/// Shared libraries the official linux builds load that a minimal Ubuntu
/// image may lack, with the package providing each. The arm64 builds link
/// `libatomic.so.1`, which Ubuntu cloud images do not install.
const RUNTIME_LIBS: [(&str, &str); 1] = [("libatomic.so.1", "libatomic1")];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NodeSettings {
    mirror: Option<String>,
}

// ------------------------------------------------------------------ helpers

/// `v26.1.0` → (26, 1, 0).
pub fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.trim().trim_start_matches('v').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

pub fn major(version: &str) -> Option<u32> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// Normalise and validate a version so it is safe to use in paths and URLs.
fn clean_version(version: &str) -> Result<String> {
    let (a, b, c) = parse_version(version).ok_or_else(|| {
        Error::invalid(format!("“{version}” is not a Node.js version like v26.1.0"))
    })?;
    Ok(format!("v{a}.{b}.{c}"))
}

fn clean_mirror(mirror: &str) -> Result<String> {
    let mirror = mirror.trim().trim_end_matches('/');
    let parsed = url::Url::parse(mirror)
        .map_err(|_| Error::invalid(format!("“{mirror}” is not a valid mirror URL")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(Error::invalid("The mirror must be an http(s) URL"));
    }
    Ok(mirror.to_string())
}

/// Architecture tag used in release file names (`linux-arm64`).
pub fn platform() -> String {
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    format!("linux-{arch}")
}

fn archive_name(version: &str) -> String {
    format!("node-{version}-{}.tar.gz", platform())
}

/// Versions sit directly in the root: `/opt/node/v26.1.0`.
fn versions_dir(ctx: &Context) -> PathBuf {
    ctx.paths.node_root()
}

fn archive_path(ctx: &Context, version: &str) -> PathBuf {
    ctx.paths.node_downloads().join(archive_name(version))
}

fn current_link(ctx: &Context) -> PathBuf {
    ctx.paths.node_root().join("current")
}

fn version_dir(ctx: &Context, version: &str) -> PathBuf {
    versions_dir(ctx).join(version)
}

fn is_installed(ctx: &Context, version: &str) -> bool {
    version_dir(ctx, version).join("bin").join("node").is_file()
}

/// The version `current` points at, if any.
fn current_version(ctx: &Context) -> Option<String> {
    let target = std::fs::read_link(current_link(ctx)).ok()?;
    target.file_name()?.to_str().map(str::to_string)
}

fn first_line(ctx: &Context, argv: &[&str]) -> Option<String> {
    let out = ctx
        .system
        .run(&CommandSpec::new(argv.iter().copied()).timeout(Duration::from_secs(10)))
        .ok()?;
    out.success()
        .then(|| out.stdout.lines().next().unwrap_or("").trim().to_string())
        .filter(|line| !line.is_empty())
}

/// `<node> --version`, or an error that says why it does not run (exit
/// code and stderr, e.g. a missing shared library).
fn node_version(ctx: &Context, node: &Path) -> Result<String> {
    let out = ctx.system.run(
        &CommandSpec::new([node.display().to_string(), "--version".to_string()])
            .timeout(Duration::from_secs(30)),
    )?;
    let line = out.stdout.lines().next().unwrap_or("").trim().to_string();
    if out.success() && !line.is_empty() {
        return Ok(line);
    }
    let stderr = out.stderr.trim();
    Err(Error::Command {
        message: format!(
            "{} does not run{}{}",
            node.display(),
            out.code
                .map(|code| format!(" (exit code {code})"))
                .unwrap_or_default(),
            if stderr.is_empty() {
                String::new()
            } else {
                format!(": {}", stderr.lines().next().unwrap_or(stderr))
            }
        ),
        output: out.combined(),
    })
}

fn which(ctx: &Context, program: &str) -> Option<PathBuf> {
    ctx.paths
        .child_path()
        .split(':')
        .map(|dir| Path::new(dir).join(program))
        .find(|path| path.is_file())
}

pub fn saved_mirror(ctx: &Context) -> String {
    ctx.store
        .setting::<NodeSettings>(SETTINGS_KEY)
        .ok()
        .flatten()
        .and_then(|settings| settings.mirror)
        .unwrap_or_else(|| DEFAULT_MIRROR.to_string())
}

fn save_mirror(ctx: &Context, mirror: &str) {
    let _ = ctx.store.set_setting(
        SETTINGS_KEY,
        &NodeSettings {
            mirror: Some(mirror.to_string()),
        },
    );
}

// -------------------------------------------------------------------- state

/// The `node` sessions run, and where it comes from.
pub fn active(ctx: &Context) -> Option<NodeRuntime> {
    let version = first_line(ctx, &["node", "--version"])?;
    let path = which(ctx, "node")
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "node".into());
    let source = if path.starts_with(&ctx.paths.node_root().display().to_string()) {
        NodeSource::Managed
    } else if path == "/usr/bin/node" {
        NodeSource::System
    } else {
        NodeSource::Other
    };
    Some(NodeRuntime {
        version,
        npm: first_line(ctx, &["npm", "--version"]),
        path,
        source,
    })
}

fn system_node(ctx: &Context) -> Option<NodeRuntime> {
    if !Path::new("/usr/bin/node").is_file() {
        return None;
    }
    Some(NodeRuntime {
        version: first_line(ctx, &["/usr/bin/node", "--version"])?,
        npm: None,
        path: "/usr/bin/node".into(),
        source: NodeSource::System,
    })
}

pub fn installed(ctx: &Context) -> Vec<NodeInstalled> {
    let current = current_version(ctx);
    let mut list: Vec<NodeInstalled> = std::fs::read_dir(versions_dir(ctx))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            parse_version(&name)?;
            is_installed(ctx, &name).then(|| NodeInstalled {
                current: current.as_deref() == Some(name.as_str()),
                path: entry.path().display().to_string(),
                version: name,
            })
        })
        .collect();
    list.sort_by_key(|item| std::cmp::Reverse(parse_version(&item.version)));
    list
}

pub fn state(ctx: &Context) -> NodeState {
    NodeState {
        active: active(ctx),
        installed: installed(ctx),
        system: system_node(ctx),
        minimum_major: MINIMUM_MAJOR,
        recommended_major: RECOMMENDED_MAJOR,
        mirror: saved_mirror(ctx),
        install_dir: versions_dir(ctx).display().to_string(),
        platform: platform(),
    }
}

// ----------------------------------------------------------------- releases

#[derive(Deserialize)]
struct IndexEntry {
    version: String,
    #[serde(default)]
    date: String,
    #[serde(default)]
    files: Vec<String>,
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    lts: serde_json::Value,
    #[serde(default)]
    security: bool,
}

/// Parse `index.json` into the releases this machine can run, newest first.
pub fn parse_index(ctx: &Context, text: &str) -> Result<Vec<NodeRelease>> {
    let entries: Vec<IndexEntry> = serde_json::from_str(text)
        .map_err(|error| Error::invalid(format!("The release index could not be read: {error}")))?;
    let tag = platform();
    let mut releases: Vec<NodeRelease> = entries
        .into_iter()
        .filter(|entry| entry.files.contains(&tag))
        .filter_map(|entry| {
            let (major, _, _) = parse_version(&entry.version)?;
            Some(NodeRelease {
                installed: is_installed(ctx, &entry.version),
                lts: entry.lts.as_str().map(str::to_string),
                version: entry.version,
                major,
                date: entry.date,
                npm: entry.npm,
                security: entry.security,
            })
        })
        .collect();
    releases.sort_by_key(|release| std::cmp::Reverse(parse_version(&release.version)));
    Ok(releases)
}

pub fn releases(ctx: &Context, mirror: Option<&str>) -> Result<Vec<NodeRelease>> {
    let mirror = clean_mirror(mirror.unwrap_or(&saved_mirror(ctx)))?;
    let text = get_text(ctx, &format!("{mirror}/index.json")).map_err(|error| {
        Error::Unavailable(format!(
            "Could not load the Node.js release list from {mirror}. {error}"
        ))
    })?;
    parse_index(ctx, &text)
}

/// The newest LTS release in the list.
pub fn latest_lts(releases: &[NodeRelease]) -> Option<&NodeRelease> {
    releases
        .iter()
        .filter(|release| release.lts.is_some())
        .max_by_key(|release| parse_version(&release.version))
}

// -------------------------------------------------------------------- steps

/// Downloads the archive (skipped when the version is already installed).
struct DownloadStep {
    ctx: Context,
    version: String,
    mirror: String,
    skip: bool,
}

impl DownloadStep {
    fn archive(&self) -> PathBuf {
        archive_path(&self.ctx, &self.version)
    }
}

impl Step for DownloadStep {
    fn name(&self) -> String {
        format!("Download {}", archive_name(&self.version))
    }

    fn prepare(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        self.skip = is_installed(&self.ctx, &self.version);
        if self.skip {
            ctx.log(format!("{} is already installed", self.version));
        }
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.skip {
            return Ok(());
        }
        let archive = self.archive();
        ensure_dir(archive.parent().unwrap_or(Path::new(".")))?;
        let url = format!(
            "{}/{}/{}",
            self.mirror,
            self.version,
            archive_name(&self.version)
        );
        ctx.log(format!("Downloading {url}"));
        let bytes = download_to(&self.ctx, &url, &archive, ctx)?;
        ctx.log(format!("{:.1} MB downloaded", bytes as f64 / 1e6));
        Ok(())
    }

    fn rollback(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let _ = std::fs::remove_file(self.archive());
        Ok(())
    }
}

/// Compares the archive with the published `SHASUMS256.txt`.
struct ChecksumStep {
    ctx: Context,
    version: String,
    mirror: String,
    skip: bool,
}

impl Step for ChecksumStep {
    fn name(&self) -> String {
        "Verify the SHA-256 checksum".into()
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.skip = is_installed(&self.ctx, &self.version);
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.skip {
            return Ok(());
        }
        let name = archive_name(&self.version);
        let url = format!("{}/{}/SHASUMS256.txt", self.mirror, self.version);
        let sums = get_text(&self.ctx, &url)?;
        let expected = sums
            .lines()
            .find_map(|line| {
                let (hash, file) = line.split_once(char::is_whitespace)?;
                (file.trim() == name).then(|| hash.to_ascii_lowercase())
            })
            .ok_or_else(|| Error::Internal(format!("{name} is not listed in SHASUMS256.txt")))?;
        let archive = archive_path(&self.ctx, &self.version);
        let out = self.ctx.system.check(
            &CommandSpec::new(["sha256sum", archive.display().to_string().as_str()])
                .timeout(Duration::from_secs(120)),
        )?;
        let actual = out
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if actual != expected {
            let _ = std::fs::remove_file(&archive);
            return Err(Error::Internal(format!(
                "Checksum mismatch for {name}: expected {expected}, got {actual}. The download was deleted; try again or choose another mirror."
            )));
        }
        ctx.log(format!("sha256 {actual} matches"));
        Ok(())
    }
}

/// Creates the install root (`/opt/node`) once, owned by the VM user, so the
/// rest of the install and every later switch runs without sudo.
struct NodeRootStep {
    ctx: Context,
    created: bool,
}

/// True when the current user can create entries in `dir`.
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".write-test-{}", std::process::id()));
    let ok = std::fs::File::create(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

impl Step for NodeRootStep {
    fn name(&self) -> String {
        format!("Prepare {}", self.ctx.paths.node_root().display())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let root = self.ctx.paths.node_root();
        if root.is_dir() {
            if writable(&root) {
                ctx.log(format!("{} is ready", root.display()));
                return Ok(());
            }
            // Never take over a directory someone else made.
            return Err(Error::Conflict(format!(
                "{} exists but is not writable by this user. Change its owner, or set CABIN_NODE_ROOT to another directory.",
                root.display()
            )));
        }
        // A writable parent needs no privileges (custom roots, tests).
        if std::fs::create_dir_all(&root).is_ok() {
            self.created = true;
            ctx.log(format!("Created {}", root.display()));
            return Ok(());
        }
        let user = first_line(&self.ctx, &["id", "-un"])
            .ok_or_else(|| Error::Internal("Could not determine the user name".into()))?;
        let group = first_line(&self.ctx, &["id", "-gn"]).unwrap_or_else(|| user.clone());
        self.ctx.system.check(
            &privileged([
                "install".to_string(),
                "-d".into(),
                "-m".into(),
                "0755".into(),
                "-o".into(),
                user.clone(),
                "-g".into(),
                group.clone(),
                root.display().to_string(),
            ])
            .timeout(Duration::from_secs(30)),
        )?;
        self.created = true;
        ctx.log(format!("Created {} (owner {user}:{group})", root.display()));
        Ok(())
    }

    fn verify(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let root = self.ctx.paths.node_root();
        if root.is_dir() && writable(&root) {
            Ok(())
        } else {
            Err(Error::Internal(format!(
                "{} is not writable",
                root.display()
            )))
        }
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let root = self.ctx.paths.node_root();
        let empty = std::fs::read_dir(&root).is_ok_and(|mut entries| entries.next().is_none());
        if self.created && empty && std::fs::remove_dir(&root).is_err() {
            let _ = self.ctx.system.run(&privileged([
                "rmdir".to_string(),
                root.display().to_string(),
            ]));
            ctx.log(format!("Removed {}", root.display()));
        }
        Ok(())
    }
}

/// Installs the system libraries the official builds need (`RUNTIME_LIBS`)
/// when the image lacks them.
struct RuntimeLibsStep {
    ctx: Context,
}

impl RuntimeLibsStep {
    /// Packages whose library the dynamic linker cannot find.
    fn missing(&self) -> Vec<&'static str> {
        let cache = self
            .ctx
            .system
            .run(&CommandSpec::new(["/sbin/ldconfig", "-p"]).timeout(Duration::from_secs(20)))
            .map(|out| out.stdout)
            .unwrap_or_default();
        RUNTIME_LIBS
            .iter()
            .filter(|(lib, _)| !cache.contains(lib))
            .map(|(_, package)| *package)
            .collect()
    }

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
            .check(&privileged(argv).timeout(Duration::from_secs(600)))
    }
}

impl Step for RuntimeLibsStep {
    fn name(&self) -> String {
        "Install required system libraries".into()
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let missing = self.missing();
        if missing.is_empty() {
            ctx.log("All required libraries are present");
            return Ok(());
        }
        ctx.log(format!("Installing {}", missing.join(", ")));
        let mut install = vec!["install", "-y", "--no-install-recommends"];
        install.extend(&missing);
        if self.apt(&install).is_err() {
            // Stale or never-fetched package lists: refresh once and retry.
            ctx.log("Refreshing the package lists");
            self.apt(&["update"])?;
            self.apt(&install)?;
        }
        Ok(())
    }

    fn verify(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        let missing = self.missing();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(Error::Internal(format!(
                "Still missing after apt-get: {}",
                missing.join(", ")
            )))
        }
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        // Shared system libraries are harmless to keep.
        ctx.log("Installed libraries are kept");
        Ok(())
    }
}

/// Unpacks into `<root>/<version>` through a staging directory.
struct UnpackStep {
    ctx: Context,
    version: String,
    skip: bool,
}

impl UnpackStep {
    fn staging(&self) -> PathBuf {
        versions_dir(&self.ctx).join(format!(".staging-{}", self.version))
    }
}

impl Step for UnpackStep {
    fn name(&self) -> String {
        format!(
            "Unpack into {}",
            version_dir(&self.ctx, &self.version).display()
        )
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.skip = is_installed(&self.ctx, &self.version);
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if self.skip {
            return Ok(());
        }
        let staging = self.staging();
        let _ = std::fs::remove_dir_all(&staging);
        ensure_dir(&staging)?;
        let archive = archive_path(&self.ctx, &self.version);
        self.ctx.system.check(
            &CommandSpec::new([
                "tar",
                "-xzf",
                archive.display().to_string().as_str(),
                "-C",
                staging.display().to_string().as_str(),
                "--strip-components=1",
            ])
            .timeout(Duration::from_secs(300)),
        )?;
        let target = version_dir(&self.ctx, &self.version);
        let _ = std::fs::remove_dir_all(&target);
        std::fs::rename(&staging, &target).map_err(|e| Error::path("move", &target, e))?;
        let _ = std::fs::remove_file(&archive);
        ctx.log(format!("Installed in {}", target.display()));
        Ok(())
    }

    fn verify(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let node = version_dir(&self.ctx, &self.version)
            .join("bin")
            .join("node");
        let reported = node_version(&self.ctx, &node)?;
        if reported != self.version {
            return Err(Error::Internal(format!(
                "{} reports {reported}, expected {}",
                node.display(),
                self.version
            )));
        }
        ctx.log(format!("{} → {reported}", node.display()));
        Ok(())
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let _ = std::fs::remove_dir_all(self.staging());
        if !self.skip {
            let _ = std::fs::remove_dir_all(version_dir(&self.ctx, &self.version));
            ctx.log("Removed the partial install");
        }
        Ok(())
    }
}

/// Points `current` at a version and links `~/.local/bin/{node,npm,…}`.
struct ActivateStep {
    ctx: Context,
    version: String,
    previous: Option<PathBuf>,
    created_links: Vec<PathBuf>,
}

impl ActivateStep {
    fn new(ctx: &Context, version: &str) -> Self {
        Self {
            ctx: ctx.clone(),
            version: version.to_string(),
            previous: None,
            created_links: Vec::new(),
        }
    }

    fn point_current(&self, target: &Path) -> Result<()> {
        let link = current_link(&self.ctx);
        let tmp = self.ctx.paths.node_root().join(".current.tmp");
        let _ = std::fs::remove_file(&tmp);
        std::os::unix::fs::symlink(target, &tmp).map_err(|e| Error::path("link", &tmp, e))?;
        std::fs::rename(&tmp, &link).map_err(|e| Error::path("replace", &link, e))
    }
}

impl Step for ActivateStep {
    fn name(&self) -> String {
        format!("Make {} the default node", self.version)
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        self.previous = std::fs::read_link(current_link(&self.ctx)).ok();
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        if !is_installed(&self.ctx, &self.version) {
            return Err(Error::NotFound(format!("Node.js {}", self.version)));
        }
        ensure_dir(&self.ctx.paths.node_root())?;
        self.point_current(&version_dir(&self.ctx, &self.version))?;
        ctx.log(format!(
            "{} → {}",
            current_link(&self.ctx).display(),
            self.version
        ));

        let bin = self.ctx.paths.home().join(".local").join("bin");
        ensure_dir(&bin)?;
        for name in LINKED {
            let link = bin.join(name);
            let target = self.ctx.paths.node_bin().join(name);
            match std::fs::read_link(&link) {
                Ok(existing) if existing == target => {}
                Ok(_) | Err(_) if link.symlink_metadata().is_ok() => {
                    ctx.log(format!(
                        "{} already exists and is not ours; left as is",
                        link.display()
                    ));
                }
                _ => {
                    std::os::unix::fs::symlink(&target, &link)
                        .map_err(|e| Error::path("link", &link, e))?;
                    self.created_links.push(link);
                }
            }
        }
        Ok(())
    }

    fn verify(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let node = self.ctx.paths.node_bin().join("node");
        let reported = node_version(&self.ctx, &node)?;
        if reported != self.version {
            return Err(Error::Internal(format!(
                "{} reports {reported}, expected {}",
                node.display(),
                self.version
            )));
        }
        ctx.log(format!("node {} is now the default", self.version));
        Ok(())
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        for link in self.created_links.drain(..) {
            let _ = std::fs::remove_file(link);
        }
        match &self.previous {
            Some(previous) => {
                self.point_current(previous)?;
                ctx.log(format!("Restored {}", previous.display()));
            }
            None => {
                let _ = std::fs::remove_file(current_link(&self.ctx));
            }
        }
        Ok(())
    }
}

struct RemoveStep {
    ctx: Context,
    version: String,
}

impl Step for RemoveStep {
    fn name(&self) -> String {
        format!("Remove Node.js {}", self.version)
    }

    fn prepare(&mut self, _ctx: &StepCtx<'_>) -> Result<()> {
        if !version_dir(&self.ctx, &self.version).is_dir() {
            return Err(Error::NotFound(format!("Node.js {}", self.version)));
        }
        if current_version(&self.ctx).as_deref() == Some(self.version.as_str()) {
            return Err(Error::Conflict(format!(
                "{} is the default node. Switch to another version first.",
                self.version
            )));
        }
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let dir = version_dir(&self.ctx, &self.version);
        std::fs::remove_dir_all(&dir).map_err(|e| Error::path("remove", &dir, e))?;
        ctx.log(format!("Deleted {}", dir.display()));
        Ok(())
    }

    fn rollback(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        ctx.log("Reinstall the version if you need it again");
        Ok(())
    }
}

// --------------------------------------------------------------- operations

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRequest {
    pub version: String,
    pub mirror: String,
    pub activate: bool,
}

pub fn install_operation(ctx: &Context, request: InstallRequest) -> Result<Operation> {
    let version = clean_version(&request.version)?;
    let mirror = clean_mirror(&request.mirror)?;
    save_mirror(ctx, &mirror);
    let request = InstallRequest {
        version: version.clone(),
        mirror: mirror.clone(),
        activate: request.activate,
    };
    let mut operation = Operation::new("node.install", format!("Install Node.js {version}"))
        .key("node")
        .request(serde_json::to_value(&request)?)
        .step(DownloadStep {
            ctx: ctx.clone(),
            version: version.clone(),
            mirror: mirror.clone(),
            skip: false,
        })
        .step(ChecksumStep {
            ctx: ctx.clone(),
            version: version.clone(),
            mirror,
            skip: false,
        })
        .step(NodeRootStep {
            ctx: ctx.clone(),
            created: false,
        })
        .step(RuntimeLibsStep { ctx: ctx.clone() })
        .step(UnpackStep {
            ctx: ctx.clone(),
            version: version.clone(),
            skip: false,
        });
    if request.activate {
        operation = operation.step(ActivateStep::new(ctx, &version));
    }
    Ok(operation)
}

/// Install the newest LTS from the saved mirror and make it the default.
pub fn install_latest_lts_operation(ctx: &Context) -> Result<Operation> {
    let mirror = saved_mirror(ctx);
    let list = releases(ctx, Some(&mirror))?;
    let release = latest_lts(&list).ok_or_else(|| {
        Error::Unavailable("The release list has no LTS build for this machine".into())
    })?;
    install_operation(
        ctx,
        InstallRequest {
            version: release.version.clone(),
            mirror,
            activate: true,
        },
    )
}

pub fn use_operation(ctx: &Context, version: &str) -> Result<Operation> {
    let version = clean_version(version)?;
    if !is_installed(ctx, &version) {
        return Err(Error::NotFound(format!(
            "Node.js {version} is not installed"
        )));
    }
    Ok(
        Operation::new("node.use", format!("Switch to Node.js {version}"))
            .key("node")
            .request(serde_json::json!({ "version": version }))
            .step(ActivateStep::new(ctx, &version)),
    )
}

pub fn remove_operation(ctx: &Context, version: &str) -> Result<Operation> {
    let version = clean_version(version)?;
    Ok(
        Operation::new("node.remove", format!("Remove Node.js {version}"))
            .key("node")
            .request(serde_json::json!({ "version": version }))
            .step(RemoveStep {
                ctx: ctx.clone(),
                version,
            }),
    )
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

    const SUM: &str = "abc123";

    fn context(dir: &Path, system: Arc<FakeSystem>) -> Context {
        Context {
            paths: AppPaths::new(dir.join("data"), dir.join("home")),
            store: Arc::new(Store::in_memory().unwrap()),
            system,
        }
    }

    fn arg_after(argv: &[String], flag: &str) -> Option<String> {
        argv.iter()
            .position(|arg| arg == flag)
            .and_then(|index| argv.get(index + 1).cloned())
    }

    /// A local mirror serving SHASUMS256.txt and the archive (recording the
    /// paths it was asked for), plus fakes for sha256sum, tar and the
    /// unpacked `node --version`.
    fn fake_network(
        fake: &FakeSystem,
        sum: &'static str,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        use std::io::{BufRead, Write};
        let name = archive_name("v26.1.0");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mirror = format!("http://{}/dist", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = requests.clone();
        let sums = format!("ffff  other.tar.gz\n{SUM}  {name}\n");
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
                seen.lock().unwrap().push(path.clone());
                let body: Vec<u8> = if path.ends_with("SHASUMS256.txt") {
                    sums.clone().into_bytes()
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
        fake.on(move |spec| {
            let argv = &spec.argv;
            match argv[0].as_str() {
                "sha256sum" => Some(ok(&format!("{sum}  {}\n", argv[1]))),
                "/sbin/ldconfig" => Some(ok(
                    "\tlibatomic.so.1 (libc6,AArch64) => /lib/aarch64-linux-gnu/libatomic.so.1\n",
                )),
                "tar" => {
                    let dest = PathBuf::from(arg_after(argv, "-C").unwrap());
                    std::fs::create_dir_all(dest.join("bin")).unwrap();
                    std::fs::write(dest.join("bin/node"), b"").unwrap();
                    Some(ok(""))
                }
                program if program.ends_with("/bin/node") => {
                    // Report the version from the path: …/versions/vX/bin/node
                    // or …/current/bin/node (resolve the symlink).
                    let path = std::fs::canonicalize(program).ok()?;
                    let version = path.parent()?.parent()?.file_name()?.to_str()?.to_string();
                    Some(ok(&format!("{version}\n")))
                }
                _ => None,
            }
        });
        (mirror, requests)
    }

    fn install(
        ctx: &Context,
        mirror: &str,
        version: &str,
        activate: bool,
    ) -> Result<crate::model::OperationRecord> {
        OperationManager::new(ctx.store.clone()).run(install_operation(
            ctx,
            InstallRequest {
                version: version.into(),
                mirror: format!("{mirror}/"),
                activate,
            },
        )?)
    }

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("v26.1.0"), Some((26, 1, 0)));
        assert_eq!(parse_version("26.1"), None);
        assert_eq!(major("v18.19.1"), Some(18));
        assert_eq!(major("garbage"), None);
        assert!(clean_version("../etc").is_err());
        assert_eq!(clean_version("26.1.0").unwrap(), "v26.1.0");
        assert!(clean_mirror("file:///etc").is_err());
        assert_eq!(
            clean_mirror("https://npmmirror.com/mirrors/node/").unwrap(),
            "https://npmmirror.com/mirrors/node"
        );
    }

    #[test]
    fn index_keeps_this_platform_and_finds_the_latest_lts() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let ctx = context(dir.path(), Arc::new(FakeSystem::default()));
        let tag = platform();
        let index = serde_json::json!([
            {"version": "v27.0.0", "date": "2026-10-20", "files": [tag], "lts": false, "security": false},
            {"version": "v26.1.0", "date": "2026-10-28", "files": [tag], "npm": "11.6.0", "lts": "Ninja", "security": true},
            {"version": "v26.0.9", "date": "2026-09-01", "files": ["win-x64-msi"], "lts": "Ninja"},
            {"version": "v24.9.0", "date": "2026-10-01", "files": [tag], "lts": "Krypton"}
        ]);
        let list = parse_index(&ctx, &index.to_string())?;
        assert_eq!(
            list.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(),
            ["v27.0.0", "v26.1.0", "v24.9.0"]
        );
        assert_eq!(list[0].lts, None);
        let lts = latest_lts(&list).unwrap();
        assert_eq!(
            (lts.version.as_str(), lts.lts.as_deref()),
            ("v26.1.0", Some("Ninja"))
        );
        assert!(lts.security);
        Ok(())
    }

    #[test]
    fn install_verifies_unpacks_and_activates() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let (mirror, requests) = fake_network(&fake, SUM);
        let ctx = context(dir.path(), fake.clone());

        let record = install(&ctx, &mirror, "v26.1.0", true)?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        assert!(is_installed(&ctx, "v26.1.0"));
        assert_eq!(current_version(&ctx).as_deref(), Some("v26.1.0"));
        let link = dir.path().join("home/.local/bin/node");
        assert_eq!(std::fs::read_link(link)?, ctx.paths.node_bin().join("node"));
        // The archive is deleted after unpacking; the mirror was saved without
        // its trailing slash and downloads use it.
        assert!(!archive_path(&ctx, "v26.1.0").exists());
        assert!(
            !ctx.paths
                .node_downloads()
                .starts_with(ctx.paths.node_root())
        );
        // Short layout: <root>/<version>, next to <root>/current.
        assert!(ctx.paths.node_root().join("v26.1.0/bin/node").is_file());
        assert!(!fake.calls().iter().any(|call| call.contains("apt-get")));
        assert_eq!(saved_mirror(&ctx), mirror);
        let requests = requests.lock().unwrap();
        assert!(
            requests.contains(&format!("/dist/v26.1.0/{}", archive_name("v26.1.0"))),
            "{requests:?}"
        );
        let listed = installed(&ctx);
        assert_eq!(listed.len(), 1);
        assert!(listed[0].current);
        Ok(())
    }

    #[test]
    fn checksum_mismatch_rolls_back_without_installing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let (mirror, requests) = fake_network(&fake, "deadbeef");
        let ctx = context(dir.path(), fake.clone());

        let record = install(&ctx, &mirror, "v26.1.0", true)?;
        assert_eq!(record.state, OperationState::RolledBack, "{record:?}");
        assert!(
            record
                .error
                .unwrap_or_default()
                .contains("Checksum mismatch")
        );
        assert!(!is_installed(&ctx, "v26.1.0"));
        assert!(current_version(&ctx).is_none());
        assert!(!fake.calls().iter().any(|call| call.starts_with("tar ")));
        assert_eq!(
            requests.lock().unwrap().len(),
            2,
            "archive and SHASUMS256.txt"
        );
        Ok(())
    }

    #[test]
    fn switching_back_and_removing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let (mirror, _requests) = fake_network(&fake, SUM);
        let ctx = context(dir.path(), fake.clone());
        install(&ctx, &mirror, "v26.1.0", true)?;
        // A second version installed side by side, not activated.
        let other = version_dir(&ctx, "v24.9.0").join("bin");
        std::fs::create_dir_all(&other)?;
        std::fs::write(other.join("node"), b"")?;
        assert_eq!(current_version(&ctx).as_deref(), Some("v26.1.0"));

        let ops = OperationManager::new(ctx.store.clone());
        let record = ops.run(use_operation(&ctx, "v24.9.0")?)?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        assert_eq!(current_version(&ctx).as_deref(), Some("v24.9.0"));

        // The default cannot be removed; another version can.
        let record = ops.run(remove_operation(&ctx, "v24.9.0")?)?;
        assert_eq!(record.state, OperationState::Failed);
        let record = ops.run(remove_operation(&ctx, "v26.1.0")?)?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        assert!(!is_installed(&ctx, "v26.1.0"));
        Ok(())
    }

    #[test]
    fn failed_activation_restores_the_previous_default() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let (mirror, _requests) = fake_network(&fake, SUM);
        let ctx = context(dir.path(), fake.clone());
        install(&ctx, &mirror, "v26.1.0", true)?;
        let broken = version_dir(&ctx, "v24.9.0").join("bin");
        std::fs::create_dir_all(&broken)?;
        std::fs::write(broken.join("node"), b"")?;
        // This build does not run.
        fake.on(|spec| {
            spec.argv[0]
                .ends_with("current/bin/node")
                .then(|| fail("Exec format error"))
        });

        let record =
            OperationManager::new(ctx.store.clone()).run(use_operation(&ctx, "v24.9.0")?)?;
        assert_eq!(record.state, OperationState::RolledBack, "{record:?}");
        assert_eq!(current_version(&ctx).as_deref(), Some("v26.1.0"));
        Ok(())
    }

    #[test]
    fn user_files_in_local_bin_are_left_alone() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let (mirror, _requests) = fake_network(&fake, SUM);
        let ctx = context(dir.path(), fake.clone());
        let bin = dir.path().join("home/.local/bin");
        std::fs::create_dir_all(&bin)?;
        std::fs::write(bin.join("npx"), b"#!/bin/sh\n")?;

        let record = install(&ctx, &mirror, "v26.1.0", true)?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        assert!(bin.join("npx").symlink_metadata()?.file_type().is_file());
        assert!(bin.join("npm").symlink_metadata()?.file_type().is_symlink());
        Ok(())
    }

    #[test]
    fn missing_libatomic_is_installed_before_unpacking() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let (mirror, _requests) = fake_network(&fake, SUM);
        // Like a fresh Ubuntu cloud image: no libatomic until apt installs it.
        let installed_lib = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = installed_lib.clone();
        fake.on(move |spec| {
            let argv = spec.argv.join(" ");
            if argv.contains("apt-get") && argv.contains("install") {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                return Some(ok(""));
            }
            (spec.argv[0] == "/sbin/ldconfig" && !flag.load(std::sync::atomic::Ordering::SeqCst))
                .then(|| ok("\tlibc.so.6 (libc6,AArch64) => /lib/aarch64-linux-gnu/libc.so.6\n"))
        });
        let ctx = context(dir.path(), fake.clone());

        let record = install(&ctx, &mirror, "v26.1.0", true)?;
        assert_eq!(record.state, OperationState::Succeeded, "{record:?}");
        let calls = fake.calls();
        let apt = calls
            .iter()
            .position(|call| call.contains("apt-get") && call.contains("libatomic1"))
            .expect("apt-get install libatomic1");
        let tar = calls
            .iter()
            .position(|call| call.starts_with("tar "))
            .unwrap();
        assert!(apt < tar, "{calls:?}");
        assert!(
            calls[apt].contains("DEBIAN_FRONTEND=noninteractive"),
            "{}",
            calls[apt]
        );
        Ok(())
    }

    #[test]
    fn a_build_that_does_not_run_reports_why() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let fake = Arc::new(FakeSystem::default());
        let (mirror, _requests) = fake_network(&fake, SUM);
        fake.on(|spec| {
            spec.argv[0].ends_with("/bin/node").then(|| {
                fail("node: error while loading shared libraries: libatomic.so.1: cannot open shared object file")
            })
        });
        let ctx = context(dir.path(), fake.clone());

        let record = install(&ctx, &mirror, "v26.1.0", true)?;
        assert_eq!(record.state, OperationState::RolledBack, "{record:?}");
        let error = record.error.unwrap_or_default();
        assert!(error.contains("does not run"), "{error}");
        assert!(error.contains("libatomic.so.1"), "{error}");
        assert!(!is_installed(&ctx, "v26.1.0"));
        Ok(())
    }
}
