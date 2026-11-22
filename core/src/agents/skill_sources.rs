//! Repository discovery and bounded, data-only skill archive extraction.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use super::extensions::valid_skill_name;
use crate::context::Context;
use crate::error::{Error, Result};
use crate::util::LockExt;

pub(super) const MAX_ZIP: usize = 12 * 1024 * 1024;
pub(super) const COPY_MARKER: &str = ".cabin-skill-copy";
const MAX_SKILL: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 4096;
const MAX_ARCHIVE: u64 = 128 * 1024 * 1024;
const MAX_EXPANDED: u64 = 512 * 1024 * 1024;
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_REPOSITORY_FILES: usize = 25_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillRepository {
    pub repository: String,
    pub branch: String,
    pub directory: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillOrigin {
    pub repository: SkillRepository,
    pub path: String,
    pub revision: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredSkill {
    pub name: String,
    pub folder: String,
    pub description: String,
    pub origin: SkillOrigin,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct RegistrySkill {
    pub id: String,
    pub skill_id: String,
    pub name: String,
    pub source: String,
    pub installs: u32,
}

#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SkillRegistry {
    pub skills: Vec<RegistrySkill>,
    pub count: usize,
}

fn safe_path(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().count() <= 32 &&
        path.components().all(|component| matches!(component, Component::Normal(name) if !name.as_encoded_bytes().contains(&b'\\')))
}

pub(super) fn repository_id(value: &str) -> Result<&str> {
    let value = value.trim().trim_end_matches('/');
    let id = value.strip_prefix("https://github.com/").unwrap_or(value);
    let id = id.strip_suffix(".git").unwrap_or(id);
    let Some((owner, name)) = id.split_once('/') else {
        return Err(Error::invalid("Enter a GitHub repository as owner/name"));
    };
    let valid = |segment: &str| {
        !segment.is_empty()
            && segment.len() <= 100
            && segment != "."
            && segment != ".."
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    if !valid(owner) || !valid(name) {
        return Err(Error::invalid("Enter a GitHub repository as owner/name"));
    }
    Ok(id)
}

impl SkillRepository {
    pub(super) fn normalized(mut self) -> Result<Self> {
        self.repository = repository_id(&self.repository)?.to_ascii_lowercase();
        self.branch = self.branch.trim().to_owned();
        if self.branch.is_empty() {
            self.branch = "HEAD".into();
        }
        if self.branch.len() > 128
            || !safe_path(Path::new(&self.branch))
            || !self
                .branch
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_/.".contains(&byte))
        {
            return Err(Error::invalid("Enter a Git branch or tag"));
        }
        self.directory = self.directory.trim().trim_end_matches('/').to_owned();
        if self.directory.len() > 512
            || !self.directory.is_empty() && !safe_path(Path::new(&self.directory))
        {
            return Err(Error::invalid("Enter a relative skills directory"));
        }
        Ok(self)
    }
}

pub(super) fn default_repositories() -> Vec<SkillRepository> {
    ["anthropics/skills", "composiohq/awesome-claude-skills"]
        .into_iter()
        .map(|repository| SkillRepository {
            repository: repository.into(),
            branch: "HEAD".into(),
            directory: String::new(),
        })
        .collect()
}

fn download(
    ctx: &Context,
    url: &str,
    label: &str,
    limit: u64,
    output: &mut impl io::Write,
) -> Result<()> {
    let response = crate::fetch::http(ctx, Duration::from_secs(60))?
        .get(url)
        .timeout(Duration::from_secs(180))
        .call()
        .map_err(|error| crate::fetch::http_error(label, error))?;
    let size = io::copy(&mut response.into_reader().take(limit + 1), output)
        .map_err(|error| Error::io(label, error))?;
    if size > limit {
        return Err(Error::invalid(format!(
            "{label} exceeded the download limit"
        )));
    }
    Ok(())
}

fn segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

struct RepositoryArchive {
    file: tempfile::NamedTempFile,
    fetched: Instant,
    indexes: Mutex<BTreeMap<String, Vec<DiscoveredSkill>>>,
}

type RepositoryCache = BTreeMap<(PathBuf, String, String), Arc<RepositoryArchive>>;

fn repository_archive(ctx: &Context, repo: &SkillRepository) -> Result<Arc<RepositoryArchive>> {
    static CACHE: Mutex<RepositoryCache> = Mutex::new(BTreeMap::new());
    let key = (
        ctx.paths.data().to_owned(),
        repo.repository.clone(),
        repo.branch.clone(),
    );
    {
        let mut cache = CACHE.locked();
        cache.retain(|_, entry| entry.fetched.elapsed() < Duration::from_secs(60));
        if let Some(archive) = cache.get(&key) {
            return Ok(archive.clone());
        }
    }
    let url = format!(
        "https://codeload.github.com/{}/tar.gz/{}",
        repo.repository,
        segment(&repo.branch)
    );
    let mut file = tempfile::NamedTempFile::new()?;
    download(
        ctx,
        &url,
        "Could not download the GitHub repository",
        MAX_ARCHIVE,
        &mut file,
    )?;
    let archive = Arc::new(RepositoryArchive {
        file,
        fetched: Instant::now(),
        indexes: Mutex::new(BTreeMap::new()),
    });
    let mut cache = CACHE.locked();
    if cache.len() >= 4
        && let Some(key) = cache
            .iter()
            .min_by_key(|(_, archive)| archive.fetched)
            .map(|(key, _)| key.clone())
    {
        cache.remove(&key);
    }
    cache.insert(key, archive.clone());
    Ok(archive)
}

fn tar_reader(file: File) -> tar::Archive<io::Take<flate2::read::GzDecoder<File>>> {
    tar::Archive::new(flate2::read::GzDecoder::new(file).take(MAX_EXPANDED))
}

fn repo_path(entry: &tar::Entry<'_, impl Read>) -> Result<PathBuf> {
    let path = entry.path()?;
    if !safe_path(&path) {
        return Err(Error::invalid("Unsafe path in repository archive"));
    }
    Ok(path.components().skip(1).collect())
}

fn check_expansion(remaining: u64) -> Result<()> {
    if remaining == 0 {
        return Err(Error::invalid(
            "The repository exceeds the expanded archive limit",
        ));
    }
    Ok(())
}

pub fn discover(ctx: &Context, repository: SkillRepository) -> Result<Vec<DiscoveredSkill>> {
    let repository = repository.normalized()?;
    let archive = repository_archive(ctx, &repository)?;
    index_repository(&archive, repository)
}

fn index_repository(
    archive: &RepositoryArchive,
    repository: SkillRepository,
) -> Result<Vec<DiscoveredSkill>> {
    let mut indexes = archive.indexes.locked();
    if let Some(skills) = indexes.get(&repository.directory) {
        return Ok(skills.clone());
    }
    let mut tar = tar_reader(archive.file.reopen()?);
    let mut files = BTreeMap::new();
    let mut manifests = Vec::new();
    let mut buffer = [0; 32 * 1024];
    for (index, entry) in tar.entries()?.enumerate() {
        if index >= MAX_REPOSITORY_FILES {
            return Err(Error::invalid("The skill repository has too many files"));
        }
        let mut entry = entry?;
        let path = repo_path(&entry)?;
        let header = entry.header();
        if !path.starts_with(&repository.directory) || !header.entry_type().is_file() {
            continue;
        }
        let mut digest = Sha256::new();
        digest.update((header.mode()? & 0o100).to_le_bytes());
        let is_manifest =
            path.file_name().is_some_and(|part| part == "SKILL.md") && entry.size() <= MAX_MANIFEST;
        if is_manifest {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            digest.update(&bytes);
            if let Ok(text) = std::str::from_utf8(&bytes) {
                let parent = path.parent().unwrap_or(Path::new(""));
                let fallback = repository
                    .repository
                    .rsplit_once('/')
                    .map_or("", |(_, name)| name);
                let folder = parent
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or(fallback);
                if valid_skill_name(folder).is_ok() {
                    let (name, description) = super::extensions::skill_front_matter(text);
                    manifests.push((
                        parent.to_owned(),
                        folder.to_owned(),
                        name,
                        description.unwrap_or_default(),
                    ));
                }
            }
        } else {
            loop {
                let size = entry.read(&mut buffer)?;
                if size == 0 {
                    break;
                }
                digest.update(&buffer[..size]);
            }
        }
        if files.insert(path, digest.finalize()).is_some() {
            return Err(Error::invalid("The repository archive has duplicate files"));
        }
    }
    check_expansion(tar.into_inner().limit())?;
    let mut skills = Vec::new();
    for (path, folder, name, description) in manifests {
        let mut hash = Sha256::new();
        for (item, digest) in files
            .range(path.clone()..)
            .take_while(|(item, _)| item.starts_with(&path))
        {
            let item = item.as_os_str().as_encoded_bytes();
            hash.update((item.len() as u64).to_le_bytes());
            hash.update(item);
            hash.update(digest);
        }
        skills.push(DiscoveredSkill {
            name: name.unwrap_or_else(|| folder.clone()),
            folder,
            description,
            origin: SkillOrigin {
                repository: repository.clone(),
                path: path.to_string_lossy().into_owned(),
                revision: format!("{:x}", hash.finalize()),
            },
        });
    }
    skills.sort_by(|a, b| a.origin.path.cmp(&b.origin.path));
    indexes.insert(repository.directory, skills.clone());
    Ok(skills)
}

pub fn registry(ctx: &Context, query: &str, offset: usize) -> Result<SkillRegistry> {
    if query.trim().is_empty() || query.len() > 128 || offset > 10_000 {
        return Err(Error::invalid("Enter a search query up to 128 bytes"));
    }
    let url = format!(
        "https://skills.sh/api/search?q={}&limit=30&offset={offset}",
        segment(query.trim())
    );
    let mut bytes = Vec::new();
    download(
        ctx,
        &url,
        "Could not search skills.sh",
        1024 * 1024,
        &mut bytes,
    )?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub(super) fn registry_origin(
    ctx: &Context,
    repository: String,
    skill_id: &str,
) -> Result<SkillOrigin> {
    valid_skill_name(skill_id)?;
    let repository = SkillRepository {
        repository,
        branch: "HEAD".into(),
        directory: String::new(),
    };
    let matches: Vec<_> = discover(ctx, repository)?
        .into_iter()
        .filter(|skill| skill.folder == skill_id)
        .collect();
    match matches.as_slice() {
        [skill] => Ok(skill.origin.clone()),
        [] => Err(Error::NotFound(format!(
            "Skill {skill_id} in the GitHub repository"
        ))),
        _ => Err(Error::Conflict(format!(
            "Several paths match {skill_id}. Choose a path under Repositories."
        ))),
    }
}

struct Extract<'a> {
    root: &'a Path,
    entries: BTreeSet<PathBuf>,
    total: u64,
}

impl<'a> Extract<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            entries: BTreeSet::new(),
            total: 0,
        }
    }

    fn add(&mut self, path: &Path, mode: u32, size: u64, reader: impl Read) -> Result<()> {
        if !safe_path(path)
            || !self.entries.insert(path.into())
            || self.entries.len() > MAX_FILES
            || size > MAX_SKILL - self.total
        {
            return Err(Error::invalid(
                "The skill archive has unsafe paths, duplicate entries or too much data",
            ));
        }
        let target = self.root.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600 | (mode & 0o100))
            .open(&target)?;
        let written = io::copy(&mut reader.take(size + 1), &mut output)?;
        if written != size {
            return Err(Error::invalid("The skill archive contains a size mismatch"));
        }
        self.total += written;
        Ok(())
    }
}

pub(super) fn from_repository(
    ctx: &Context,
    origin: SkillOrigin,
    target: &Path,
) -> Result<(String, SkillOrigin)> {
    let repository = origin.repository.normalized()?;
    let archive = repository_archive(ctx, &repository)?;
    // Index and extract the same downloaded snapshot so the recorded revision
    // cannot refer to different content if a branch moves during installation.
    let skill = index_repository(&archive, repository)?
        .into_iter()
        .find(|skill| skill.origin.path == origin.path)
        .ok_or_else(|| Error::NotFound("Skill path in the GitHub repository".into()))?;
    extract_tar(
        archive.file.reopen()?,
        Path::new(&skill.origin.path),
        target,
    )?;
    Ok((skill.folder, skill.origin))
}

fn extract_tar(file: File, subtree: &Path, target: &Path) -> Result<()> {
    let mut archive = tar_reader(file);
    let mut extract = Extract::new(target);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let rest = repo_path(&entry)?;
        let Ok(path) = rest.strip_prefix(subtree) else {
            continue;
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(Error::invalid(
                "Skill directories must contain regular files and folders",
            ));
        }
        extract.add(path, entry.header().mode()?, entry.size(), &mut entry)?;
    }
    check_expansion(archive.into_inner().limit())?;
    if !target.join("SKILL.md").is_file() {
        return Err(Error::NotFound("SKILL.md in the repository archive".into()));
    }
    Ok(())
}

pub(super) fn from_zip(bytes: &[u8], target: &Path) -> Result<String> {
    if bytes.len() > MAX_ZIP {
        return Err(Error::invalid("Choose a skill ZIP up to 12 MiB"));
    }
    let bad_zip = |_: zip::result::ZipError| Error::invalid("Could not read the skill ZIP");
    let mut archive = zip::ZipArchive::new(io::Cursor::new(bytes)).map_err(bad_zip)?;
    if archive.len() > MAX_FILES {
        return Err(Error::invalid("The skill ZIP has too many files"));
    }
    let mut manifests = Vec::new();
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(bad_zip)?;
        let path = entry
            .enclosed_name()
            .filter(|path| safe_path(path))
            .ok_or_else(|| Error::invalid("Unsafe path in skill ZIP"))?;
        if entry.is_file() && path.file_name().is_some_and(|name| name == "SKILL.md") {
            manifests.push(path);
        }
    }
    manifests.sort_by_key(|path| path.components().count());
    let manifest = manifests
        .first()
        .ok_or_else(|| Error::NotFound("SKILL.md in the ZIP".into()))?;
    let subtree = manifest.parent().unwrap_or(Path::new(""));
    if manifests.iter().any(|path| !path.starts_with(subtree)) {
        return Err(Error::invalid(
            "Choose a ZIP containing one skill directory",
        ));
    }
    let mut extract = Extract::new(target);
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(bad_zip)?;
        let path = entry.enclosed_name().expect("checked ZIP paths");
        let Ok(path) = path.strip_prefix(subtree) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let mode = entry.unix_mode().unwrap_or(0o100644);
        if !entry.is_file() || !matches!(mode & 0o170000, 0 | 0o100000) {
            return Err(Error::invalid(
                "Skill ZIPs must contain regular files and folders",
            ));
        }
        extract.add(path, mode, entry.size(), &mut entry)?;
    }
    let text = manifest_text(target)?;
    let (name, _) = super::extensions::skill_front_matter(&text);
    let folder = subtree
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| valid_skill_name(name).is_ok())
        .map(str::to_owned)
        .or(name)
        .ok_or_else(|| Error::invalid("Give the skill a valid name in SKILL.md"))?;
    valid_skill_name(&folder)?;
    Ok(folder)
}

pub(super) fn manifest_text(root: &Path) -> Result<String> {
    let path = root.join("SKILL.md");
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() || meta.len() > MAX_MANIFEST {
        return Err(Error::invalid(
            "Use a regular SKILL.md file smaller than 256 KiB",
        ));
    }
    let mut text = String::new();
    File::open(path)?
        .take(MAX_MANIFEST + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_MANIFEST {
        return Err(Error::invalid("SKILL.md exceeds 256 KiB"));
    }
    Ok(text)
}

/// Hash paths, executable bits and file contents, optionally copying that exact tree.
/// Links and special files are rejected before opening them.
pub(super) fn digest(root: &Path, destination: Option<&Path>) -> Result<String> {
    digest_tree(root, destination, false)
}

pub(super) fn digest_copy(root: &Path) -> Result<String> {
    digest_tree(root, None, true)
}

fn digest_tree(root: &Path, destination: Option<&Path>, installed_copy: bool) -> Result<String> {
    let mut pending = vec![PathBuf::new()];
    let mut count = 0;
    let mut total = 0;
    let mut hash = Sha256::new();
    while let Some(relative) = pending.pop() {
        let mut entries: Vec<_> = fs::read_dir(root.join(&relative))?.collect::<io::Result<_>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            count += 1;
            let relative = relative.join(entry.file_name());
            if relative == Path::new(COPY_MARKER) {
                if installed_copy {
                    continue;
                }
                return Err(Error::invalid("The skill uses a reserved file name"));
            }
            if count > MAX_FILES || !safe_path(&relative) {
                return Err(Error::invalid(
                    "Skill directory has too many files or unsafe paths",
                ));
            }
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(relative);
                continue;
            }
            if !kind.is_file() {
                return Err(Error::invalid(
                    "Skill directories must contain regular files and folders",
                ));
            }
            let path = entry.path();
            let mut input = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&path)?;
            let metadata = input.metadata()?;
            if !metadata.is_file() || metadata.len() > MAX_SKILL - total {
                return Err(Error::invalid("Skill directory exceeds 64 MiB"));
            }
            let executable = metadata.permissions().mode() & 0o100;
            let name = relative.as_os_str().as_encoded_bytes();
            hash.update((name.len() as u64).to_le_bytes());
            hash.update(name);
            hash.update(executable.to_le_bytes());
            hash.update(metadata.len().to_le_bytes());
            let mut output = if let Some(destination) = destination {
                let target = destination.join(&relative);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                Some(
                    OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600 | executable)
                        .open(target)?,
                )
            } else {
                None
            };
            let mut read = 0;
            let mut buffer = [0u8; 16 * 1024];
            while read <= metadata.len() {
                let length = (metadata.len() + 1 - read).min(buffer.len() as u64) as usize;
                let n = input.read(&mut buffer[..length])?;
                if n == 0 {
                    break;
                }
                hash.update(&buffer[..n]);
                if let Some(output) = &mut output {
                    io::Write::write_all(output, &buffer[..n])?;
                }
                read += n as u64;
            }
            if read != metadata.len() {
                return Err(Error::Conflict(
                    "A skill file changed while reading it".into(),
                ));
            }
            total += read;
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}
