//! Locations of mutable application state.
//!
//! Packaged resources are read-only; everything the app writes lives under the
//! data directory (`CABIN_HOME`, else `$XDG_DATA_HOME/cabin`, else
//! `~/.local/share/cabin`).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppPaths {
    data: PathBuf,
    home: PathBuf,
    node: PathBuf,
}

/// Where the guest keeps managed Node.js builds: short, system-wide and
/// outside the home directory (`/opt/node/<version>`, `/opt/node/current`).
pub const DEFAULT_NODE_ROOT: &str = "/opt/node";

impl AppPaths {
    /// Self-contained paths (tests): Node.js lives under `<data>/node`.
    pub fn new(data: impl Into<PathBuf>, home: impl Into<PathBuf>) -> Self {
        let data = data.into();
        Self {
            node: data.join("node"),
            data,
            home: home.into(),
        }
    }

    pub fn with_node_root(mut self, node: impl Into<PathBuf>) -> Self {
        self.node = node.into();
        self
    }

    pub fn from_env() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let data = std::env::var_os("CABIN_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("XDG_DATA_HOME").map(|dir| PathBuf::from(dir).join("cabin"))
            })
            .unwrap_or_else(|| home.join(".local/share/cabin"));
        let node = std::env::var_os("CABIN_NODE_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_NODE_ROOT));
        Self::new(data, home).with_node_root(node)
    }

    pub fn data(&self) -> &Path {
        &self.data
    }

    /// The user's home directory (agent configs live here).
    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    pub fn database(&self) -> PathBuf {
        self.data.join("state.sqlite3")
    }

    pub fn backups(&self) -> PathBuf {
        self.data.join("backups")
    }

    pub fn environment_script(&self) -> PathBuf {
        self.data.join("environment.sh")
    }

    /// Node.js builds managed by cabin: `<v>/` per version and the
    /// `current` symlink (`CABIN_NODE_ROOT`, else `/opt/node`).
    pub fn node_root(&self) -> PathBuf {
        self.node.clone()
    }

    /// Downloaded archives waiting to be unpacked (user-owned cache).
    pub fn node_downloads(&self) -> PathBuf {
        self.data.join("node-downloads")
    }

    /// Stable bin directory of the selected Node.js (through `current`).
    pub fn node_bin(&self) -> PathBuf {
        self.node_root().join("current").join("bin")
    }

    /// Directories searched for agent binaries in addition to `PATH`. The
    /// managed Node.js comes first so it wins over Ubuntu's `/usr/bin/node`.
    pub fn extra_bin_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.node_bin()];
        dirs.extend(
            [".local/bin", ".npm-global/bin", ".bun/bin", ".opencode/bin"]
                .iter()
                .map(|dir| self.home.join(dir)),
        );
        dirs
    }

    /// `PATH` used for child processes: the inherited one plus user bin dirs.
    pub fn child_path(&self) -> String {
        let mut parts: Vec<String> = self
            .extra_bin_dirs()
            .iter()
            .map(|dir| dir.display().to_string())
            .collect();
        let inherited =
            std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
        for part in inherited.split(':') {
            if !part.is_empty() && !parts.iter().any(|existing| existing == part) {
                parts.push(part.to_string());
            }
        }
        parts.join(":")
    }
}
