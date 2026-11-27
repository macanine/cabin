//! Deploying and reaching `cabind` inside the VM over SSH.
//!
//! Whether the guest runs the bundled build is decided by the daemon's own
//! hello line (`build` = SHA-256 of its executable), not by a separate
//! checksum command: the app connects first and deploys only on a mismatch.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

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
            crate::arch::GuestArch::current().rust_musl_triple()
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
    argv.push(format!(
        "sudo -n sh -c {}",
        crate::util::shell_quote(&script)
    ));
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

/// The numeric target is supplied by the host's verified qcow2 capacity.
pub(crate) fn grow_root(runtime: &Runtime, bytes: u64) -> Result<()> {
    let (code, output) = runtime.ssh_capture(
        &format!("sudo -n {HELPER_BIN} guest-system grow-root {bytes}"),
        Duration::from_secs(240),
    );
    if code != 0 {
        return message(format!(
            "Could not verify guest disk expansion (exit {code}): {}",
            output.trim()
        ));
    }
    Ok(())
}

fn deploy_script(expected: &str) -> String {
    format!(
        r#"
set -eu
uid=$(id -u); gid=$(id -g)
for dir in "$HOME/.local" "$HOME/.local/share" "$HOME/.local/state" "$HOME/.local/bin" "$HOME/.config" "$HOME/.config/systemd/user"; do
    if ! mkdir -p "$dir" 2>/dev/null || [ ! -w "$dir" ]; then
        sudo -n install -d -o "$uid" -g "$gid" "$dir"
        sudo -n chown "$uid:$gid" "$HOME/.local" "$HOME/.config" "$dir"
    fi
done
target="$HOME/{REMOTE_BIN}"
tmp=$(mktemp "$target.XXXXXXXXXX")
trap 'rm -f -- "$tmp"' EXIT
trap 'exit 1' HUP INT TERM
cat > "$tmp"
printf '%s  %s\n' '{expected}' "$tmp" | sha256sum --check
chmod 0755 "$tmp"
mv -fT -- "$tmp" "$target"
"$target" install-service
"#
    )
}

/// Copy into a unique file beside cabind, verify it and replace the installed
/// binary atomically before (re)starting the systemd user service.
pub fn deploy(runtime: &Runtime, binary: &Path) -> Result<()> {
    require_binary(binary)?;
    let expected = crate::util::sha256_file(binary)?;
    let mut argv = runtime.ssh_argv(false)?;
    argv.push(format!(
        "sh -c {}",
        crate::util::shell_quote(&deploy_script(&expected))
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

/// Check the current QEMU process and its chardevs without connecting to a lane.
#[cfg(unix)]
pub fn native_channel_dir(runtime: &Runtime) -> Result<PathBuf> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let unavailable =
        || crate::Error::Message("Restart the VM to enable the native CLI channels".into());
    let dir = crate::vm::cli_dir(&runtime.cfg);
    let meta = dir.symlink_metadata().map_err(|_| unavailable())?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return message("The native CLI channel directory is not private");
    }
    for lane in crate::vm::CLI_LANES {
        if !dir
            .join(format!("{lane}.sock"))
            .symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_socket())
        {
            return Err(unavailable());
        }
    }
    let timeout = Duration::from_secs(1);
    let deadline = Instant::now() + timeout;
    let mut qmp = crate::qmp::QmpClient::connect(&runtime.cfg.qmp_socket(), timeout)?;
    let devices = qmp.execute_until("query-chardev", None, deadline)?;
    if !has_native_chardevs(&devices, &dir) {
        return Err(unavailable());
    }
    Ok(dir.canonicalize()?)
}

#[cfg(unix)]
fn has_native_chardevs(devices: &Value, dir: &Path) -> bool {
    devices.as_array().is_some_and(|devices| {
        crate::vm::CLI_LANES.into_iter().all(|lane| {
            let label = format!("cabin-{lane}");
            let path = dir.join(format!("{lane}.sock"));
            let filename = format!("unix:{},server=on", path.display());
            devices.iter().any(|device| {
                device["label"].as_str() == Some(&label)
                    && device["filename"].as_str().is_some_and(|name| {
                        name.strip_prefix("disconnected:").unwrap_or(name) == filename
                    })
            })
        })
    })
}

/// The service can be active before it has opened the virtio device. Verify
/// the actual process and its descriptor without borrowing any lane.
#[cfg(unix)]
const NATIVE_GUEST_CHECK: &str = r#"
native_device() {
  target=$(readlink -f "/dev/virtio-ports/org.cabin.$1") || return 1
  case "$target" in /dev/vport[0-9]*p[0-9]*) ;; *) return 1 ;; esac
  test -c "$target" && printf '%s\n' "$target"
}
native_lane_ready() {
  target=$(native_device "$1") || return 1
  unit="cabind-native-$1.service"
  systemctl --user is-active --quiet "$unit" || return 1
  pid=$(systemctl --user show --property=MainPID --value "$unit") || return 1
  case "$pid" in ''|0|*[!0-9]*) return 1 ;; esac
  test "/proc/$pid/exe" -ef "$binary" || return 1
  for fd in /proc/"$pid"/fd/*; do
    if test "$fd" -ef "$target"; then return 0; fi
  done
  return 1
}
native_ready() {
  for lane in exec sync watch; do
    native_lane_ready "$lane" || return 1
  done
}
"#;

#[cfg(unix)]
pub fn verify_native_guest(runtime: &Runtime) -> Result<()> {
    run_native_script(runtime, "native_ready", Duration::from_secs(4))
}

#[cfg(unix)]
fn run_native_script(runtime: &Runtime, body: &str, timeout: Duration) -> Result<()> {
    let script = format!("set -eu\nbinary=\"$HOME/{REMOTE_BIN}\"\n{NATIVE_GUEST_CHECK}\n{body}");
    let (code, output) = runtime.ssh_capture(
        &format!("sh -c {}", crate::util::shell_quote(&script)),
        timeout,
    );
    if code == 0 {
        Ok(())
    } else {
        let detail = output.trim();
        if detail.is_empty() {
            return message("The guest native CLI services have not opened all three channels");
        }
        message(format!("Guest native CLI channels (exit {code}): {detail}"))
    }
}

/// Bootstrap the unprivileged native lanes on a boot that has the new QEMU
/// devices. This uses the existing trusted provisioning connection; no CLI
/// request or project payload is sent through it.
#[cfg(unix)]
pub fn prepare_native(runtime: &Runtime, restart: bool) -> Result<()> {
    let script = format!(
        r#"
uid=$(id -u); gid=$(id -g)
for lane in exec sync watch; do
  device="/dev/virtio-ports/org.cabin.$lane"
  target=$(native_device "$lane")
  sudo -n chown "$uid:$gid" "$target"
  sudo -n chmod 0600 "$target"
  unit="cabind-native-$lane.service"
  if [ "$(systemctl --user show --property=LoadState --value "$unit")" = loaded ]; then
    if [ '{restart}' = 'true' ] || ! native_lane_ready "$lane"; then
      systemctl --user restart "$unit"
    fi
  else
    systemd-run --user --quiet --collect --unit="$unit" --property=Type=exec --property=Restart=on-failure \
      "$binary" native-channel "$lane" "$device"
  fi
done
for attempt in 1 2 3 4 5 6 7 8 9 10; do
  if native_ready; then exit 0; fi
  sleep 0.1
done
native_ready
"#
    );
    run_native_script(runtime, &script, Duration::from_secs(20))
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn qmp_must_report_every_native_socket_for_the_current_vm() {
        use serde_json::json;

        let dir = Path::new("/state/current/cli");
        let mut devices = json!([
            {"label": "cabin-exec", "filename": "disconnected:unix:/state/current/cli/exec.sock,server=on"},
            {"label": "cabin-sync", "filename": "unix:/state/current/cli/sync.sock,server=on"},
            {"label": "cabin-watch", "filename": "disconnected:unix:/state/current/cli/watch.sock,server=on"}
        ]);
        assert!(has_native_chardevs(&devices, dir));
        devices[2]["filename"] = json!("unix:/state/old/cli/watch.sock,server=on");
        assert!(!has_native_chardevs(&devices, dir));
        devices.as_array_mut().unwrap().pop();
        assert!(!has_native_chardevs(&devices, dir));
    }

    #[test]
    fn interrupted_deploy_preserves_the_installed_binary_and_removes_staging_file() {
        let home = crate::sockets::test_dir();
        let installed = home.join(REMOTE_BIN);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"working build").unwrap();
        // A partial upload arrives at EOF after SSH drops. Simulate a checksum
        // failure for it, including on hosts with no GNU sha256sum.
        let script = format!(
            "sha256sum() {{ cat >/dev/null; return 1; }}\n{}",
            deploy_script("expected")
        );
        let args = vec!["sh".into(), "-c".into(), script];
        let env = vec![("HOME".into(), home.to_string_lossy().into_owned())];
        let output = crate::util::capture_env(
            &args,
            Duration::from_secs(5),
            Some("partial"),
            None,
            Some(&env),
        );
        assert_eq!(output.code, 1, "{}", output.output);
        assert_eq!(std::fs::read(&installed).unwrap(), b"working build");
        assert_eq!(
            std::fs::read_dir(installed.parent().unwrap())
                .unwrap()
                .count(),
            1
        );
        std::fs::remove_dir_all(home).unwrap();
    }
}
