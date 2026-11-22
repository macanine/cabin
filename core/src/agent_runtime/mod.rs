//! Guest-side chat runtime. Every agent runs over the Agent Client Protocol
//! (`acp`), on the runtime `runtimes` manages; `session` owns one live chat
//! and `context` is the only write path a runtime gets.
pub mod acp;
pub mod context;
pub mod discover;
pub mod runtimes;
pub mod session;
pub(crate) mod stdio;

use std::path::{Component, Path, PathBuf};

use crate::model::AgentKind;

/// Lexically normalize a provider path against the workspace root. `None`
/// when it escapes the workspace; file-change UI never sees outside paths.
pub fn relativize(path: &str, root: &Path) -> Option<String> {
    let candidate = Path::new(path);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        root.join(candidate)
    };
    let mut normal = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::ParentDir => {
                normal.pop();
            }
            Component::CurDir => {}
            other => normal.push(other.as_os_str()),
        }
    }
    let relative = normal.strip_prefix(root).ok()?;
    let relative = relative.to_string_lossy().to_string();
    (!relative.is_empty()).then_some(relative)
}

/// The runtime binary a chat drives, and how to install it when it is missing.
pub struct RuntimeBinary {
    pub path: std::path::PathBuf,
    /// npm argv that creates `path`; `None` when it is already there or overridden.
    pub install: Option<Vec<String>>,
}

/// The ACP runtime a new chat with this agent starts (see `runtimes`).
pub fn runtime_binary(
    ctx: &crate::context::Context,
    agent: AgentKind,
) -> crate::error::Result<RuntimeBinary> {
    runtimes::binary(ctx, agent)
}

/// Run the install (if one is needed and the binary is still missing).
pub fn ensure_installed(
    binary: &RuntimeBinary,
    env: &[(String, String)],
) -> crate::error::Result<()> {
    use crate::error::Error;
    let Some(argv) = &binary.install else {
        return Ok(());
    };
    if binary.path.is_file() {
        return Ok(());
    }
    if let Some(dir) = argv
        .iter()
        .position(|arg| arg == "--prefix")
        .and_then(|index| argv.get(index + 1))
    {
        std::fs::create_dir_all(dir)
            .map_err(|error| Error::io("create the runtime folder", error))?;
    }
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| Error::io("install the agent runtime", error))?;
    if out.status.success() && binary.path.is_file() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let tail: String = stderr
        .chars()
        .rev()
        .take(600)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Err(Error::Unavailable(format!(
        "Could not install the agent runtime: {}",
        tail.trim()
    )))
}
