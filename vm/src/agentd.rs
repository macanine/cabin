//! Deploying and reaching `cabind` inside the VM over SSH.
//!
//! Whether the guest runs the bundled build is decided by the daemon's own
//! hello line (`build` = SHA-256 of its executable), not by a separate
//! checksum command: the app connects first and deploys only on a mismatch.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::Result;
use serde_json::Value;

use crate::util::{capture, capture_file, message};
use crate::vm::Runtime;

/// Install location inside the guest (relative to the user's home).
pub const REMOTE_BIN: &str = ".local/bin/cabind";

/// Root-owned helper used before the daemon is available and by guest services.
/// This is deliberately separate from the user's daemon and its service.
pub const HELPER_BIN: &str = "/usr/local/libexec/cabin/cabind";
const HELPER_DIR: &str = "/usr/local/libexec/cabin";

/// Default guest binary for standalone vmctl. The app supplies its resource path
/// explicitly, so a missing or invalid override is never silently replaced.
pub fn default_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("CABIND_BIN") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|dir| dir.join("cabind")))
        .unwrap_or_else(|| PathBuf::from("cabind"))
}

pub fn require_binary(binary: &Path) -> Result<&Path> {
    if !binary.is_file() {
        return message(format!(
            "Guest cabind binary not found at {}. Set CABIND_BIN to the Linux {} cabind binary (or place it next to vmctl).",
            binary.display(),
            crate::platform::guest_arch()
        ));
    }
    Ok(binary)
}

// The directory and each of its ancestors must be root-owned and not writable
// by the guest user. Check without following a final symlink before creating a
// root-only temporary file on the same filesystem as the installed helper.
fn helper_script(directory: &str, expected: &str, owner: u32, ancestors: &[&str]) -> String {
    let mut script = String::from("set -eu\n");
    for path in ancestors.iter().copied().chain(std::iter::once(directory)) {
        let quoted = crate::util::shell_quote(path);
        script.push_str(&format!(
            "if [ ! -e {quoted} ] && [ ! -L {quoted} ]; then install -d -m 0755 {quoted}; fi\n\
             [ ! -L {quoted} ] && [ -d {quoted} ] && [ \"$(stat -c %u {quoted})\" = {owner} ] && [ -z \"$(find {quoted} -maxdepth 0 -perm /022 -print)\" ] || {{ printf '%s\\n' 'Unsafe helper directory: {path}' >&2; exit 1; }}\n"
        ));
    }
    let dir = crate::util::shell_quote(directory);
    script.push_str(&format!(
        "dir={dir}\n\
         target=\"$dir/cabind\"\n\
         tmp=$(mktemp \"$dir/.cabind.XXXXXXXXXX\")\n\
         trap 'rm -f -- \"$tmp\"' EXIT HUP INT TERM\n\
         cat > \"$tmp\"\n\
         printf '%s  %s\\n' '{expected}' \"$tmp\" | sha256sum --check --status || {{ printf '%s\\n' 'Guest helper SHA-256 verification failed' >&2; exit 1; }}\n\
         chmod 0755 \"$tmp\"\n\
         mv -fT -- \"$tmp\" \"$target\"\n"
    ));
    script
}

/// Install the bundled guest binary for native helpers independently of the
/// hello-first daemon service. Each upload uses a private root-owned temporary
/// file, verifies the bytes received and atomically replaces the stable path.
pub fn ensure_helper(runtime: &Runtime, binary: &Path) -> Result<()> {
    require_binary(binary)?;
    let expected = crate::util::sha256_file(binary)?;
    let script = helper_script(
        HELPER_DIR,
        &expected,
        0,
        &["/usr", "/usr/local", "/usr/local/libexec"],
    );
    let mut argv = runtime.ssh_argv(false)?;
    argv.push(format!("sudo -n sh -c {}", crate::util::shell_quote(&script)));
    let output = capture_file(&argv, Duration::from_secs(120), File::open(binary)?);
    if output.code == 124 {
        return message("Timed out deploying the guest native helper over SSH");
    }
    if output.code != 0 {
        return message(format!(
            "Could not deploy the guest native helper: {}",
            output.output.trim()
        ));
    }
    Ok(())
}

/// Copy `binary` into the guest, replace the old one atomically and
/// (re)start the systemd user service.
pub fn deploy(runtime: &Runtime, binary: &Path) -> Result<()> {
    if !binary.is_file() {
        return message(format!(
            "cabind for the VM was not found at {}",
            binary.display()
        ));
    }
    let expected = crate::util::sha256_file(binary)?;
    let mut argv = runtime.ssh_argv(false)?;
    argv.push(format!(
        "uid=$(id -u); gid=$(id -g); for dir in \"$HOME/.local\" \"$HOME/.local/share\" \"$HOME/.local/state\" \"$HOME/.local/bin\" \"$HOME/.config\" \"$HOME/.config/systemd/user\"; do if ! mkdir -p \"$dir\" 2>/dev/null || [ ! -w \"$dir\" ]; then sudo -n install -d -o \"$uid\" -g \"$gid\" \"$dir\" && sudo -n chown \"$uid:$gid\" \"$HOME/.local\" \"$HOME/.config\" \"$dir\"; fi; done; \
         cat > \"$HOME/{REMOTE_BIN}.new\" && \
         printf '%s  %s\\n' '{expected}' \"$HOME/{REMOTE_BIN}.new\" | sha256sum --check && \
         chmod 0755 \"$HOME/{REMOTE_BIN}.new\" && mv -f \"$HOME/{REMOTE_BIN}.new\" \"$HOME/{REMOTE_BIN}\" && \
         \"$HOME/{REMOTE_BIN}\" install-service"
    ));
    let output = capture_file(&argv, Duration::from_secs(75), File::open(binary)?);
    if output.code == 124 {
        return message("Timed out deploying cabind in the VM");
    }
    if output.code != 0 {
        return message(format!(
            "Could not deploy cabind in the VM: {}",
            output.output.trim()
        ));
    }
    Ok(())
}

/// Run one guest display operation with a JSON request on stdin.
pub fn native_display(runtime: &Runtime, request: &Value, timeout: Duration) -> Result<Value> {
    let mut argv = runtime.ssh_argv(false)?;
    argv.push(format!("\"$HOME/{REMOTE_BIN}\" native-display"));
    let payload = serde_json::to_string(request)?;
    let response = capture(&argv, timeout, Some(&payload));
    if response.code == 124 {
        return message("The guest display command timed out.");
    }
    if response.code != 0 {
        return message(format!(
            "Guest display command failed: {}",
            response.output.trim()
        ));
    }
    // capture appends stderr after stdout. The guest writes one JSON line.
    serde_json::from_str(response.output.lines().next().unwrap_or_default())
        .map_err(|error| crate::Error::Message(format!("Invalid guest display response: {error}")))
}

/// The command that bridges stdin/stdout to the guest daemon.
pub fn connect_argv(runtime: &Runtime) -> Result<Vec<String>> {
    let mut argv = runtime.ssh_argv(false)?;
    argv.push(format!("\"$HOME/{REMOTE_BIN}\" connect"));
    Ok(argv)
}
