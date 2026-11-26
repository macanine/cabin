//! The hello line the daemon sends first, and what the app concludes from it.
//!
//! Split from `rpc` so the desktop shell can compare builds on any host;
//! the rest of the wire protocol is guest-side (`rpc`).

use serde_json::Value;

pub const PROTOCOL: u32 = 3;
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The running executable's SHA-256, computed once. It identifies the exact
/// build even when two builds share `VERSION`. A process whose file was
/// replaced on disk still hashes its own (old) image, so a stale daemon left
/// by an interrupted update is reported as such.
pub fn build_id() -> &'static str {
    static BUILD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BUILD.get_or_init(|| {
        // Unit tests run from large debug binaries; a fixed id keeps them fast.
        if cfg!(test) {
            return crate::util::sha256_text("cabind test build");
        }
        let exe = if cfg!(unix) {
            let proc_exe = std::path::Path::new("/proc/self/exe");
            if proc_exe.exists() {
                proc_exe.to_path_buf()
            } else {
                std::env::current_exe().unwrap_or_default()
            }
        } else {
            std::env::current_exe().unwrap_or_default()
        };
        crate::util::sha256_file(&exe).unwrap_or_default()
    })
}

/// What the Mac concludes from a daemon's hello line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Handshake {
    /// The daemon is exactly the bundled build; carries its version.
    Ready(String),
    /// A different (or unidentified) build: redeploy, then reconnect.
    Outdated(String),
}

/// Compare a hello line with the build the app ships. A matching `build`
/// (the executable's SHA-256) is the authority; version and protocol must
/// agree too, so a hash collision of meaning can never slip through.
pub fn check_hello(hello: &Value, expected_build: &str) -> Handshake {
    let hello = &hello["hello"];
    let version = hello["version"].as_str().unwrap_or("unknown");
    let build = hello["build"].as_str().unwrap_or("");
    if build.is_empty() {
        return Handshake::Outdated(format!("cabind {version} predates build fingerprints"));
    }
    if build != expected_build {
        return Handshake::Outdated(format!(
            "cabind {version} ({}) is not the bundled build ({})",
            short_build(build),
            short_build(expected_build)
        ));
    }
    if version != VERSION || hello["protocol"].as_u64() != Some(u64::from(PROTOCOL)) {
        return Handshake::Outdated(format!(
            "cabind {version} speaks protocol {} but the app expects {VERSION} / {PROTOCOL}",
            hello["protocol"]
        ));
    }
    Handshake::Ready(version.to_string())
}

/// The first 12 hex digits, for messages and the UI.
pub fn short_build(build: &str) -> &str {
    &build[..build.len().min(12)]
}
