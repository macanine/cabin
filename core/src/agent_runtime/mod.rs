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
    /// npm argv for a managed runtime, including an already installed version.
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
    if let Some(argv) = &binary.install {
        runtimes::ensure_installed_until(&binary.path, argv, env, &|| false)?;
    }
    Ok(())
}
