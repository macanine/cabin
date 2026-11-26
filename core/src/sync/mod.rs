//! Project sync between a folder on the Mac and its copy inside the VM.
//!
//! The copy lives on the VM's own disk, so agents, git and builds run at
//! native speed (a 9p mount pays a VM exit per file operation). Both sides
//! run the same code from here: `scan` lists a tree under the same ignore
//! rules, `plan::reconcile` decides per path what the chosen mode does, and
//! `archive` moves file contents as one tar stream (pack on one side, atomic
//! apply on the other). The Mac drives a cycle; `cabind sync …` is the
//! guest's half (`run_command`).

pub mod archive;
pub mod base;
pub mod plan;
pub mod scan;
pub mod watch;

use std::io::{BufRead, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(test)]
use ts_rs::TS;

pub use plan::{Action, BaseRecord, reconcile};
pub use scan::{Entry, EntryKind, scan};

/// Name of the per-project ignore file (gitignore syntax), on either side.
pub const IGNORE_FILE: &str = ".cabinignore";

/// Prefix for Cabin's write-access probes inside a project folder.
pub const ACCESS_PROBE_PREFIX: &str = ".cabin-sync-probe-";
const STAGED_FILE_PREFIX: &str = ".vasync-";
const ALWAYS_IGNORED: &[&str] = &[".DS_Store", "._*", "*.vasync-tmp"];

/// Keep the watcher and the scanner in agreement about internal files.
fn ignored_patterns() -> impl Iterator<Item = String> {
    ALWAYS_IGNORED
        .iter()
        .map(|pattern| (*pattern).to_owned())
        .chain([
            format!("{ACCESS_PROBE_PREFIX}*.tmp"),
            format!("{STAGED_FILE_PREFIX}*.tmp"),
        ])
}

/// What flows where.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum SyncMode {
    /// The machine's copy always equals the Mac's; changes made inside the
    /// machine to synced files are replaced.
    Mirror,
    /// Mac changes flow into the machine; a file also edited inside the
    /// machine is kept and reported as a conflict. Files that exist only in
    /// the machine are left alone.
    #[default]
    Safe,
    /// Changes flow both ways; a file edited on both sides is a conflict.
    TwoWay,
}

/// How the project's `.git` folder takes part. Git rewrites its index, refs
/// and lock files constantly; merging them file by file from two writers
/// can mix one side's index with the other's refs and break the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum GitHandling {
    /// Copied into the machine once (the first sync), then each side keeps
    /// its own history while the working files sync. Both copies stay real
    /// repositories; nothing can corrupt either.
    #[default]
    CopyOnce,
    /// Synced like any other folder. Safe with Mirror (one writer); risky in
    /// Two-way if git runs on both sides at once.
    Sync,
    /// Never synced; the machine's copy is not a git repository unless one
    /// is created there.
    Skip,
}

/// Which files take part. Ignored paths are never read, written or deleted
/// on either side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct SyncRules {
    /// Honour `.gitignore` files (and `.git/info/exclude`).
    pub gitignore: bool,
    /// Extra gitignore-style patterns, e.g. `*.log` or `build/`.
    pub ignore: Vec<String>,
    /// Relative paths the user unticked; each excludes itself and below.
    pub excluded: Vec<String>,
    /// How `.git` takes part (only `Sync` lets the regular scan see it).
    #[serde(default)]
    pub git: GitHandling,
}

impl Default for SyncRules {
    fn default() -> Self {
        Self {
            gitignore: true,
            ignore: Vec::new(),
            excluded: Vec::new(),
            git: GitHandling::CopyOnce,
        }
    }
}

impl SyncRules {
    /// The rules for one `.git` copy: everything inside it, nothing else.
    pub fn git_only() -> Self {
        Self {
            gitignore: false,
            ignore: Vec::new(),
            excluded: Vec::new(),
            git: GitHandling::Sync,
        }
    }
}

/// Request for the guest's `sync scan`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanRequest {
    pub rules: SyncRules,
    /// Limit the walk to these relative folders (and their ancestors);
    /// `None` walks everything.
    pub scope: Option<Vec<String>>,
}

/// Request for the guest's `sync pack`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackRequest {
    pub paths: Vec<String>,
}

/// `~/x` and `$HOME/x` → absolute, for paths stored with the user's home.
pub fn expand_home(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if path == "~" {
        return home.unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path
        .strip_prefix("~/")
        .or_else(|| path.strip_prefix("$HOME/"))
        && let Some(home) = home
    {
        return home.join(rest);
    }
    PathBuf::from(path)
}

/// A relative path from the wire is safe: no `..`, not absolute, not empty.
pub fn safe_relative(path: &str) -> Option<&Path> {
    let rel = Path::new(path);
    (!path.is_empty()
        && rel
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_))))
    .then_some(rel)
}

/// The guest's half: `cabind sync <scan|pack|apply|watch> <root>`, with
/// a JSON request (scan, pack, watch) or a header line plus tar (apply) on
/// stdin.
pub fn run_command(args: &[String]) -> Result<(), String> {
    let (Some(command), Some(root)) = (args.first(), args.get(1)) else {
        return Err("usage: cabind sync <scan|pack|apply|watch> <root>".into());
    };
    let root = expand_home(root);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    match command.as_str() {
        "scan" => {
            let request: ScanRequest = read_json(&mut stdin.lock())?;
            let entries = match root.metadata() {
                Ok(_) => scan(&root, &request.rules, request.scope.as_deref())
                    .map_err(|e| e.to_string())?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error.to_string()),
            };
            let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());
            scan::write_binary(&entries, &mut out).map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
        }
        "pack" => {
            let request: PackRequest = read_json(&mut stdin.lock())?;
            let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());
            archive::pack(&root, &request.paths, &mut out).map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
        }
        "apply" => {
            let report = archive::apply_stream(&root, stdin.lock()).map_err(|e| e.to_string())?;
            write_json(&mut stdout.lock(), &report)
        }
        "watch" => {
            let rules: SyncRules = read_json(&mut stdin.lock())?;
            std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            watch::print_changes(&root, &rules)
        }
        other => Err(format!("unknown sync command {other:?}")),
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(input: &mut impl BufRead) -> Result<T, String> {
    let mut line = String::new();
    input.read_line(&mut line).map_err(|e| e.to_string())?;
    serde_json::from_str(line.trim()).map_err(|e| format!("bad sync request: {e}"))
}

fn write_json<T: Serialize>(out: &mut impl Write, value: &T) -> Result<(), String> {
    serde_json::to_writer(&mut *out, value).map_err(|e| e.to_string())?;
    out.write_all(b"\n")
        .and_then(|()| out.flush())
        .map_err(|e| e.to_string())
}

/// The top level of `root` as the New project dialog shows it: what would
/// sync under each item, and why an item is left out.
pub fn preview(root: &Path, rules: &SyncRules) -> std::io::Result<crate::model::SyncPreview> {
    use crate::model::{SyncPreview, SyncPreviewItem};
    let entries = scan(root, rules, None)?;
    let mut items = Vec::new();
    let mut names: Vec<(String, bool)> = std::fs::read_dir(root)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            let dir = entry.file_type().ok()?.is_dir();
            Some((name, dir))
        })
        .filter(|(name, _)| name != ".DS_Store")
        .collect();
    names.sort_by(|(a, a_dir), (b, b_dir)| {
        b_dir
            .cmp(a_dir)
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    let (mut files, mut bytes) = (0, 0);
    for (name, dir) in names {
        let prefix = format!("{name}/");
        let mut item = SyncPreviewItem {
            name: name.clone(),
            dir,
            files: 0,
            bytes: 0,
            included: false,
            reason: None,
        };
        for entry in entries
            .iter()
            .filter(|e| e.path == name || e.path.starts_with(&prefix))
        {
            item.included = true;
            if entry.kind != EntryKind::Dir {
                item.files += 1;
                item.bytes += entry.size;
            }
        }
        if !item.included {
            item.reason = Some(
                if rules
                    .excluded
                    .iter()
                    .any(|path| path.trim_matches('/') == name)
                {
                    "unticked"
                } else if name == ".git" {
                    "git"
                } else {
                    "ignored"
                }
                .to_string(),
            );
        }
        files += item.files;
        bytes += item.bytes;
        items.push(item);
    }
    Ok(SyncPreview {
        host_path: root.display().to_string(),
        items,
        files,
        bytes,
        git: root.join(".git").is_dir(),
        gitignore: root.join(".gitignore").is_file(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_counts_what_syncs_and_says_why_the_rest_does_not() {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in [
            ("src/a.rs", "12345"),
            ("node_modules/x.js", "x"),
            ("docs/r.md", "x"),
            (".gitignore", "node_modules/\n"),
            (".git/HEAD", "x"),
        ] {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        let rules = SyncRules {
            excluded: vec!["docs".into()],
            ..SyncRules::default()
        };
        let preview = preview(dir.path(), &rules).unwrap();
        let item = |name: &str| {
            preview
                .items
                .iter()
                .find(|i| i.name == name)
                .unwrap()
                .clone()
        };
        assert_eq!(
            (item("src").files, item("src").bytes, item("src").included),
            (1, 5, true)
        );
        assert_eq!(item("node_modules").reason.as_deref(), Some("ignored"));
        assert_eq!(item("docs").reason.as_deref(), Some("unticked"));
        assert_eq!(item(".git").reason.as_deref(), Some("git"));
        assert!(preview.git && preview.gitignore);
        assert_eq!(preview.files, 2);
        // Folders first.
        assert!(preview.items[0].dir);
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
        assert!(safe_relative("a/../b").is_none() && safe_relative("a/b").is_some());
    }
}
