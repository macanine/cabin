//! Turning file-system events into small batches of folders to re-scan.
//!
//! FSEvents (Mac) and inotify (Linux) both report paths; a burst (a branch
//! switch, an `npm install`) is collected for a short quiet period and handed
//! on as the set of parent folders that changed, which `scan` takes as its
//! scope. Events under ignored paths are dropped early (approximately: the
//! root's ignore files and the rules; nested `.gitignore` files are honoured
//! by the scan itself).

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::{RecursiveMode, Watcher};

use super::{GitHandling, IGNORE_FILE, SyncRules};

/// Quiet time that ends a burst.
const SETTLE: Duration = Duration::from_millis(200);
/// A burst is never held longer than this.
const MAX_HOLD: Duration = Duration::from_millis(1500);
/// More folders than this: re-scan everything instead.
const MAX_SCOPE: usize = 256;

fn noise_filter(root: &Path, rules: &SyncRules) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    if rules.gitignore {
        let _ = builder.add(root.join(".gitignore"));
    }
    let _ = builder.add(root.join(IGNORE_FILE));
    for line in [".DS_Store", "._*", "*.vasync-tmp"].into_iter().map(str::to_string).chain(rules.ignore.iter().cloned()) {
        let _ = builder.add_line(None, &line);
    }
    for path in &rules.excluded {
        let _ = builder.add_line(None, &format!("/{}", path.trim_matches('/')));
    }
    if rules.git != GitHandling::Sync {
        let _ = builder.add_line(None, "/.git");
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

/// The folder (relative, `""` for the root) whose listing an event changes.
fn scope_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let parent = rel.parent().unwrap_or(Path::new(""));
    Some(parent.to_string_lossy().into_owned())
}

/// Collapse `a`, `a/b` into `a`; too many folders become one full scan.
pub fn collapse(dirs: BTreeSet<String>) -> Vec<String> {
    if dirs.contains("") || dirs.len() > MAX_SCOPE {
        return vec![String::new()];
    }
    let mut out: Vec<String> = Vec::new();
    for dir in dirs {
        if !out.iter().any(|kept| dir.starts_with(&format!("{kept}/"))) {
            out.push(dir);
        }
    }
    out
}

/// A running watch: batches of folders to re-scan. The watch stops when
/// `watcher` is dropped (the receiver then ends).
pub struct Batches {
    pub watcher: notify::RecommendedWatcher,
    pub batches: Receiver<Vec<String>>,
}

pub fn watch(root: &Path, rules: &SyncRules) -> notify::Result<Batches> {
    let root: PathBuf = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let filter = noise_filter(&root, rules);
    let (events_tx, events) = mpsc::channel::<PathBuf>();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Ok(event) = result {
            for path in event.paths {
                let _ = events_tx.send(path);
            }
        }
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;
    let (batch_tx, batches) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let Ok(first) = events.recv() else { return };
            let mut dirs = BTreeSet::new();
            let mut add = |path: PathBuf| {
                let ignored = path
                    .strip_prefix(&root)
                    .map(|rel| filter.matched_path_or_any_parents(rel, path.is_dir()).is_ignore())
                    .unwrap_or(true);
                if !ignored && let Some(dir) = scope_of(&root, &path) {
                    dirs.insert(dir);
                }
            };
            add(first);
            let started = Instant::now();
            loop {
                let wait = SETTLE.min(MAX_HOLD.saturating_sub(started.elapsed()));
                match events.recv_timeout(wait) {
                    Ok(path) => add(path),
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
                if started.elapsed() >= MAX_HOLD {
                    break;
                }
            }
            if !dirs.is_empty() && batch_tx.send(collapse(dirs)).is_err() {
                return;
            }
        }
    });
    Ok(Batches { watcher, batches })
}

/// The guest's `sync watch`: one JSON array of folders per line, until the
/// Mac closes the connection.
pub fn print_changes(root: &Path, rules: &SyncRules) -> Result<(), String> {
    let batches = watch(root, rules).map_err(|e| format!("could not watch {}: {e}", root.display()))?;
    let stdout = std::io::stdout();
    // A ready line lets the Mac know the watch is live before it scans.
    writeln!(stdout.lock(), "[]").map_err(|e| e.to_string())?;
    for batch in batches.batches {
        let mut out = stdout.lock();
        let line = serde_json::to_string(&batch).map_err(|e| e.to_string())?;
        if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_folders_collapse_and_big_bursts_become_a_full_scan() {
        let dirs: BTreeSet<String> = ["src", "src/a", "docs", "srcx"].into_iter().map(String::from).collect();
        assert_eq!(collapse(dirs), ["docs", "src", "srcx"]);
        let many: BTreeSet<String> = (0..300).map(|i| format!("d{i}")).collect();
        assert_eq!(collapse(many), [""]);
    }

    #[test]
    fn events_in_ignored_paths_are_noise() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();
        let filter = noise_filter(dir.path(), &SyncRules::default());
        assert!(filter.matched_path_or_any_parents("node_modules/a/b.js", false).is_ignore());
        assert!(filter.matched_path_or_any_parents(".git/index", false).is_ignore());
        assert!(!filter.matched_path_or_any_parents("src/main.rs", false).is_ignore());
        assert_eq!(scope_of(dir.path(), &dir.path().join("a.txt")).as_deref(), Some(""));
        assert_eq!(scope_of(dir.path(), &dir.path().join("src/a.rs")).as_deref(), Some("src"));
    }

    #[test]
    fn a_change_arrives_as_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let batches = watch(dir.path(), &SyncRules::default()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        std::fs::write(dir.path().join("src/new.rs"), "x").unwrap();
        let batch = batches.batches.recv_timeout(Duration::from_secs(5)).expect("a batch");
        assert!(batch == ["src"] || batch == [""], "{batch:?}");
    }
}
