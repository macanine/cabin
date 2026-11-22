//! The source-control half of the panel, like an editor's SCM view: `git
//! status` (porcelain v2, NUL-separated) grouped into conflicts, staged,
//! changes and untracked, per-file diffs, and stage / unstage / discard /
//! commit.
//!
//! Status runs with `GIT_OPTIONAL_LOCKS=0` so a refresh never takes the
//! index lock an agent's own git command may need; pathspecs are literal.

use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use super::join;
use crate::error::{Error, Result};
use crate::model::{GitCommitResult, GitFileDiff, GitFileStatus, GitStatus};

const MAX_FILES: usize = 2000;
const MAX_PATCH: usize = 400_000;

fn command(root: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["-c", "core.quotepath=false", "-c", "color.ui=false"])
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

/// stdout when git exits with one of `ok` codes.
pub(crate) fn run_input(root: &Path, args: &[&str], input: &[u8], ok: &[i32]) -> Option<String> {
    let mut child = command(root, args).stdin(Stdio::piped()).spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    let output = child.wait_with_output().ok()?;
    ok.contains(&output.status.code()?)
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run(root: &Path, args: &[&str], ok: &[i32]) -> Option<String> {
    let output = command(root, args).output().ok()?;
    ok.contains(&output.status.code()?)
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run a git command the user asked for; its stderr becomes the error.
fn exec(root: &Path, args: &[&str]) -> Result<String> {
    let output = command(root, args)
        .output()
        .map_err(|error| Error::io("run git", error))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = [stderr.trim(), stdout.trim()]
        .into_iter()
        .find(|text| !text.is_empty())
        .unwrap_or("git failed")
        .lines()
        .filter(|line| !line.starts_with("hint:"))
        .collect::<Vec<_>>()
        .join("\n");
    Err(Error::invalid(message))
}

fn is_repo(root: &Path) -> bool {
    run(root, &["rev-parse", "--is-inside-work-tree"], &[0]).is_some_and(|out| out.trim() == "true")
}

fn has_head(root: &Path) -> bool {
    run(root, &["rev-parse", "--verify", "--quiet", "HEAD"], &[0]).is_some()
}

/// Added/deleted line counts keyed by (new) path, from `--numstat -z`.
fn numstat(text: &str) -> HashMap<String, (Option<u32>, Option<u32>)> {
    let mut out = HashMap::new();
    let mut fields = text.split('\0');
    while let Some(record) = fields.next() {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let path = if path.is_empty() {
            // A rename: "a\td\t\0old\0new".
            let _old = fields.next();
            fields.next().unwrap_or_default().to_string()
        } else {
            path.to_string()
        };
        out.insert(path, (added.parse().ok(), deleted.parse().ok()));
    }
    out
}

fn entry(path: &str, orig: Option<&str>, status: char, stats: &HashMap<String, (Option<u32>, Option<u32>)>) -> GitFileStatus {
    let (additions, deletions) = stats.get(path).copied().unwrap_or((None, None));
    GitFileStatus {
        path: path.to_string(),
        orig_path: orig.map(str::to_string),
        status: status.to_string(),
        additions,
        deletions,
    }
}

/// Parse `git status --porcelain=v2 --branch -z` into `status` (counts come
/// from the two numstat maps).
pub(crate) fn parse_status(
    text: &str,
    staged_stats: &HashMap<String, (Option<u32>, Option<u32>)>,
    unstaged_stats: &HashMap<String, (Option<u32>, Option<u32>)>,
) -> GitStatus {
    let mut status = GitStatus { repository: true, ..GitStatus::default() };
    let mut records = text.split('\0');
    let mut files = 0usize;
    while let Some(record) = records.next() {
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" if value != "(initial)" => status.head = Some(value.chars().take(7).collect()),
                "branch.head" if value != "(detached)" => status.branch = Some(value.to_string()),
                "branch.upstream" => status.upstream = Some(value.to_string()),
                "branch.ab" => {
                    for part in value.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            status.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            status.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if record.is_empty() {
            continue;
        }
        files += 1;
        if files > MAX_FILES {
            status.truncated = true;
            break;
        }
        let kind = record.as_bytes()[0];
        match kind {
            b'1' | b'2' => {
                // 1 XY sub mH mI mW hH hI path
                // 2 XY sub mH mI mW hH hI Xscore path \0 origPath
                let fields = if kind == b'1' { 9 } else { 10 };
                let parts: Vec<&str> = record.splitn(fields, ' ').collect();
                let Some(path) = parts.get(fields - 1).copied() else { continue };
                let xy: Vec<char> = parts.get(1).copied().unwrap_or("..").chars().collect();
                let orig = if kind == b'2' { records.next() } else { None };
                let (x, y) = (xy.first().copied().unwrap_or('.'), xy.get(1).copied().unwrap_or('.'));
                if x != '.' {
                    status.staged.push(entry(path, orig, x, staged_stats));
                }
                if y != '.' {
                    status.unstaged.push(entry(path, None, y, unstaged_stats));
                }
            }
            b'u' => {
                let parts: Vec<&str> = record.splitn(11, ' ').collect();
                if let Some(path) = parts.get(10) {
                    status.conflicted.push(entry(path, None, 'U', unstaged_stats));
                }
            }
            b'?' => {
                let path = &record[2..];
                status.untracked.push(entry(path, None, '?', unstaged_stats));
            }
            _ => {}
        }
    }
    status
}

/// The work tree's state, grouped like an editor's Source Control view.
pub fn status(root: &Path) -> Result<GitStatus> {
    if !is_repo(root) {
        return Ok(GitStatus::default());
    }
    let text = run(
        root,
        &["status", "--porcelain=v2", "--branch", "-z", "--untracked-files=all", "--", "."],
        &[0],
    )
    .ok_or_else(|| Error::invalid("git status failed"))?;
    let staged = run(root, &["diff", "--cached", "--numstat", "-z", "-M", "--relative"], &[0]).unwrap_or_default();
    let unstaged = run(root, &["diff", "--numstat", "-z", "--relative"], &[0]).unwrap_or_default();
    // Paths in porcelain v2 are relative to the repository top; keep them
    // relative to the root (a worktree root is the top anyway).
    let prefix = run(root, &["rev-parse", "--show-prefix"], &[0]).unwrap_or_default();
    let prefix = prefix.trim();
    let mut status = parse_status(&text, &numstat(&staged), &numstat(&unstaged));
    if !prefix.is_empty() {
        for list in [&mut status.conflicted, &mut status.staged, &mut status.unstaged, &mut status.untracked] {
            list.retain_mut(|file| match file.path.strip_prefix(prefix) {
                Some(rest) => {
                    file.path = rest.to_string();
                    true
                }
                None => false,
            });
        }
    }
    Ok(status)
}

fn checked<'a>(root: &Path, paths: &'a [String]) -> Result<Vec<&'a str>> {
    if paths.is_empty() {
        return Err(Error::invalid("No files given"));
    }
    paths
        .iter()
        .map(|path| join(root, path).map(|_| path.trim_matches('/')))
        .collect()
}

/// One file's diff: the index against HEAD (`staged`), or the work tree
/// against the index. An untracked file diffs against nothing.
pub fn diff(root: &Path, path: &str, staged: bool) -> Result<GitFileDiff> {
    join(root, path)?;
    let path = path.trim_matches('/');
    let tracked = run(root, &["ls-files", "--error-unmatch", "--", path], &[0]).is_some();
    let base = ["diff", "--no-ext-diff", "--no-color", "--relative"];
    let mut patch = if staged {
        let mut args = base.to_vec();
        args.extend(["--cached", "-M", "--", path]);
        run(root, &args, &[0]).unwrap_or_default()
    } else if tracked {
        let mut args = base.to_vec();
        args.extend(["--", path]);
        run(root, &args, &[0]).unwrap_or_default()
    } else {
        // `--no-index` exits 1 when the files differ, which is the point.
        run(root, &["diff", "--no-ext-diff", "--no-color", "--no-index", "--", "/dev/null", path], &[0, 1])
            .unwrap_or_default()
    };
    let binary = patch.lines().any(|line| line.starts_with("Binary files ") || line == "GIT binary patch");
    let truncated = patch.len() > MAX_PATCH;
    if truncated {
        let mut cut = MAX_PATCH;
        while !patch.is_char_boundary(cut) {
            cut -= 1;
        }
        patch.truncate(cut);
        if let Some(line) = patch.rfind('\n') {
            patch.truncate(line + 1);
        }
    }
    Ok(GitFileDiff { path: path.to_string(), staged, patch, binary, truncated })
}

pub fn stage(root: &Path, paths: &[String]) -> Result<()> {
    let paths = checked(root, paths)?;
    let mut args = vec!["add", "-A", "--"];
    args.extend(paths);
    exec(root, &args).map(drop)
}

pub fn unstage(root: &Path, paths: &[String]) -> Result<()> {
    let paths = checked(root, paths)?;
    let mut args = if has_head(root) {
        vec!["restore", "--staged", "--"]
    } else {
        // Nothing to restore from on an unborn branch: drop from the index.
        vec!["rm", "--cached", "-r", "-q", "--"]
    };
    args.extend(paths);
    exec(root, &args).map(drop)
}

/// Throw away work-tree changes: tracked files go back to the index,
/// untracked files are removed (`git clean`, which never touches ignored files).
pub fn discard(root: &Path, paths: &[String]) -> Result<()> {
    let paths = checked(root, paths)?;
    let mut list = vec!["ls-files", "-z", "--"];
    list.extend(paths.iter().copied());
    let known = run(root, &list, &[0]).unwrap_or_default();
    let known: std::collections::HashSet<&str> = known.split('\0').filter(|p| !p.is_empty()).collect();
    let (tracked, untracked): (Vec<&str>, Vec<&str>) = paths.iter().partition(|path| known.contains(**path));
    if !untracked.is_empty() {
        let mut args = vec!["clean", "-f", "-q", "--"];
        args.extend(untracked);
        exec(root, &args)?;
    }
    if !tracked.is_empty() {
        let mut args = vec!["restore", "--worktree", "--"];
        args.extend(tracked);
        exec(root, &args)?;
    }
    Ok(())
}

/// Commit what is staged; with `all`, stage every change first (including
/// untracked files), like an editor's "Commit All".
pub fn commit(root: &Path, message: &str, all: bool) -> Result<GitCommitResult> {
    let message = message.trim();
    if message.is_empty() {
        return Err(Error::invalid("Write a commit message first"));
    }
    if all {
        exec(root, &["add", "-A", "--", "."])?;
    }
    exec(root, &["commit", "-q", "-m", message])?;
    let head = run(root, &["rev-parse", "--short", "HEAD"], &[0]).unwrap_or_default().trim().to_string();
    Ok(GitCommitResult { head, summary: message.lines().next().unwrap_or_default().to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_porcelain_v2() {
        let text = "# branch.oid 1234567890abcdef\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 M. N... 100644 100644 100644 aaa bbb src/a b.rs\0\
1 .M N... 100644 100644 100644 aaa bbb README.md\0\
2 R. N... 100644 100644 100644 aaa bbb R100 new.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 a b c conflict.txt\0\
? notes/todo.txt\0";
        let mut staged = HashMap::new();
        staged.insert("src/a b.rs".to_string(), (Some(3), Some(1)));
        let status = parse_status(text, &staged, &HashMap::new());
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.head.as_deref(), Some("1234567"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        assert_eq!(status.staged.len(), 2);
        assert_eq!(status.staged[0].path, "src/a b.rs");
        assert_eq!(status.staged[0].additions, Some(3));
        assert_eq!(status.staged[1].status, "R");
        assert_eq!(status.staged[1].orig_path.as_deref(), Some("old.rs"));
        assert_eq!(status.unstaged[0].path, "README.md");
        assert_eq!(status.conflicted[0].path, "conflict.txt");
        assert_eq!(status.untracked[0].path, "notes/todo.txt");
    }

    #[test]
    fn numstat_handles_renames() {
        let stats = numstat("1\t2\ta.rs\0-\t-\timg.png\0");
        assert_eq!(stats["a.rs"], (Some(1), Some(2)));
        assert_eq!(stats["img.png"], (None, None));
        let renamed = numstat("4\t0\t\0old.rs\0new.rs\0");
        assert_eq!(renamed["new.rs"], (Some(4), Some(0)));
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            assert!(Command::new("git").arg("-C").arg(root).args(args).output().unwrap().status.success(), "{args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.invalid"]);
        git(&["config", "user.name", "Test"]);
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "init"]);
        dir
    }

    #[test]
    fn stage_unstage_discard_commit_round_trip() {
        let dir = repo();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("new.txt"), "fresh\n").unwrap();
        let s = status(root).unwrap();
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.unstaged[0].path, "a.txt");
        assert_eq!(s.unstaged[0].additions, Some(1));
        assert_eq!(s.untracked[0].path, "new.txt");

        let d = diff(root, "a.txt", false).unwrap();
        assert!(d.patch.contains("+two"));
        let u = diff(root, "new.txt", false).unwrap();
        assert!(u.patch.contains("+fresh"));

        stage(root, &["a.txt".into()]).unwrap();
        let s = status(root).unwrap();
        assert_eq!(s.staged[0].path, "a.txt");
        assert!(diff(root, "a.txt", true).unwrap().patch.contains("+two"));
        unstage(root, &["a.txt".into()]).unwrap();
        assert!(status(root).unwrap().staged.is_empty());

        discard(root, &["new.txt".into(), "a.txt".into()]).unwrap();
        let s = status(root).unwrap();
        assert!(s.unstaged.is_empty() && s.untracked.is_empty());
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "one\n");

        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        let done = commit(root, "Add b\n\nbody", true).unwrap();
        assert_eq!(done.summary, "Add b");
        assert!(!done.head.is_empty());
        assert!(commit(root, "  ", false).is_err());
        assert!(stage(root, &["../x".into()]).is_err());
    }

    #[test]
    fn not_a_repository_reports_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!status(dir.path()).unwrap().repository);
    }
}
