//! Shared skill masters, agent bindings and recoverable changes in the guest.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use base64::Engine;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use super::extensions::{
    owned_skill, scan_skills, skill_body, skill_front_matter, user_skills_dir, valid_skill_name,
};
use super::skill_sources::{
    self as sources, COPY_MARKER, MAX_ZIP, digest, digest_copy, manifest_text,
};
use crate::context::Context;
use crate::error::{Error, Result};
use crate::fsx;
use crate::model::{AgentKind, SkillEntry, SkillFile};
use crate::ops::{Operation, Step, StepCtx};
use crate::util::{new_id, now_millis};
pub use sources::{
    DiscoveredSkill, RegistrySkill, SkillOrigin, SkillRegistry, SkillRepository, discover, registry,
};

const REPOS_KEY: &str = "skill-library-repositories";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum SkillMode {
    #[default]
    Link,
    Copy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ManagedSkill {
    pub folder: String,
    pub name: String,
    pub description: String,
    pub origin: Option<SkillOrigin>,
    #[serde(default)]
    pub imported_from: Option<String>,
    pub digest: String,
    #[serde(default)]
    pub mode: SkillMode,
    pub targets: BTreeSet<AgentKind>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillLibrary {
    pub installed: Vec<InstalledSkill>,
    pub local: Vec<LocalSkill>,
    pub backups: Vec<SkillBackup>,
    pub repositories: Vec<SkillRepository>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkill {
    pub skill: ManagedSkill,
    pub bindings: Vec<SkillBinding>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillBinding {
    pub kind: AgentKind,
    pub state: String,
    pub wanted: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct LocalSkill {
    pub kind: AgentKind,
    pub entry: SkillEntry,
}

#[derive(Debug, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillBackup {
    pub id: String,
    pub folder: String,
    pub name: String,
    pub at: i64,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SkillInstall {
    Repository {
        origin: SkillOrigin,
    },
    Registry {
        repository: String,
        #[serde(rename = "skillId")]
        skill_id: String,
    },
    Zip {
        contents: String,
    },
    Local {
        kind: AgentKind,
        path: String,
    },
    Update {
        folder: String,
    },
}

#[derive(Debug, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SkillChange {
    Bind {
        folders: Vec<String>,
        targets: Vec<AgentKind>,
        enabled: bool,
    },
    Remove {
        folders: Vec<String>,
    },
    Restore {
        id: String,
    },
    DeleteBackup {
        id: String,
    },
    Sync,
}

fn root(ctx: &Context) -> PathBuf {
    ctx.paths.data().join("skill-library")
}
fn installed(ctx: &Context) -> PathBuf {
    root(ctx).join("installed")
}
fn backups(ctx: &Context) -> PathBuf {
    root(ctx).join("backups")
}
fn content(ctx: &Context, folder: &str) -> PathBuf {
    installed(ctx).join(folder).join("content")
}
fn native(ctx: &Context, kind: AgentKind, folder: &str) -> PathBuf {
    user_skills_dir(ctx, kind).join(folder)
}

fn directories(path: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(path) {
        Ok(entries) => entries
            .filter_map(|entry| {
                match entry.and_then(|entry| Ok((entry.file_type()?, entry.path()))) {
                    Ok((kind, path)) if kind.is_dir() => Some(Ok(path)),
                    Ok(_) => None,
                    Err(error) => Some(Err(Error::path("read", path, error))),
                }
            })
            .collect(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(Error::path("read", path, error)),
    }
}

fn metadata(path: &Path) -> Result<ManagedSkill> {
    let path = path.join("skill.json");
    let skill: ManagedSkill =
        serde_json::from_slice(&fs::read(&path).map_err(|err| Error::path("read", &path, err))?)?;
    valid_skill_name(&skill.folder)?;
    Ok(skill)
}

fn verify_master(path: &Path, skill: &ManagedSkill) -> Result<()> {
    if digest(&path.join("content"), None)? != skill.digest {
        return Err(Error::Conflict(format!(
            "The managed files for {} have local edits",
            skill.folder
        )));
    }
    Ok(())
}

fn record(path: &Path, skill: &ManagedSkill) -> Result<()> {
    fsx::atomic_write(
        &path.join("skill.json"),
        &serde_json::to_vec(skill)?,
        Some(0o600),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Location {
    Link,
    Copy,
    Local,
    Missing,
}

fn location(path: &Path, master: &Path) -> Result<Location> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() && fs::read_link(path)? == master => Ok(Location::Link),
        Ok(meta) if meta.is_dir() && copy_marker(path, master)? => Ok(Location::Copy),
        Ok(_) => Ok(Location::Local),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Location::Missing),
        Err(error) => Err(Error::path("inspect", path, error)),
    }
}

fn copy_marker(path: &Path, master: &Path) -> Result<bool> {
    let marker = path.join(COPY_MARKER);
    let expected = master.as_os_str().as_encoded_bytes();
    match fs::symlink_metadata(&marker) {
        Ok(meta) if meta.is_file() && meta.len() == expected.len() as u64 => {
            use std::io::Read;
            use std::os::unix::fs::OpenOptionsExt;
            let mut bytes = Vec::new();
            fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(marker)?
                .take(expected.len() as u64 + 1)
                .read_to_end(&mut bytes)?;
            Ok(bytes == expected)
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(Error::path("inspect", &marker, error)),
    }
}

fn binding(path: &Path, master: &Path) -> Result<&'static str> {
    Ok(match location(path, master)? {
        Location::Link | Location::Copy => "enabled",
        Location::Local => "local",
        Location::Missing => "disabled",
    })
}

fn repo_list(ctx: &Context) -> Result<Vec<SkillRepository>> {
    Ok(ctx
        .store
        .setting(REPOS_KEY)?
        .unwrap_or_else(sources::default_repositories))
}

pub fn save_repositories(
    ctx: &Context,
    repositories: Vec<SkillRepository>,
) -> Result<Vec<SkillRepository>> {
    if repositories.len() > 30 {
        return Err(Error::invalid("Keep up to 30 configured repositories"));
    }
    let mut seen = BTreeSet::new();
    let mut normalized = Vec::new();
    for repository in repositories {
        let source = repository.normalized()?;
        if seen.insert((
            source.repository.clone(),
            source.branch.clone(),
            source.directory.clone(),
        )) {
            normalized.push(source);
        }
    }
    ctx.store.set_setting(REPOS_KEY, &normalized)?;
    Ok(normalized)
}

pub fn list(ctx: &Context) -> Result<SkillLibrary> {
    let mut entries = Vec::new();
    for path in directories(&installed(ctx))? {
        let skill = metadata(&path)?;
        let master = path.join("content");
        let bindings = AgentKind::ALL
            .into_iter()
            .map(|kind| SkillBinding {
                kind,
                state: binding(&native(ctx, kind, &skill.folder), &master)
                    .unwrap_or("unreadable")
                    .to_owned(),
                wanted: skill.targets.contains(&kind),
            })
            .collect();
        entries.push(InstalledSkill { skill, bindings });
    }
    entries.sort_by(|a, b| a.skill.folder.cmp(&b.skill.folder));
    let mut local = Vec::new();
    for kind in AgentKind::ALL {
        let entries = super::adapter(kind)
            .skill_dirs(ctx, None)
            .into_iter()
            .flat_map(|(dir, scope)| scan_skills(&dir, scope));
        for entry in entries {
            let path = Path::new(&entry.path);
            let folder = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let master = content(ctx, folder);
            if matches!(location(path, &master), Ok(Location::Link | Location::Copy)) {
                continue;
            }
            local.push(LocalSkill { kind, entry });
        }
    }
    let mut saved = Vec::new();
    for path in directories(&backups(ctx))? {
        let Some(id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(at) = id.split_once('-').and_then(|(stamp, _)| stamp.parse().ok()) else {
            continue;
        };
        let Ok(skill) = metadata(&path) else {
            continue;
        };
        saved.push(SkillBackup {
            id: id.to_owned(),
            folder: skill.folder,
            name: skill.name,
            at,
        });
    }
    saved.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(SkillLibrary {
        installed: entries,
        local,
        backups: saved,
        repositories: repo_list(ctx)?,
    })
}

pub fn read(ctx: &Context, folder: &str) -> Result<SkillFile> {
    valid_skill_name(folder)?;
    let path = content(ctx, folder);
    let text = manifest_text(&path)?;
    let (name, description) = skill_front_matter(&text);
    Ok(SkillFile {
        path: path.display().to_string(),
        name: name.unwrap_or_else(|| folder.into()),
        description: description.unwrap_or_default(),
        body: skill_body(&text),
        scope: "user".into(),
    })
}

pub fn updates(ctx: &Context) -> Result<Vec<String>> {
    let mut folders = Vec::new();
    let mut cache = std::collections::BTreeMap::new();
    for path in directories(&installed(ctx))? {
        let skill = metadata(&path)?;
        let Some(origin) = skill.origin else {
            continue;
        };
        let key = (
            origin.repository.repository.clone(),
            origin.repository.branch.clone(),
            origin.repository.directory.clone(),
        );
        let remote = match cache.entry(key) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(discover(ctx, origin.repository)?)
            }
        };
        if remote.iter().any(|remote| {
            remote.origin.path == origin.path && remote.origin.revision != origin.revision
        }) {
            folders.push(skill.folder);
        }
    }
    folders.sort();
    Ok(folders)
}

struct PreparedSkill {
    directory: TempDir,
    skill: ManagedSkill,
    previous: Option<ManagedSkill>,
}

fn staging(ctx: &Context) -> Result<TempDir> {
    let path = root(ctx).join("staging");
    fsx::ensure_dir(&path)?;
    Ok(tempfile::tempdir_in(&path)?)
}

fn download(
    ctx: &Context,
    source: SkillInstall,
    targets: Vec<AgentKind>,
    mode: SkillMode,
) -> Result<PreparedSkill> {
    let directory = staging(ctx)?;
    let destination = directory.path().join("content");
    fs::create_dir(&destination)?;
    let mut previous = None;
    let mut imported_from = None;
    let (folder, origin) = match source {
        SkillInstall::Repository { origin } => {
            let (folder, origin) = sources::from_repository(ctx, origin, &destination)?;
            (folder, Some(origin))
        }
        SkillInstall::Registry {
            repository,
            skill_id,
        } => {
            let origin = sources::registry_origin(ctx, repository, &skill_id)?;
            let (folder, origin) = sources::from_repository(ctx, origin, &destination)?;
            (folder, Some(origin))
        }
        SkillInstall::Zip { contents } => {
            if contents.len() > MAX_ZIP.div_ceil(3) * 4 {
                return Err(Error::invalid("Choose a skill ZIP up to 12 MiB"));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(contents)
                .map_err(|_| Error::invalid("Invalid skill ZIP data"))?;
            (sources::from_zip(&bytes, &destination)?, None)
        }
        SkillInstall::Local { kind, path } => {
            let path = owned_skill(ctx, kind, &path, None)?;
            let folder = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            valid_skill_name(folder)?;
            if !fs::symlink_metadata(&path)?.is_dir() {
                return Err(Error::invalid(
                    "Import a regular skill directory from the agent's user skills folder",
                ));
            }
            digest(&path, Some(&destination))?;
            imported_from = Some(path.display().to_string());
            (folder.to_owned(), None)
        }
        SkillInstall::Update { folder } => {
            valid_skill_name(&folder)?;
            let skill = metadata(&installed(ctx).join(&folder))?;
            let origin = skill
                .origin
                .clone()
                .ok_or_else(|| Error::invalid("This skill has no configured update source"))?;
            let (_, origin) = sources::from_repository(ctx, origin, &destination)?;
            previous = Some(skill);
            (folder, Some(origin))
        }
    };
    let text = manifest_text(&destination)?;
    let (name, description) = skill_front_matter(&text);
    let skill = ManagedSkill {
        folder: folder.clone(),
        name: name.unwrap_or(folder),
        description: description.unwrap_or_default(),
        origin,
        imported_from,
        digest: digest(&destination, None)?,
        mode: previous.as_ref().map_or(mode, |skill| skill.mode),
        targets: previous
            .as_ref()
            .map(|skill| skill.targets.clone())
            .unwrap_or_else(|| targets.into_iter().collect()),
    };
    record(directory.path(), &skill)?;
    Ok(PreparedSkill {
        directory,
        skill,
        previous,
    })
}

pub fn install_operation(
    ctx: &Context,
    source: SkillInstall,
    targets: Vec<AgentKind>,
    mode: SkillMode,
) -> Result<Operation> {
    let skill = download(ctx, source, targets, mode)?;
    let title = if skill.previous.is_some() {
        "Update"
    } else {
        "Install"
    };
    Ok(operation(
        ctx,
        format!("{title} skill {}", skill.skill.folder),
        Request::Install(Box::new(skill)),
    ))
}

pub fn change_operation(ctx: &Context, change: SkillChange) -> Operation {
    let title = match change {
        SkillChange::Bind { enabled: true, .. } => "Enable skills",
        SkillChange::Bind { enabled: false, .. } => "Disable skills",
        SkillChange::Remove { .. } => "Uninstall skills",
        SkillChange::Restore { .. } => "Restore skill backup",
        SkillChange::DeleteBackup { .. } => "Delete skill backup",
        SkillChange::Sync => "Sync skills to agents",
    };
    operation(ctx, title.into(), Request::Change(change))
}

enum Request {
    Install(Box<PreparedSkill>),
    Change(SkillChange),
}

fn operation(ctx: &Context, title: String, request: Request) -> Operation {
    Operation::new("agent.skillLibrary", title)
        .key("skill-library")
        .step(WriteSkills {
            ctx: ctx.clone(),
            request,
            transaction: None,
            moves: Vec::new(),
            native_changes: NativeChanges::default(),
        })
}

enum NativeCheck {
    Link(PathBuf),
    Copy { master: PathBuf, digest: String },
}

struct Move {
    from: PathBuf,
    to: PathBuf,
    applied: bool,
    expected: Option<NativeCheck>,
}

struct WriteSkills {
    ctx: Context,
    request: Request,
    transaction: Option<TempDir>,
    moves: Vec<Move>,
    native_changes: NativeChanges,
}

fn absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(Error::Conflict(format!(
            "{} already exists",
            path.display()
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::path("inspect", path, error)),
    }
}

#[derive(Default)]
struct NativeChanges {
    staged: Vec<TempDir>,
    moves: Vec<Move>,
    checks: Vec<(PathBuf, PathBuf, Location)>,
}

struct Target<'a> {
    skill: &'a ManagedSkill,
    previous: Option<&'a ManagedSkill>,
    kind: AgentKind,
    enabled: bool,
    incoming: Option<&'a Path>,
}

impl NativeChanges {
    fn prepare(&mut self, ctx: &Context, target: Target<'_>) -> Result<()> {
        let Target {
            skill,
            previous,
            kind,
            enabled,
            incoming,
        } = target;
        let master = content(ctx, &skill.folder);
        let path = native(ctx, kind, &skill.folder);
        let location = location(&path, &master)?;
        if location == Location::Local {
            return if enabled {
                Err(Error::Conflict(format!(
                    "{} already has a local {} skill",
                    super::adapter(kind).spec().name,
                    skill.folder
                )))
            } else {
                Ok(())
            };
        }
        let old_digest = previous.map_or(&skill.digest, |old| &old.digest);
        let desired = match (enabled, skill.mode) {
            (false, _) => Location::Missing,
            (true, SkillMode::Link) => Location::Link,
            (true, SkillMode::Copy) => Location::Copy,
        };
        if location == desired && (location != Location::Copy || old_digest == &skill.digest) {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| Error::invalid("Invalid agent skills folder"))?;
        fsx::ensure_dir(parent)?;
        let stage = tempfile::Builder::new()
            .prefix(".cabin-skill-")
            .tempdir_in(parent)?;
        let expected = match location {
            Location::Link => Some(NativeCheck::Link(master.clone())),
            Location::Copy => Some(NativeCheck::Copy {
                master: master.clone(),
                digest: old_digest.clone(),
            }),
            _ => None,
        };
        if let Some(expected) = expected {
            let mut movement = prepare_move(path.clone(), stage.path().join("previous"));
            movement.expected = Some(expected);
            self.moves.push(movement);
        }
        let next = stage.path().join("next");
        match desired {
            Location::Link => symlink(&master, &next)?,
            Location::Copy => {
                fs::create_dir(&next)?;
                if digest(incoming.unwrap_or(&master), Some(&next))? != skill.digest {
                    return Err(Error::Conflict(format!(
                        "The master for {} has changed",
                        skill.folder
                    )));
                }
                fsx::atomic_write(
                    &next.join(COPY_MARKER),
                    master.as_os_str().as_encoded_bytes(),
                    Some(0o600),
                )?;
            }
            _ => {}
        }
        if enabled {
            self.moves.push(prepare_move(next, path.clone()));
        }
        self.checks.push((path, master, desired));
        self.staged.push(stage);
        Ok(())
    }
}

fn transfer(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fsx::ensure_dir(parent)?;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::ffi::CString;
        let source = CString::new(from.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::invalid("Invalid skill path"))?;
        let target = CString::new(to.as_os_str().as_encoded_bytes())
            .map_err(|_| Error::invalid("Invalid skill path"))?;
        // Both C strings stay alive throughout the system call. The flags
        // require the destination to be absent at the moment of the rename.
        #[cfg(target_os = "linux")]
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let result = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if result == 0 {
            return Ok(());
        }
        Err(Error::path("move", from, std::io::Error::last_os_error()))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        absent(to)?;
        fs::rename(from, to).map_err(|error| Error::path("move", from, error))
    }
}

fn prepare_move(from: PathBuf, to: PathBuf) -> Move {
    Move {
        from,
        to,
        applied: false,
        expected: None,
    }
}

fn backup_path(ctx: &Context) -> PathBuf {
    backups(ctx).join(format!("{}-{}", now_millis(), new_id("skill")))
}

fn backup_entry(ctx: &Context, id: &str) -> Result<PathBuf> {
    if id.len() > 100
        || !id
            .split_once('-')
            .is_some_and(|(at, _)| at.parse::<u64>().is_ok())
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        return Err(Error::invalid("Invalid skill backup ID"));
    }
    let path = backups(ctx).join(id);
    if !fs::symlink_metadata(&path)?.is_dir() {
        return Err(Error::NotFound("Skill backup".into()));
    }
    Ok(path)
}

fn stage_binding_file(
    transaction: &Path,
    directory: &Path,
    skill: &ManagedSkill,
    moves: &mut Vec<Move>,
) -> Result<()> {
    let stage = transaction.join(&skill.folder);
    record(&stage, skill)?;
    moves.push(prepare_move(
        directory.join("skill.json"),
        stage.join("original.json"),
    ));
    moves.push(prepare_move(
        stage.join("skill.json"),
        directory.join("skill.json"),
    ));
    Ok(())
}

impl Step for WriteSkills {
    fn name(&self) -> String {
        "Update skill library and agent folders".into()
    }

    fn prepare(&mut self, _: &StepCtx<'_>) -> Result<()> {
        let ctx = &self.ctx;
        let transaction = staging(ctx)?;
        match &self.request {
            Request::Install(prepared) => {
                let skill = &prepared.skill;
                let path = installed(ctx).join(&skill.folder);
                if let Some(old) = &prepared.previous {
                    if metadata(&path)? != *old {
                        return Err(Error::Conflict(
                            "The skill changed during the download. Try again.".into(),
                        ));
                    }
                    verify_master(&path, old)?;
                    if skill.digest == old.digest {
                        stage_binding_file(transaction.path(), &path, skill, &mut self.moves)?;
                        self.transaction = Some(transaction);
                        return Ok(());
                    }
                    self.moves
                        .push(prepare_move(path.clone(), backup_path(ctx)));
                } else {
                    absent(&path)?;
                }
                for kind in &skill.targets {
                    self.native_changes.prepare(
                        ctx,
                        Target {
                            skill,
                            previous: prepared.previous.as_ref(),
                            kind: *kind,
                            enabled: true,
                            incoming: Some(&prepared.directory.path().join("content")),
                        },
                    )?;
                }
                self.moves
                    .push(prepare_move(prepared.directory.path().into(), path));
            }
            Request::Change(SkillChange::Bind {
                folders,
                targets,
                enabled,
            }) => {
                for folder in folders.iter().collect::<BTreeSet<_>>() {
                    valid_skill_name(folder)?;
                    let path = installed(ctx).join(folder);
                    let mut skill = metadata(&path)?;
                    for kind in targets.iter().copied().collect::<BTreeSet<_>>() {
                        self.native_changes.prepare(
                            ctx,
                            Target {
                                skill: &skill,
                                previous: None,
                                kind,
                                enabled: *enabled,
                                incoming: None,
                            },
                        )?;
                        if *enabled {
                            skill.targets.insert(kind);
                        } else {
                            skill.targets.remove(&kind);
                        }
                    }
                    stage_binding_file(transaction.path(), &path, &skill, &mut self.moves)?;
                }
            }
            Request::Change(SkillChange::Remove { folders }) => {
                for folder in folders.iter().collect::<BTreeSet<_>>() {
                    valid_skill_name(folder)?;
                    let path = installed(ctx).join(folder);
                    let skill = metadata(&path)?;
                    verify_master(&path, &skill)?;
                    for kind in AgentKind::ALL {
                        self.native_changes.prepare(
                            ctx,
                            Target {
                                skill: &skill,
                                previous: None,
                                kind,
                                enabled: false,
                                incoming: None,
                            },
                        )?;
                    }
                    self.moves.push(prepare_move(path, backup_path(ctx)));
                }
            }
            Request::Change(SkillChange::Restore { id }) => {
                let backup = backup_entry(ctx, id)?;
                let skill = metadata(&backup)?;
                manifest_text(&backup.join("content"))?;
                if digest(&backup.join("content"), None)? != skill.digest {
                    return Err(Error::Conflict("The backup contents changed".into()));
                }
                let path = installed(ctx).join(&skill.folder);
                let previous = if fs::symlink_metadata(&path).is_ok() {
                    let old = metadata(&path)?;
                    verify_master(&path, &old)?;
                    self.moves
                        .push(prepare_move(path.clone(), backup_path(ctx)));
                    Some(old)
                } else {
                    None
                };
                for kind in AgentKind::ALL {
                    self.native_changes.prepare(
                        ctx,
                        Target {
                            skill: &skill,
                            previous: previous.as_ref(),
                            kind,
                            enabled: skill.targets.contains(&kind),
                            incoming: Some(&backup.join("content")),
                        },
                    )?;
                }
                self.moves.push(prepare_move(backup, path));
            }
            Request::Change(SkillChange::DeleteBackup { id }) => {
                self.moves.push(prepare_move(
                    backup_entry(ctx, id)?,
                    transaction.path().join("deleted"),
                ));
            }
            Request::Change(SkillChange::Sync) => {
                for path in directories(&installed(ctx))? {
                    let skill = metadata(&path)?;
                    for kind in AgentKind::ALL {
                        self.native_changes.prepare(
                            ctx,
                            Target {
                                skill: &skill,
                                previous: None,
                                kind,
                                enabled: skill.targets.contains(&kind),
                                incoming: None,
                            },
                        )?;
                    }
                }
            }
        }
        self.transaction = Some(transaction);
        Ok(())
    }

    fn apply(&mut self, step: &StepCtx<'_>) -> Result<()> {
        for movement in self.moves.iter_mut().chain(&mut self.native_changes.moves) {
            if step.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let intact = match &movement.expected {
                Some(NativeCheck::Link(master)) => {
                    location(&movement.from, master)? == Location::Link
                }
                Some(NativeCheck::Copy { master, digest }) => {
                    location(&movement.from, master)? == Location::Copy
                        && digest_copy(&movement.from)? == *digest
                }
                None => true,
            };
            if !intact {
                return Err(Error::Conflict(format!(
                    "The skill at {} has local edits",
                    movement.from.display()
                )));
            }
            transfer(&movement.from, &movement.to)?;
            movement.applied = true;
            step.log(format!("Moved skill data to {}", movement.to.display()));
        }
        Ok(())
    }

    fn verify(&mut self, _: &StepCtx<'_>) -> Result<()> {
        for movement in self.moves.iter().chain(&self.native_changes.moves) {
            fs::symlink_metadata(&movement.to)?;
        }
        for (path, master, expected) in &self.native_changes.checks {
            if location(path, master)? != *expected
                || *expected != Location::Missing && !path.join("SKILL.md").is_file()
            {
                return Err(Error::Conflict(format!(
                    "Could not verify {}",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    fn rollback(&mut self, _: &StepCtx<'_>) -> Result<()> {
        let mut failure = None;
        for movement in self
            .moves
            .iter()
            .chain(&self.native_changes.moves)
            .rev()
            .filter(|movement| movement.applied)
        {
            if let Err(error) = transfer(&movement.to, &movement.from) {
                failure = Some(error);
            }
        }
        if let Some(error) = failure {
            // Keep any previous version still in the staging directory for recovery.
            if let Some(directory) = self.transaction.take() {
                let _ = directory.keep();
            }
            for directory in self.native_changes.staged.drain(..) {
                let _ = directory.keep();
            }
            return Err(error);
        }
        Ok(())
    }
}
