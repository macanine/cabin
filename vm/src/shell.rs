//! Shell and guest-Chrome operations used by the desktop UI.
//!
//! These helpers deliberately live in `vmctl-core`: the Tauri IPC layer only
//! forwards arguments/results.  No QEMU argv or guest shell script is assembled
//! in the frontend or in a Tauri command.

use base64::Engine;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Result;
use crate::config::{Config, default_project_root, load_instance_config};
use crate::display::DisplayGeometry;
use crate::journal::load_state;
use crate::lock::FileLock;
use crate::paths::InstancePaths;
use crate::util::{message, shell_quote};
use crate::vm::Runtime;
use crate::{agentd, browser};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum NativeDisplayRequest {
    Status,
    Apply {
        width: u32,
        height: u32,
        scale: f64,
        zoom: u32,
    },
    Restore,
}

impl NativeDisplayRequest {
    fn changing(&self) -> bool {
        !matches!(self, Self::Status)
    }

    fn validate(&self) -> Result<()> {
        let Self::Apply {
            width,
            height,
            scale,
            zoom,
        } = self
        else {
            return Ok(());
        };
        DisplayGeometry::new(*width, *height, *scale)?;
        if ![100, 125, 150, 175, 200].contains(zoom) {
            return message("The requested display zoom is unsupported.");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ShellResult {
    pub code: i32,
    pub output: String,
}

fn config_for(home: &Path, name: &str) -> Result<Config> {
    load_instance_config(&default_project_root(), home, name)
}

fn truncate_output(mut output: String) -> String {
    const LIMIT: usize = 200_000;
    if output.len() > LIMIT {
        let mut boundary = LIMIT;
        while !output.is_char_boundary(boundary) {
            boundary -= 1;
        }
        output.truncate(boundary);
        output.push_str("\n... (output truncated)");
    }
    output
}

fn require_running(cfg: &Config) -> Result<Runtime> {
    let runtime = Runtime::new(cfg);
    if !runtime.running() {
        return message(format!(
            "instance {:?} is not running; start it first",
            cfg.name
        ));
    }
    Ok(runtime)
}

fn browser_data(cfg: &Config) -> Option<Value> {
    if cfg.browser_enabled() {
        browser::resolve(cfg).ok()
    } else {
        None
    }
}

/// Report desktop/Chrome/browser state using step state plus a live guest probe.
pub fn browser_status(home: &Path, name: &str) -> Result<Value> {
    let cfg = config_for(home, name)?;
    let desktop = load_state(&cfg.home, &cfg.name, "desktop").unwrap_or(Value::Null);
    let browser_state = load_state(&cfg.home, &cfg.name, "browser").unwrap_or(Value::Null);
    let data = browser_data(&cfg);
    let start_url = data
        .as_ref()
        .and_then(|value| value.get("start_url"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let user_data_dir = data
        .as_ref()
        .and_then(|value| value.get("user_data_dir"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let runtime = Runtime::new(&cfg);
    let running = runtime.running();
    let probe = if running && cfg.ssh_key_path().exists() {
        agentd::native_display(
            &runtime,
            &json!({"action": "browser_status"}),
            Duration::from_secs(10),
        )?
    } else {
        json!({
            "chrome_installed": false,
            "chrome_version": null,
            "controller_running": false,
            "chrome_running": false,
            "viewer_ready": null,
        })
    };
    Ok(json!({
        "desktop_env": cfg.desktop_env,
        "desktop_applied": desktop.get("status").and_then(Value::as_str) == Some("applied"),
        "browser_enabled": cfg.browser_enabled(),
        "browser_applied": browser_state.get("status").and_then(Value::as_str) == Some("applied"),
        "chrome_requested": cfg.desktop_chrome,
        "vm_running": running,
        "chrome_installed": probe["chrome_installed"],
        "chrome_version": probe["chrome_version"],
        "controller_running": probe["controller_running"],
        "chrome_running": probe["chrome_running"],
        "viewer_ready": probe["viewer_ready"],
        "start_url": start_url,
        "user_data_dir": user_data_dir,
    }))
}

fn wayland_prefix() -> String {
    "uid=$(id -u)\nexport XDG_RUNTIME_DIR=\"${XDG_RUNTIME_DIR:-/run/user/$uid}\"\nexport DISPLAY=\"${DISPLAY:-:0}\"\nif [ -z \"${WAYLAND_DISPLAY:-}\" ]; then\n  WAYLAND_DISPLAY=$(basename \"$(ls \"$XDG_RUNTIME_DIR\"/wayland-* 2>/dev/null | head -1)\" 2>/dev/null || true)\n  export WAYLAND_DISPLAY\nfi\nif [ -z \"${SWAYSOCK:-}\" ]; then\n  SWAYSOCK=$(ls \"$XDG_RUNTIME_DIR\"/sway-ipc.* 2>/dev/null | head -1)\n  export SWAYSOCK\nfi\n".to_string()
}

/// Inspect or change the guest's native display, with guest-side rollback.
pub fn browser_native_display(home: &Path, name: &str, request: Value) -> Result<Value> {
    let cfg = config_for(home, name)?;
    if cfg.desktop_env.as_deref() != Some("sway") || !cfg.browser_enabled() {
        return message("Native display scaling requires the managed Sway desktop and Chrome.");
    }
    let request: NativeDisplayRequest = serde_json::from_value(request).map_err(|error| {
        crate::Error::Message(format!("Invalid native display request: {error}"))
    })?;
    request.validate()?;
    let changing = request.changing();
    let payload = serde_json::to_value(&request)?;
    let runtime = require_running(&cfg)?;
    let _lock = if changing {
        Some(FileLock::acquire(
            &InstancePaths::new(cfg.instance_dir()).browser_lock(),
        )?)
    } else {
        None
    };
    agentd::native_display(
        &runtime,
        &payload,
        Duration::from_secs(if changing { 300 } else { 10 }),
    )
}

/// Keep a system-synchronized browser profile aligned after the guest's time
/// zone or language changes. The display settings in the guest profile are
/// preserved, including the restore baseline used by native scaling.
pub fn browser_follow_environment(
    home: &Path,
    name: &str,
    timezone: &str,
    language: &str,
) -> Result<bool> {
    let cfg = config_for(home, name)?;
    if !cfg.browser_enabled()
        || cfg.browser.get("sync_with_system").and_then(Value::as_bool) != Some(true)
    {
        return Ok(false);
    }
    let runtime = require_running(&cfg)?;
    let _lock = FileLock::acquire(&InstancePaths::new(cfg.instance_dir()).browser_lock())?;
    let mut updated = cfg.clone();
    let browser = updated
        .browser
        .as_object_mut()
        .ok_or_else(|| crate::Error::Message("The browser profile is not an object".into()))?;
    browser.insert("timezone".into(), json!(timezone));
    browser.insert("locale".into(), json!(language));
    apply_browser_region(&runtime, &cfg, &updated)
}

/// Apply a time zone / language change to the running guest's browser
/// profile in place (no reinstall) and save `updated`. Only the region
/// fields may differ between `current` and `updated`.
///
/// The browser step records a fingerprint of the whole profile, region
/// included; a start re-applies the step when it no longer matches. The
/// guest was just updated in place, so the record is moved along with it,
/// otherwise the next start would reinstall the browser for nothing (and a
/// failed reinstall would keep the machine from starting).
pub(crate) fn apply_browser_region(
    runtime: &Runtime,
    current: &Config,
    updated: &Config,
) -> Result<bool> {
    let configured_changed = current.browser != updated.browser;
    let before = browser::fingerprint(current)?;
    let profile = browser::install_profile(&browser::resolve(updated)?);
    let response = agentd::native_display(
        runtime,
        &json!({
            "action": "sync_environment",
            "environment": {
                "timezone": profile["timezone"],
                "locale": profile["locale"],
                "language": profile["navigator"]["language"],
                "languages": profile["navigator"]["languages"],
                "chrome_language": profile["chrome"]["lang"],
            },
        }),
        Duration::from_secs(240),
    )?;
    let profile_changed = response["changed"].as_bool().ok_or_else(|| {
        crate::Error::Message("Invalid guest browser environment response".into())
    })?;
    if profile_changed || configured_changed {
        updated.save_instance_meta()?;
        crate::steps::browser::record_live_change(updated, &before)?;
    }
    Ok(profile_changed || configured_changed)
}

/// Paste UTF-8 text into the focused guest application via its Wayland clipboard.
pub fn browser_paste(home: &Path, name: &str, text: Option<&str>) -> Result<()> {
    let cfg = config_for(home, name)?;
    let runtime = require_running(&cfg)?;
    let text = match text {
        Some(text) => text.to_string(),
        None => {
            #[cfg(unix)]
            let output = std::process::Command::new("/usr/bin/pbpaste").output()?;
            #[cfg(not(unix))]
            let output = {
                let mut command = std::process::Command::new("powershell");
                command.args(["-NoProfile", "-Command", "Get-Clipboard"]);
                crate::util::no_window(&mut command);
                command.output()?
            };
            if !output.status.success() {
                return message("Could not read clipboard");
            }
            String::from_utf8(output.stdout)
                .map_err(|_| crate::Error::Message("Clipboard is not text".into()))?
        }
    };
    if text.is_empty() {
        return Ok(());
    }
    if text.len() > 32768 {
        return message("Text is too large");
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let need_cjk = text
        .chars()
        .any(|ch| ('\u{2e80}'..='\u{9fff}').contains(&ch));
    let apt_proxy = cfg
        .guest_proxy_url()
        .map(|proxy| {
            format!(
                " -o {} -o {}",
                shell_quote(&format!("Acquire::http::Proxy={proxy}")),
                shell_quote(&format!("Acquire::https::Proxy={proxy}"))
            )
        })
        .unwrap_or_default();
    let script = format!(
        "set -eu\n{}if [ -z \"${{WAYLAND_DISPLAY:-}}\" ]; then echo 'Desktop is not ready' >&2; exit 1; fi\nif ! command -v wl-copy >/dev/null || ! command -v wtype >/dev/null || {{ [ {need_cjk} = true ] && ! fc-list :lang=zh | grep -q .; }}; then sudo -n apt-get{apt_proxy} update && sudo -n env DEBIAN_FRONTEND=noninteractive apt-get{apt_proxy} install -y wl-clipboard wtype fonts-noto-cjk >/dev/null; fi\nprintf '%s' {encoded_q} | base64 -d | wl-copy --type 'text/plain;charset=utf-8' >/dev/null 2>&1\nchrome_focused=0\ni=0\nwhile [ \"$i\" -lt 20 ]; do\n  if command -v swaymsg >/dev/null 2>&1 && [ -n \"${{SWAYSOCK:-}}\" ]; then\n    chrome_focused=$(swaymsg -t get_tree 2>/dev/null | {helper} guest-desktop focused 2>/dev/null || printf 2)\n  fi\n  [ \"$chrome_focused\" = 1 ] && break\n  sleep 0.1\n  i=$((i + 1))\ndone\nif [ \"$chrome_focused\" = 1 ] && [ -x {helper} ] && pgrep -u \"$uid\" -f '^/opt/google/chrome/chrome( |$)' >/dev/null 2>&1; then\n  if {helper} browser paste-base64 {encoded_q} >/dev/null 2>&1; then\n    exit 0\n  fi\nfi\nwtype -M ctrl -k v -m ctrl\n",
        wayland_prefix(),
        helper = shell_quote(crate::agentd::HELPER_BIN),
        encoded_q = shell_quote(&encoded)
    );
    let (code, output) = runtime.ssh_capture(&script, std::time::Duration::from_secs(300));
    if code != 0 {
        return message(format!("Paste failed: {}", output.trim()));
    }
    Ok(())
}

fn chrome_flag(data: &Option<Value>) -> String {
    let Some(user_data_dir) = data
        .as_ref()
        .and_then(|value| value.get("user_data_dir"))
        .and_then(Value::as_str)
    else {
        return String::new();
    };
    format!(" --user-data-dir={}", shell_quote(user_data_dir))
}

/// Open (or focus) Chrome in the guest and navigate to `url`.
pub fn browser_open(home: &Path, name: &str, url: &str) -> Result<ShellResult> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://") || url == "about:blank") {
        return message("url must be an http(s) URL or about:blank");
    }
    let cfg = config_for(home, name)?;
    if cfg.desktop_env.is_none() {
        return message("this instance has no desktop; install it with --desktop sway");
    }
    if !cfg.desktop_chrome && !cfg.browser_enabled() {
        return message("this desktop was not installed with Chrome (--chrome)");
    }
    let runtime = require_running(&cfg)?;
    let _lock = FileLock::acquire(&InstancePaths::new(cfg.instance_dir()).browser_lock())?;
    let data = browser_data(&cfg);
    let user_data = chrome_flag(&data);
    let quoted_url = shell_quote(url);
    let script = if cfg.browser_enabled() {
        format!(
            r#"set -eu
{prefix}
status_path="${{XDG_RUNTIME_DIR}}/vmctl-fp-status.json"
chrome_count() {{ ps -u "$uid" -o args= | awk '$1 == "/opt/google/chrome/chrome" && $0 !~ / --type=/ {{ count++ }} END {{ print count + 0 }}'; }}
controller_count() {{ pgrep -u "$uid" -f '^{helper} browser run$' 2>/dev/null | wc -l | tr -d ' '; }}
if [ "$(controller_count)" -eq 0 ]; then
  rm -f "$status_path"
  nohup {helper} browser run </dev/null >"$XDG_RUNTIME_DIR/cabin-chrome-start.log" 2>&1 &
fi
i=0
while [ "$i" -lt 60 ]; do
  [ "$(chrome_count)" -eq 1 ] && break
  sleep 0.5
  i=$((i + 1))
done
[ "$(chrome_count)" -eq 1 ] || {{ echo 'Chrome did not start or has duplicate main processes' >&2; exit 5; }}
i=0
while [ "$i" -lt 120 ]; do
  if {helper} browser ready 2>/dev/null; then break; fi
  sleep 0.25
  i=$((i + 1))
done
{helper} browser ready 2>/dev/null || {{
  echo 'Browser controller did not become ready' >&2
  [ ! -s "$status_path" ] || cat "$status_path" >&2
  exit 6
}}
[ "$(controller_count)" -eq 1 ] || {{ echo 'Browser controller is not singular' >&2; exit 7; }}
swaymsg -q '[app_id="(?i)^(google-chrome|chrome)(-stable)?$"] focus' >/dev/null 2>&1 || true
swaymsg -q '[class="(?i)^google-chrome(-stable)?$"] focus' >/dev/null 2>&1 || true
{helper} browser navigate {quoted_url}
printf 'opened %s\n' {quoted_url}
"#,
            prefix = wayland_prefix(),
            helper = shell_quote(crate::agentd::HELPER_BIN),
            quoted_url = quoted_url
        )
    } else {
        format!(
            "set -eu\n{}if [ -z \"${{WAYLAND_DISPLAY:-}}\" ]; then echo 'Wayland display not found; is Sway running?' >&2; exit 3; fi\nif ! pgrep -u \"$uid\" -f '^/opt/google/chrome/chrome( |$)' >/dev/null 2>&1; then\n  nohup google-chrome-stable --ozone-platform=x11 --disable-quic{} --new-window {quoted_url} >/tmp/vmctl-chrome-open.log 2>&1 &\nelse\n  nohup google-chrome-stable --ozone-platform=x11 --disable-quic{} --new-tab {quoted_url} >/tmp/vmctl-chrome-open.log 2>&1 &\nfi\nprintf 'opened %s\\n' {quoted_url}\n",
            wayland_prefix(),
            user_data,
            user_data,
            quoted_url = quoted_url
        )
    };
    let (code, output) = runtime.ssh_capture(&script, std::time::Duration::from_secs(75));
    Ok(ShellResult {
        code,
        output: truncate_output(output),
    })
}

const CHROME_STOP: &str = r#"set -eu
uid=$(id -u)
wrappers='^(/bin/sh|/bin/bash) (-c )?/home/[^/]+/\.config/sway/start-chrome\.sh$'
managed='^(/usr/local/libexec/cabin/cabind browser run$|/usr/bin/python3 /opt/vmctl-fp/controller\.py$|/opt/google/chrome/(chrome|chrome_crashpad_handler)( |$))'
stop_matching() {
  pids=$(pgrep -u "$uid" -f "$1" || true)
  [ -z "$pids" ] || kill -"$2" $pids 2>/dev/null || true
}
stop_matching "$wrappers" KILL
stop_matching "$managed" TERM
for i in $(seq 1 50); do
  if ! pgrep -u "$uid" -f "$managed" >/dev/null; then
    rm -f "/run/user/$uid/vmctl-fp-status.json"
    exit 0
  fi
  [ "$i" -ne 30 ] || stop_matching "$managed" KILL
  sleep 0.2
done
echo 'Chrome processes did not stop' >&2
exit 1
"#;

/// Remove the managed autostart entry from older installations, keeping other Sway settings.
pub fn migrate_chrome_autostart(runtime: &Runtime) -> Result<()> {
    if !runtime.cfg.desktop_chrome {
        return Ok(());
    }
    let _lock = FileLock::acquire(&InstancePaths::new(runtime.cfg.instance_dir()).browser_lock())?;
    let script = r#"set -eu
config="$HOME/.config/sway/config"
entry="exec $HOME/.config/sway/start-chrome.sh"
[ -f "$config" ] && grep -Fxq -- "$entry" "$config" || exit 0
"#;
    // Stop first so a failed cleanup leaves the migration retryable.
    let script = format!(
        "{script}if ! (\n{CHROME_STOP}\n); then exit 1; fi\ntmp=$(mktemp \"$config.cabin.XXXXXX\")\ntrap 'rm -f -- \"$tmp\"' EXIT\nawk -v entry=\"$entry\" '$0 != entry' \"$config\" >\"$tmp\"\nchmod --reference=\"$config\" \"$tmp\"\nmv -f -- \"$tmp\" \"$config\"\n"
    );
    let (code, output) = runtime.ssh_capture(&script, Duration::from_secs(20));
    if code != 0 {
        return message(format!(
            "Could not remove legacy Chrome autostart: {}",
            output.trim()
        ));
    }
    Ok(())
}

/// Stop the managed Chrome/controller in the guest.
pub fn browser_stop(home: &Path, name: &str) -> Result<ShellResult> {
    let cfg = config_for(home, name)?;
    if cfg.desktop_env.is_none() {
        return message("this instance has no desktop");
    }
    let runtime = require_running(&cfg)?;
    let _lock = FileLock::acquire(&InstancePaths::new(cfg.instance_dir()).browser_lock())?;
    let (code, output) = runtime.ssh_capture(CHROME_STOP, Duration::from_secs(20));
    Ok(ShellResult {
        code,
        output: truncate_output(output),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::NativeDisplayRequest;

    #[test]
    fn truncated_shell_output_preserves_utf8_boundaries() {
        let short = "short output".to_string();
        assert_eq!(super::truncate_output(short.clone()), short);
        let output = format!("{}界more", "a".repeat(199_999));
        let truncated = super::truncate_output(output);
        assert_eq!(
            truncated,
            format!("{}\n... (output truncated)", "a".repeat(199_999))
        );
    }

    #[test]
    fn native_display_requests_are_typed_and_validated() {
        let apply: NativeDisplayRequest = serde_json::from_value(json!({
            "action": "apply",
            "width": 1920,
            "height": 1080,
            "scale": 2.0,
            "zoom": 125
        }))
        .unwrap();
        apply.validate().unwrap();
        assert!(apply.changing());

        let status: NativeDisplayRequest =
            serde_json::from_value(json!({"action": "status"})).unwrap();
        status.validate().unwrap();
        assert!(!status.changing());

        let invalid: NativeDisplayRequest = serde_json::from_value(json!({
            "action": "apply",
            "width": 1920,
            "height": 1080,
            "scale": 0,
            "zoom": 125
        }))
        .unwrap();
        assert!(invalid.validate().is_err());
        assert!(
            serde_json::from_value::<NativeDisplayRequest>(json!({"action": "other"})).is_err()
        );
    }
}
