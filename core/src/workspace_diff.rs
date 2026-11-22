//! The guest's own record of what a turn changed in the workspace, independent
//! of what the agent reported: shell commands, formatters and subagents write
//! files the provider never mentions. The workspace is snapshotted as a git
//! tree before and after the turn (through a throwaway index, so the user's
//! index, HEAD and worktree are never touched) and the two trees are diffed.
//! Requires a git repository; anything else reports "not available".

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::{ChangeKind, ChangeOrigin, FileChangeView};

const MAX_FILES: usize = 200;
const MAX_PATCH_BYTES: usize = 200_000;

fn git(root: &Path, index: Option<&Path>, args: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command.output().ok()?;
    output.status.success().then_some(output.stdout)
}

/// A tree id for the workspace's current contents (tracked, untracked, minus ignored).
pub fn snapshot(root: &Path) -> Option<String> {
    git(root, None, &["rev-parse", "--is-inside-work-tree"])?;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let index: PathBuf = std::env::temp_dir().join(format!(
        "cabin-index-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    // Start from HEAD's tree so unchanged tracked files are cheap, then add everything.
    let _ = git(root, Some(&index), &["read-tree", "HEAD"]);
    let tree = git(root, Some(&index), &["add", "-A", "--", "."])
        .and_then(|_| git(root, Some(&index), &["write-tree"]));
    let _ = std::fs::remove_file(&index);
    let tree = String::from_utf8(tree?).ok()?.trim().to_string();
    (tree.len() >= 40).then_some(tree)
}

fn parse_numstat(text: &str) -> Vec<(String, Option<u32>, Option<u32>)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let added = parts.next()?;
            let deleted = parts.next()?;
            let path = parts.next()?.to_string();
            // Binary files report "-".
            Some((path, added.parse().ok(), deleted.parse().ok()))
        })
        .collect()
}

/// The structured changes between two snapshots, workspace-relative, origin `Workspace`.
pub fn diff(root: &Path, base: &str, head: &str) -> Vec<FileChangeView> {
    if base == head {
        return vec![];
    }
    let Some(stat) = git(
        root,
        None,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--numstat",
            "--relative",
            "-z",
            "--no-renames",
            base,
            head,
        ],
    ) else {
        return vec![];
    };
    // -z separates records with NUL and fields with TAB.
    let stat = String::from_utf8_lossy(&stat).replace('\0', "\n");
    let mut out = Vec::new();
    for (path, additions, deletions) in parse_numstat(&stat).into_iter().take(MAX_FILES) {
        // `--relative` keeps paths inside the workspace; refuse anything else.
        if path.starts_with('/')
            || Path::new(&path)
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            continue;
        }
        let kind = match (
            git(
                root,
                None,
                &[
                    "cat-file",
                    "-e",
                    &format!("{base}:{}", rel_to_repo(root, &path)),
                ],
            )
            .is_some(),
            git(
                root,
                None,
                &[
                    "cat-file",
                    "-e",
                    &format!("{head}:{}", rel_to_repo(root, &path)),
                ],
            )
            .is_some(),
        ) {
            (false, true) => ChangeKind::Added,
            (true, false) => ChangeKind::Deleted,
            (true, true) => ChangeKind::Modified,
            _ => ChangeKind::Unknown,
        };
        let patch = git(
            root,
            None,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--relative",
                "--no-renames",
                base,
                head,
                "--",
                &path,
            ],
        )
        .map(|bytes| {
            String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PATCH_BYTES)]).to_string()
        })
        .filter(|p| !p.is_empty());
        out.push(FileChangeView {
            path,
            origin: ChangeOrigin::Workspace,
            kind,
            additions,
            deletions,
            patch,
        });
    }
    out
}

/// `git cat-file <tree>:<path>` wants a repo-root-relative path.
fn rel_to_repo(root: &Path, relative: &str) -> String {
    let prefix = git(root, None, &["rev-parse", "--show-prefix"])
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    format!("{prefix}{relative}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(dir.path())
                    .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        run(&["init", "-q"]);
        std::fs::write(dir.path().join("keep.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.log\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn it_finds_added_modified_and_deleted_files_the_agent_never_reported() {
        let dir = repo();
        let before = snapshot(dir.path()).unwrap();
        std::fs::write(dir.path().join("keep.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "hello\n").unwrap();
        std::fs::write(dir.path().join("ignored.log"), "noise\n").unwrap();
        std::fs::remove_file(dir.path().join(".gitignore")).unwrap();
        let after = snapshot(dir.path()).unwrap();
        let changes = diff(dir.path(), &before, &after);
        let by = |p: &str| {
            changes
                .iter()
                .find(|c| c.path == p)
                .unwrap_or_else(|| panic!("{p} in {changes:?}"))
        };
        assert_eq!(by("new.txt").kind, ChangeKind::Added);
        assert_eq!(by("keep.txt").kind, ChangeKind::Modified);
        assert_eq!(by(".gitignore").kind, ChangeKind::Deleted);
        assert!(by("keep.txt").patch.as_deref().unwrap().contains("+three"));
        assert!(changes.iter().all(|c| c.origin == ChangeOrigin::Workspace));
        // With .gitignore gone, ignored.log is now an ordinary new file; a still-ignored one would not be.
        assert_eq!(by("ignored.log").kind, ChangeKind::Added);
    }

    #[test]
    fn ignored_files_and_unchanged_trees_produce_nothing_and_the_real_index_is_untouched() {
        let dir = repo();
        let before = snapshot(dir.path()).unwrap();
        std::fs::write(dir.path().join("ignored.log"), "noise\n").unwrap();
        let after = snapshot(dir.path()).unwrap();
        assert_eq!(before, after);
        assert!(diff(dir.path(), &before, &after).is_empty());
        std::fs::write(dir.path().join("untracked.txt"), "x\n").unwrap();
        snapshot(dir.path()).unwrap();
        let status =
            String::from_utf8(git(dir.path(), None, &["status", "--porcelain"]).unwrap()).unwrap();
        assert_eq!(
            status.trim(),
            "?? untracked.txt",
            "the user's index is untouched: untracked stays untracked"
        );
    }

    #[test]
    fn a_folder_that_is_not_a_repository_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(snapshot(dir.path()), None);
    }

    #[test]
    fn a_workspace_inside_a_larger_repository_only_reports_its_own_files() {
        let dir = repo();
        std::fs::create_dir_all(dir.path().join("pkg")).unwrap();
        let root = dir.path().join("pkg");
        let before = snapshot(&root).unwrap();
        std::fs::write(root.join("inner.txt"), "i\n").unwrap();
        std::fs::write(dir.path().join("outer.txt"), "o\n").unwrap();
        let after = snapshot(&root).unwrap();
        let changes = diff(&root, &before, &after);
        assert_eq!(
            changes.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["inner.txt"]
        );
    }
}
