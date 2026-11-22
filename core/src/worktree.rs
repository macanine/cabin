//! Managed, session-specific Git worktrees in the guest.

use crate::util::LockExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::model::{OperationState, SessionWorktree};
use crate::ops::{Operation, OperationManager, Step, StepCtx};

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct WorktreeRefs {
    pub head: String,
    pub branches: Vec<String>,
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|error| Error::io("Could not run Git", error))?;
    if !output.status.success() {
        return Err(Error::invalid(format!(
            "Git: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    String::from_utf8(output.stdout)
        .map(|s| s.trim_end().to_owned())
        .map_err(|_| Error::invalid("Git returned a non-UTF-8 path or reference"))
}

pub fn refs(path: &Path) -> Result<WorktreeRefs> {
    let commit = git(path, &["rev-parse", "--verify", "HEAD^{commit}"])
        .map_err(|_| Error::invalid("A Git repository with at least one commit is required"))?;
    let head = git(path, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .unwrap_or_else(|_| format!("{} (detached)", &commit[..commit.len().min(8)]));
    let branches = git(
        path,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
        ],
    )?
    .lines()
    .filter(|line| !line.ends_with("/HEAD"))
    .take(300)
    .map(str::to_owned)
    .collect();
    Ok(WorktreeRefs { head, branches })
}

struct Create {
    source: PathBuf,
    destination: PathBuf,
    base: String,
    repository: Option<PathBuf>,
    relative: Option<PathBuf>,
    commit: Option<String>,
    created: bool,
    result: Arc<Mutex<Option<SessionWorktree>>>,
}

impl Step for Create {
    fn name(&self) -> String {
        "Create Git worktree".into()
    }

    fn prepare(&mut self, _: &StepCtx<'_>) -> Result<()> {
        let source = self
            .source
            .canonicalize()
            .map_err(|e| Error::path("Open workspace", &self.source, e))?;
        let repository =
            PathBuf::from(git(&source, &["rev-parse", "--show-toplevel"])?).canonicalize()?;
        let relative = source
            .strip_prefix(&repository)
            .map_err(|_| Error::invalid("Workspace is outside its Git repository"))?
            .to_path_buf();
        if self.destination.exists() {
            return Err(Error::Conflict(
                "Worktree destination already exists".into(),
            ));
        }
        let revision = format!("{}^{{commit}}", self.base);
        let commit = git(
            &repository,
            &["rev-parse", "--verify", "--end-of-options", &revision],
        )
        .map_err(|_| Error::invalid("The selected Git reference does not resolve to a commit"))?;
        self.repository = Some(repository);
        self.relative = Some(relative);
        self.commit = Some(commit);
        Ok(())
    }

    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        let parent = self
            .destination
            .parent()
            .ok_or_else(|| Error::invalid("Invalid worktree path"))?;
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::path("Create worktree directory", parent, e))?;
        let repository = self
            .repository
            .as_ref()
            .ok_or_else(|| Error::Internal("Missing repository".into()))?;
        let destination = self
            .destination
            .to_str()
            .ok_or_else(|| Error::invalid("Invalid worktree path"))?;
        ctx.log(format!("Checking out {}", self.base));
        git(
            repository,
            &[
                "worktree",
                "add",
                "--detach",
                "--",
                destination,
                self.commit.as_deref().unwrap_or(""),
            ],
        )?;
        self.created = true;
        Ok(())
    }

    fn verify(&mut self, _: &StepCtx<'_>) -> Result<()> {
        let root = self.destination.canonicalize()?;
        let path = root.join(
            self.relative
                .as_ref()
                .ok_or_else(|| Error::Internal("Missing workspace path".into()))?,
        );
        let path = path.canonicalize().map_err(|_| Error::invalid(
            "The workspace subdirectory does not exist in the selected commit",
        ))?;
        if !path.is_dir() || !path.starts_with(&root) {
            return Err(Error::invalid("The workspace subdirectory must stay inside the new worktree"));
        }
        let worktree = SessionWorktree {
            path: path.to_string_lossy().into_owned(),
            root: root.to_string_lossy().into_owned(),
            repository: self
                .repository
                .as_ref()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            base: self.base.clone(),
        };
        *self.result.locked() = Some(worktree);
        Ok(())
    }

    fn rollback(&mut self, _: &StepCtx<'_>) -> Result<()> {
        if self.created {
            let repository = self.repository.as_ref().unwrap();
            let destination = self
                .destination
                .to_str()
                .ok_or_else(|| Error::invalid("Invalid worktree path"))?;
            git(repository, &["worktree", "remove", "--", destination])?;
        }
        Ok(())
    }
}

pub fn create(
    ops: &OperationManager,
    source: &Path,
    data: &Path,
    id: &str,
    base: &str,
) -> Result<SessionWorktree> {
    let result = Arc::new(Mutex::new(None));
    let record = ops.run(
        Operation::new("worktree.create", "Create session worktree")
            .key(format!("worktree:{id}"))
            .step(Create {
                source: source.to_owned(),
                destination: data.join("worktrees").join(id),
                base: base.to_owned(),
                repository: None,
                relative: None,
                commit: None,
                created: false,
                result: result.clone(),
            }),
    )?;
    if record.state != OperationState::Succeeded {
        return Err(Error::invalid(
            record
                .error
                .unwrap_or_else(|| "Worktree creation failed".into()),
        ));
    }
    let value = result.locked()
        .take()
        .ok_or_else(|| Error::Internal("Worktree creation did not return a path".into()))?;
    Ok(value)
}

struct Remove {
    worktree: SessionWorktree,
    data: PathBuf,
}

impl Step for Remove {
    fn name(&self) -> String {
        "Remove clean Git worktree".into()
    }
    fn apply(&mut self, ctx: &StepCtx<'_>) -> Result<()> {
        ctx.log(format!("Checking {}", self.worktree.root));
        remove(&self.worktree, &self.data)
    }
    fn verify(&mut self, _: &StepCtx<'_>) -> Result<()> {
        if Path::new(&self.worktree.root).exists() {
            return Err(Error::Conflict(
                "Worktree still exists after removal".into(),
            ));
        }
        Ok(())
    }
}

pub fn remove_managed(
    ops: &OperationManager,
    worktree: &SessionWorktree,
    data: &Path,
) -> Result<()> {
    let record = ops.run(
        Operation::new("worktree.remove", "Remove session worktree")
            .key(format!("worktree:{}", worktree.root))
            .step(Remove {
                worktree: worktree.clone(),
                data: data.to_owned(),
            }),
    )?;
    if record.state != OperationState::Succeeded {
        return Err(Error::Conflict(
            record
                .error
                .unwrap_or_else(|| "Worktree removal failed".into()),
        ));
    }
    Ok(())
}

pub fn remove(worktree: &SessionWorktree, data: &Path) -> Result<()> {
    let managed = data.join("worktrees").canonicalize()?;
    let root = Path::new(&worktree.root).canonicalize()?;
    if root.parent() != Some(managed.as_path()) {
        return Err(Error::invalid("This worktree is not managed by cabin"));
    }
    let status = git(
        &root,
        &[
            "status",
            "--porcelain",
            "--untracked-files=all",
            "--ignored=matching",
        ],
    )?;
    if !status.is_empty() {
        return Err(Error::Conflict(
            "Worktree has uncommitted or untracked changes; clean or save them before removing it"
                .into(),
        ));
    }
    let references = git(
        &root,
        &[
            "for-each-ref",
            "--contains",
            "HEAD",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    if references.is_empty() {
        return Err(Error::Conflict("This worktree contains commits not retained by a branch or tag. Save them to a branch before removing it".into()));
    }
    git(
        Path::new(&worktree.repository),
        &["worktree", "remove", "--", &worktree.root],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    fn repository() -> Result<(tempfile::TempDir, PathBuf, PathBuf, OperationManager)> {
        let dir = tempfile::tempdir()?;
        let repo = dir.path().join("repo");
        let data = dir.path().join("data");
        std::fs::create_dir_all(repo.join("project"))?;
        git(&repo, &["init"])?;
        git(&repo, &["config", "user.name", "Test"])?;
        git(&repo, &["config", "user.email", "test@example.invalid"])?;
        std::fs::write(repo.join("project/file.txt"), "committed\n")?;
        std::fs::write(repo.join(".gitignore"), "ignored.txt\n")?;
        git(&repo, &["add", "."])?;
        git(
            &repo,
            &["-c", "commit.gpgsign=false", "commit", "-m", "initial"],
        )?;
        let ops = OperationManager::new(Arc::new(Store::in_memory()?));
        Ok((dir, repo, data, ops))
    }

    #[test]
    fn isolated_checkout_uses_selected_commit_and_protects_changes() -> Result<()> {
        let (_dir, repo, data, ops) = repository()?;
        git(&repo, &["branch", "other"])?;
        std::fs::write(repo.join("project/file.txt"), "uncommitted\n")?;
        let tree = create(
            &ops,
            &repo.join("project"),
            &data,
            "ses_test",
            "refs/heads/other",
        )?;
        assert_eq!(
            std::fs::read_to_string(Path::new(&tree.path).join("file.txt"))?,
            "committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("project/file.txt"))?,
            "uncommitted\n"
        );
        assert!(
            refs(&repo)?
                .branches
                .iter()
                .any(|item| item == "refs/heads/other")
        );
        std::fs::write(Path::new(&tree.root).join("ignored.txt"), "keep")?;
        assert!(remove(&tree, &data).is_err());
        assert!(Path::new(&tree.root).join("ignored.txt").exists());
        std::fs::remove_file(Path::new(&tree.root).join("ignored.txt"))?;
        std::fs::write(Path::new(&tree.path).join("file.txt"), "new changes")?;
        assert!(remove(&tree, &data).is_err());
        git(Path::new(&tree.root), &["restore", "project/file.txt"])?;
        std::fs::write(Path::new(&tree.path).join("file.txt"), "committed work")?;
        git(Path::new(&tree.root), &["add", "project/file.txt"])?;
        git(
            Path::new(&tree.root),
            &["-c", "commit.gpgsign=false", "commit", "-m", "work"],
        )?;
        assert!(remove(&tree, &data).is_err());
        git(Path::new(&tree.root), &["branch", "saved-work"])?;
        remove(&tree, &data)?;
        assert!(!Path::new(&tree.root).exists());
        Ok(())
    }

    #[test]
    fn missing_subdirectory_rolls_back_without_touching_source() -> Result<()> {
        let (_dir, repo, data, ops) = repository()?;
        git(&repo, &["branch", "before-removal"])?;
        git(&repo, &["rm", "-r", "project"])?;
        git(
            &repo,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "remove project",
            ],
        )?;
        // Restore the directory locally so the source remains a valid workspace.
        std::fs::create_dir(repo.join("project"))?;
        assert!(create(&ops, &repo.join("project"), &data, "ses_missing", "HEAD").is_err());
        assert!(!data.join("worktrees/ses_missing").exists());
        assert!(repo.join("project").exists());
        assert!(create(&ops, &repo, &data, "ses_bad", "not-a-ref").is_err());
        Ok(())
    }
}
