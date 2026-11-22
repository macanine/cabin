//! The session panel's workbench: browse the session's files, review and
//! stage its git changes, and find the dev servers a browser preview can show.
//!
//! Everything works on one root (the session's worktree, else the workspace
//! folder). Paths crossing IPC are root-relative and "/"-separated; they are
//! checked lexically (no absolute paths, no `..`) and, for anything read from
//! disk, again after resolving symlinks, so nothing outside the root is read.

pub mod files;
pub mod git;
pub mod ports;

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// `rel` joined to `root`, refusing absolute paths and `..`. Does not touch disk.
pub fn join(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim_matches('/');
    if rel.is_empty() || rel == "." {
        return Ok(root.to_path_buf());
    }
    let path = Path::new(rel);
    if !path
        .components()
        .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::invalid(format!("Not a path inside the workspace: {rel}")));
    }
    Ok(root.join(path))
}

/// `join`, then require that the real (symlink-resolved) path stays in the root.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let joined = join(root, rel)?;
    let real_root = root
        .canonicalize()
        .map_err(|error| Error::path("open", root, error))?;
    let real = joined
        .canonicalize()
        .map_err(|error| Error::path("open", &joined, error))?;
    if !real.starts_with(&real_root) {
        return Err(Error::invalid(format!("{rel} points outside the workspace")));
    }
    Ok(real)
}

/// Root-relative, "/"-separated form of a path under `root`.
pub fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_refuses_escapes() {
        let root = Path::new("/w");
        assert_eq!(join(root, "").unwrap(), PathBuf::from("/w"));
        assert_eq!(join(root, "src/a.rs").unwrap(), PathBuf::from("/w/src/a.rs"));
        assert!(join(root, "../etc/passwd").is_err());
        assert!(join(root, "src/../../x").is_err());
        // Leading slashes are trimmed: the path is always root-relative.
        assert_eq!(join(root, "/src").unwrap(), PathBuf::from("/w/src"));
    }

    #[test]
    fn resolve_refuses_symlinks_out_of_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(dir.path().join("secret"), "x").unwrap();
        std::fs::write(root.join("ok.txt"), "y").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), root.join("link")).unwrap();
        assert!(resolve(&root, "ok.txt").is_ok());
        assert!(resolve(&root, "link").is_err());
    }
}
