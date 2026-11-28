//! Post-provision desktop stack: Wayland (Sway/Waybar/Foot/Wofi) + Chrome.
//!
//! Normally Sway runs headless. Opting into SPICE moves the same Sway session
//! to virtio-gpu/DRM with the software renderer. Both use wayvnc on the guest
//! loopback so VNC stays available on the desktop's current output.

use base64::Engine;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::config::Config;
use crate::util::{message, py_g, shell_quote as q};

pub use crate::display::WAYBAR_HEIGHT;

const SWAY_PACKAGES: &[&str] = &[
    "sway",
    "swaybg",
    "swayidle",
    "swaylock",
    "waybar",
    "foot",
    "wofi",
    "xwayland",
    "xdg-desktop-portal-gtk",
    "grim",
    "slurp",
    "wl-clipboard",
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    "fonts-dejavu-core",
    "fonts-noto-color-emoji",
    "fonts-noto-cjk",
    "fonts-liberation",
    "wtype",
    "locales",
    "curl",
    "ca-certificates",
    "dbus-x11",
    "wayvnc",
];

/// Frames per second the in-app display may reach (wayvnc's default is 30).
const DISPLAY_MAX_FPS: u32 = 60;
const WAYVNC_SUPERVISOR: &str = "/usr/local/libexec/cabin-wayvnc";
const WAYVNC_LAUNCH: &str = "exec /usr/local/libexec/cabin-wayvnc";
const SPICE_DROP_IN: &str = "/etc/systemd/system/sway-autologin.service.d/cabin-spice.conf";
const SPICE_SWAY: &str = "/usr/local/libexec/cabin-sway-drm";

// Spawned by Sway to inherit the matching WAYLAND_DISPLAY and SWAYSOCK. The
// lock also lets a live migration supervise an already running legacy server.
fn wayvnc_supervisor() -> String {
    format!(
        r#"#!/bin/sh
set -eu
: "${{XDG_RUNTIME_DIR:?}}" "${{SWAYSOCK:?}}" "${{WAYLAND_DISPLAY:?}}"
exec 9>"$XDG_RUNTIME_DIR/cabin-wayvnc.lock"
flock -w 5 9 || exit 0
sway_pid="${{SWAYSOCK%.sock}}"
sway_pid="${{sway_pid##*.}}"
child=
trap 'if [ -n "$child" ]; then kill "$child" 2>/dev/null || true; fi; exit 0' TERM INT HUP
while [ -S "$SWAYSOCK" ] && kill -0 "$sway_pid" 2>/dev/null; do
    if ! ss -Hln 'sport = :5900' | grep -q '127\.0\.0\.1:5900'; then
        /usr/bin/wayvnc --max-fps={DISPLAY_MAX_FPS} 127.0.0.1 5900 9>&- &
        child=$!
        wait "$child" || true
        child=
    fi
    sleep 1
done
"#
    )
}

fn write_supervisor() -> String {
    format!(
        "install -d /usr/local/libexec\nprintf '%s' {} | base64 -d | install -m 0755 /dev/stdin {WAYVNC_SUPERVISOR}\n",
        q(&b64(&wayvnc_supervisor()))
    )
}

pub fn enabled(cfg: &Config) -> bool {
    cfg.desktop_env.as_deref() == Some("sway")
}

pub fn packages(cfg: &Config) -> Vec<String> {
    if !enabled(cfg) {
        return Vec::new();
    }
    let mut base: Vec<String> = SWAY_PACKAGES
        .iter()
        .map(|item| (*item).to_string())
        .collect();
    base.extend(cfg.desktop_extra_packages.iter().cloned());
    base.sort();
    base.dedup();
    base
}

pub fn sway_config(cfg: &Config) -> String {
    format!(
        "# managed by vmctl desktop step\nset $mod Mod4\nfont pango:DejaVu Sans 10\nxwayland enable\noutput * bg #1e1e2e solid_color\noutput Virtual-1 resolution {}\noutput Virtual-1 scale {}\nexec_always waybar\n\nbindsym $mod+Return exec foot\nbindsym $mod+d exec wofi --show drun\nbindsym $mod+Shift+q kill\nbindsym $mod+Shift+e exec swaynag -t warning -m 'Exit Sway?' -B 'Yes' 'swaymsg exit'\nbindsym $mod+Shift+r reload\nbindsym $mod+Shift+f fullscreen toggle\ninput * {{\n    xkb_layout us\n    tap enabled\n}}\n",
        cfg.desktop_resolution,
        py_g(cfg.desktop_scale)
    )
}

pub fn waybar_config() -> String {
    let value = json!({
        "layer": "top",
        "position": "top",
        "height": WAYBAR_HEIGHT,
        "modules-left": ["sway/workspaces"],
        "modules-center": ["sway/window"],
        "modules-right": ["pulseaudio", "clock"],
        "clock": {"format": "%Y-%m-%d %H:%M"},
        "pulseaudio": {"format": "{icon} {volume}%", "format-muted": "muted"},
    });
    format!("{}\n", crate::py_json::dumps_pretty_with(&value, false))
}

pub fn waybar_style() -> String {
    "* { font-family: \"DejaVu Sans\"; font-size: 12px; }\nwindow#waybar { background: #11111b; color: #cdd6f4; }\n#workspaces button { padding: 0 8px; color: #cdd6f4; }\n#workspaces button.focused { background: #313244; }\n#clock, #pulseaudio { padding: 0 10px; }\n".to_string()
}

fn chrome_script(cfg: &Config) -> String {
    let proxy = cfg
        .guest_proxy_url()
        .map(|value| format!(" --proxy-server={value}"))
        .unwrap_or_default();
    format!(
        "#!/bin/sh\nexport DISPLAY=\"${{DISPLAY:-:0}}\"\nexec google-chrome-stable --ozone-platform=x11 --no-first-run --no-default-browser-check --disable-dev-shm-usage --disable-quic --disable-features=Translate --password-store=basic{proxy}\n"
    )
}

fn autologin_service(cfg: &Config) -> String {
    let user = cfg.current_username();
    let home = format!("/home/{user}");
    let headless = if cfg.gpu_mode == "none" {
        "Environment=WLR_BACKENDS=headless\nEnvironment=WLR_HEADLESS_OUTPUTS=1\nEnvironment=WLR_LIBINPUT_NO_DEVICES=1\n"
    } else {
        ""
    };
    format!(
        "[Unit]\nDescription=vmctl Sway autologin on tty1\nAfter=systemd-user-sessions.service\nConflicts=getty@tty1.service\n[Service]\nUser={user}\nPAMName=login\nTTYPath=/dev/tty1\nTTYReset=yes\nTTYVHangup=yes\nTTYVTDisallocate=yes\nWorkingDirectory={home}\nEnvironment=XDG_SESSION_TYPE=wayland\nEnvironment=XDG_CURRENT_DESKTOP=sway\nEnvironment=MOZ_ENABLE_WAYLAND=1\nEnvironment=WLR_RENDERER=pixman\nEnvironment=LIBGL_ALWAYS_SOFTWARE=1\n{headless}ExecStart=/usr/bin/sway\nRestart=always\nRestartSec=2\n[Install]\nWantedBy=graphical.target\n"
    )
}

pub fn install_script(cfg: &Config) -> Result<String> {
    if !enabled(cfg) {
        return message("desktop is not enabled with the supported Sway profile");
    }
    let pkgs: Vec<String> = packages(cfg).iter().map(|item| q(item)).collect();
    let user = cfg.current_username();
    let home = format!("/home/{user}");
    let mut sway = sway_config(cfg);
    if cfg.gpu_mode == "none" {
        sway.push_str(&format!(
            "output HEADLESS-1 resolution {}\noutput HEADLESS-1 scale {}\n",
            cfg.desktop_resolution,
            py_g(cfg.desktop_scale)
        ));
    }
    sway.push_str(WAYVNC_LAUNCH);
    sway.push('\n');
    let supervisor = write_supervisor();
    let waybar = waybar_config();
    let style = waybar_style();
    let chrome_script = chrome_script(cfg);
    let autologin = autologin_service(cfg);
    let chrome_install = if cfg.desktop_chrome {
        format!(
            "\nsay 'installing Google Chrome ARM64'\ncurl --retry 5 --retry-delay 3 --connect-timeout 20 -fsSL -o /tmp/google-chrome.deb {}\nsize=$(stat -c %s /tmp/google-chrome.deb 2>/dev/null || echo 0)\n[ \"$size\" -gt 50000000 ] || {{ echo \"Chrome deb is too small: $size\"; exit 31; }}\nfor i in 1 2 3; do\n  if apt-get install -y --no-install-recommends /tmp/google-chrome.deb; then break; fi\n  [ \"$i\" = 3 ] && exit 32\n  heal_dpkg\n  sleep 5\ndone\ngoogle-chrome-stable --version\nrm -f /tmp/google-chrome.deb\n",
            q(&cfg.desktop_chrome_url)
        )
    } else {
        String::new()
    };
    let extra = cfg.desktop_extra_commands.join("\n");
    // Ubuntu 24.04 splits most drivers into linux-modules-extra.
    let kernel_modules = cfg
        .guest_os()
        .extra_modules_package()
        .map(|package| {
            format!(
                "say 'installing kernel graphics modules'\nfor i in 1 2 3; do\n  if apt-get install -y --no-install-recommends {package}; then break; fi\n  [ \"$i\" = 3 ] && exit 23\n  heal_dpkg\n  sleep 5\ndone\n"
            )
        })
        .unwrap_or_default();
    let user_start = format!(
        "\nsay 'writing Sway/Waybar/Chrome configuration'\ninstall -d -o {user} -g {user} {} {}\nprintf '%s' {} | base64 -d > {}\nprintf '%s' {} | base64 -d > {}\nprintf '%s' {} | base64 -d > {}\nprintf '%s' {} | base64 -d > {}\nchmod 755 {}\nchown -R {user}:{user} {}\nsay 'configuring graphical autologin'\nprintf '%s' {} | base64 -d > /etc/systemd/system/sway-autologin.service\nsystemctl daemon-reload\nsystemctl set-default graphical.target\nsystemctl disable --now getty@tty1.service 2>/dev/null || true\nsystemctl enable sway-autologin.service\n",
        q(&format!("{home}/.config/sway")),
        q(&format!("{home}/.config/waybar")),
        q(&b64(&sway)),
        q(&format!("{home}/.config/sway/config")),
        q(&b64(&waybar)),
        q(&format!("{home}/.config/waybar/config")),
        q(&b64(&style)),
        q(&format!("{home}/.config/waybar/style.css")),
        q(&b64(&chrome_script)),
        q(&format!("{home}/.config/sway/start-chrome.sh")),
        q(&format!("{home}/.config/sway/start-chrome.sh")),
        q(&format!("{home}/.config")),
        q(&b64(&autologin)),
        user = q(&user),
    );
    Ok(format!(
        "set -euo pipefail\nexport DEBIAN_FRONTEND=noninteractive\nsay() {{ printf '\\n[desktop] %s\\n' \"$*\"; }}\nUSER_NAME={user}\nHOME_DIR={home_dir}\nwait_apt_locks() {{\n  for i in $(seq 1 60); do\n    if ! fuser /var/lib/dpkg/lock-frontend /var/lib/dpkg/lock /var/cache/apt/archives/lock >/dev/null 2>&1; then break; fi\n    sleep 2\n  done\n}}\nheal_dpkg() {{\n  wait_apt_locks\n  dpkg --configure -a --force-confdef --force-confold || true\n  apt-get install -f -y --no-install-recommends || true\n}}\nsay 'waiting for apt/dpkg locks and checking package state'\nheal_dpkg\nsay 'updating apt indexes (retry)'\nfor i in 1 2 3; do\n  if apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 update -qq; then break; fi\n  [ \"$i\" = 3 ] && exit 21\n  sleep 5\ndone\nsay 'installing desktop packages'\nfor i in 1 2 3; do\n  if apt-get install -y --no-install-recommends {}; then break; fi\n  [ \"$i\" = 3 ] && exit 22\n  heal_dpkg\n  apt-get -o Acquire::Retries=5 -o Acquire::http::Timeout=30 update -qq || true\n  sleep 5\ndone\n{kernel_modules}modprobe virtio_gpu 2>/dev/null || true\n{chrome_install}\nusermod -aG audio,video,render,netdev \"$USER_NAME\" 2>/dev/null || true\n{supervisor}{user_start}\n{extra}\nsay 'desktop stack ready'\n",
        pkgs.join(" "),
        user = q(&user),
        home_dir = q(&home),
    ))
}

fn migrate_wayvnc_script(cfg: &Config) -> String {
    let user = q(&cfg.current_username());
    let supervisor = write_supervisor();
    format!(
        r#"set -euo pipefail
{supervisor}
runuser -u {user} -- sh -s <<'MIGRATE'
set -eu
file="$HOME/.config/sway/config"
legacy='exec /usr/bin/wayvnc --max-fps={DISPLAY_MAX_FPS} 127.0.0.1 5900'
if [ -f "$file" ] && [ ! -L "$file" ] && grep -Fqx -- "$legacy" "$file"; then
    staged=$(mktemp "$file.XXXXXX")
    trap 'rm -f "$staged"' EXIT
    awk -v old="$legacy" -v new='{WAYVNC_LAUNCH}' '{{print $0 == old ? new : $0}}' "$file" > "$staged"
    chmod --reference="$file" "$staged"
    mv -f -- "$staged" "$file"
fi
MIGRATE
uid=$(id -u {user})
pid=$(systemctl show -p MainPID --value sway-autologin.service)
xrd="/run/user/$uid"
socket="$xrd/sway-ipc.$uid.$pid.sock"
if [ -S "$socket" ]; then
    runuser -u {user} -- env XDG_RUNTIME_DIR="$xrd" swaymsg -q -s "$socket" exec {WAYVNC_SUPERVISOR}
fi
"#
    )
}

pub(crate) fn ensure_wayvnc(cfg: &Config, runtime: &crate::vm::Runtime) -> Result<()> {
    let healthy = r#"lock="/run/user/$(id -u)/cabin-wayvnc.lock"
[ -x /usr/local/libexec/cabin-wayvnc ] && [ -f "$lock" ] || exit 1
flock -n -E 3 "$lock" true
[ "$?" -eq 3 ] && ss -Hln 'sport = :5900' | grep -q '127\.0\.0\.1:5900'"#;
    if runtime
        .ssh_capture(healthy, std::time::Duration::from_secs(5))
        .0
        == 0
    {
        return Ok(());
    }
    let _lock = crate::lock::FileLock::acquire(
        &crate::paths::InstancePaths::new(cfg.instance_dir()).browser_lock(),
    )?;
    let (code, output) = runtime.ssh_script(
        &migrate_wayvnc_script(cfg),
        std::time::Duration::from_secs(20),
    );
    if code != 0 {
        return message(format!(
            "Could not start the desktop's VNC supervisor: {}",
            output.trim()
        ));
    }
    Ok(())
}

pub(crate) fn stage_spice_script() -> String {
    // On an interrupted conversion, the drop-in still starts the previous
    // headless configuration when virtio-gpu is absent.
    let wrapper = r#"#!/bin/sh
for connector in /sys/class/drm/card*-Virtual-1; do
    if [ -d "$connector" ]; then
        unset WLR_BACKENDS WLR_HEADLESS_OUTPUTS WLR_LIBINPUT_NO_DEVICES
        break
    fi
done
exec /usr/bin/sway
"#;
    let service = format!("[Service]\nExecStart=\nExecStart={SPICE_SWAY}\n");
    format!(
        r#"set -euo pipefail
modprobe virtio_gpu
install -d /usr/local/libexec /etc/systemd/system/sway-autologin.service.d
printf '%s' {} | base64 -d | install -m 0755 /dev/stdin {SPICE_SWAY}
printf '%s' {} | base64 -d | install -m 0644 /dev/stdin {SPICE_DROP_IN}
"#,
        q(&b64(wrapper)),
        q(&b64(&service))
    )
}

pub(crate) fn unstage_spice_script() -> String {
    format!("set -eu\nrm -f {SPICE_DROP_IN} {SPICE_SWAY}\nsystemctl daemon-reload\n")
}

const VNC_PROBE: &str = r#"timeout 2 bash -c 'exec 3<>/dev/tcp/127.0.0.1/5900; IFS= read -r banner <&3; [ "$banner" = "RFB 003.008" ]' 2>/dev/null"#;

pub(crate) fn wait_for_vnc(runtime: &crate::vm::Runtime) -> Result<()> {
    let script = format!(
        "for i in $(seq 1 20); do\nif {VNC_PROBE}\nthen exit 0; fi\nsleep 1\ndone\necho 'The desktop did not serve a VNC connection' >&2\nexit 1"
    );
    let (code, output) = runtime.ssh_capture(&script, std::time::Duration::from_secs(45));
    if code != 0 {
        return message(output.trim());
    }
    Ok(())
}

pub(crate) fn drm_output(runtime: &crate::vm::Runtime) -> Result<(u32, u32)> {
    let script = r#"set -eu
uid=$(id -u)
export XDG_RUNTIME_DIR="/run/user/$uid"
pid=$(systemctl show -p MainPID --value sway-autologin.service)
socket="$XDG_RUNTIME_DIR/sway-ipc.$uid.$pid.sock"
test -S "$socket"
swaymsg -r -s "$socket" -t get_outputs"#;
    let (code, output) = runtime.ssh_capture(script, std::time::Duration::from_secs(8));
    if code != 0 {
        return message(format!(
            "Could not read the Sway DRM output: {}",
            output.trim()
        ));
    }
    let outputs: Vec<Value> = serde_json::from_str(&output)?;
    outputs
        .into_iter()
        .find_map(|output| {
            if output["active"] != true || output["name"] != "Virtual-1" {
                return None;
            }
            let mode = &output["current_mode"];
            let width = u32::try_from(mode["width"].as_u64()?).ok()?;
            let height = u32::try_from(mode["height"].as_u64()?).ok()?;
            Some((width, height))
        })
        .ok_or_else(|| crate::Error::Message("Sway has no active virtio DRM output".into()))
}

pub fn verify_script(cfg: &Config) -> String {
    if !enabled(cfg) {
        return "true\n".to_string();
    }
    let user = cfg.current_username();
    let mut checks: Vec<String> = vec![
        "command -v sway".to_string(),
        "command -v waybar".to_string(),
        "command -v foot".to_string(),
        "command -v wofi".to_string(),
        "systemctl is-enabled sway-autologin.service".to_string(),
        format!("test -f /home/{user}/.config/sway/config"),
        "systemctl get-default | grep -qx graphical.target".to_string(),
        format!("id -nG {user} | grep -qw audio"),
    ];
    checks.push(format!(
        "runuser -u {user} -- env XDG_RUNTIME_DIR=/run/user/$(id -u {user}) WLR_BACKENDS=headless WLR_RENDERER=pixman sway -C -c /home/{user}/.config/sway/config"
    ));
    if cfg.desktop_chrome {
        checks.push("command -v google-chrome-stable".to_string());
        checks.push("google-chrome-stable --version".to_string());
    }
    if cfg.gpu_mode != "none" {
        checks.push("test -e /dev/dri/card0 || test -e /sys/class/drm/card0".to_string());
    }
    format!("set -e\n{}\n", checks.join("\n"))
}

pub fn summary(cfg: &Config) -> Value {
    let sway_hash = sha256_text(&sway_config(cfg));
    json!({
        "env": cfg.desktop_env,
        "packages": packages(cfg),
        "chrome": cfg.desktop_chrome,
        "chrome_url": cfg.desktop_chrome_url,
        "display": cfg.display_mode,
        "resolution": cfg.desktop_resolution,
        "scale": cfg.desktop_scale,
        "sway_config": sway_hash,
        "gpu": cfg.gpu_mode,
        "usb_tablet": cfg.usb_tablet,
        "audio": cfg.audio_backend,
        "fingerprint": fingerprint(cfg),
    })
}

pub fn fingerprint(cfg: &Config) -> String {
    let mut payload = Map::new();
    payload.insert("env".to_string(), json!(cfg.desktop_env));
    payload.insert("packages".to_string(), json!(packages(cfg)));
    payload.insert("chrome".to_string(), json!(cfg.desktop_chrome));
    payload.insert("chrome_url".to_string(), json!(cfg.desktop_chrome_url));
    payload.insert(
        "extra_packages".to_string(),
        json!(cfg.desktop_extra_packages),
    );
    payload.insert(
        "extra_commands".to_string(),
        json!(cfg.desktop_extra_commands),
    );
    payload.insert("user".to_string(), json!(cfg.username));
    payload.insert("gpu".to_string(), json!(cfg.gpu_mode));
    payload.insert("display".to_string(), json!(cfg.display_mode));
    payload.insert("resolution".to_string(), json!(cfg.desktop_resolution));
    payload.insert("scale".to_string(), json!(cfg.desktop_scale));
    payload.insert(
        "sway_config".to_string(),
        json!(sha256_text(&sway_config(cfg))),
    );
    payload.insert("audio".to_string(), json!(cfg.audio_backend));
    crate::util::sha256_text(&crate::py_json::dumps_default(
        &Value::Object(payload),
        true,
    ))
}

fn b64(text: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
}

fn sha256_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    crate::util::hex_lower(&hasher.finalize())
}

// ------------------------------------------------------------ apps

/// A graphical app installed in the guest (a `.desktop` entry).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopApp {
    /// The entry's file name, e.g. `google-chrome.desktop`.
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
}

/// Lists visible, non-terminal `.desktop` entries as JSON.
pub fn apps(cfg: &Config) -> Result<Vec<DesktopApp>> {
    let command = format!("{} guest-desktop apps", crate::agentd::HELPER_BIN);
    let (code, output) =
        crate::vm::Runtime::new(cfg).ssh_capture(&command, std::time::Duration::from_secs(20));
    if code != 0 {
        return message(format!(
            "Could not list the desktop's apps: {}",
            output.trim()
        ));
    }
    let json = output
        .lines()
        .rev()
        .find(|line| line.starts_with('['))
        .unwrap_or("[]");
    serde_json::from_str(json)
        .map_err(|error| crate::Error::Message(format!("Invalid app list: {error}")))
}

/// A `.desktop` file name: no paths, no shell syntax.
fn valid_app_id(id: &str) -> bool {
    id.ends_with(".desktop")
        && id.len() <= 200
        && !id.starts_with('.')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

/// Start an app on its own workspace (named after it), so it gets the whole
/// display and Chrome keeps workspace 1.
pub fn launch(cfg: &Config, id: &str) -> Result<()> {
    if !valid_app_id(id) {
        return message("Unknown app");
    }
    let script = format!(
        "{helper} guest-desktop launch {id}",
        helper = q(crate::agentd::HELPER_BIN),
        id = q(id),
    );
    let (code, output) =
        crate::vm::Runtime::new(cfg).ssh_capture(&script, std::time::Duration::from_secs(20));
    if code != 0 {
        return message(format!("Could not start the app: {}", output.trim()));
    }
    Ok(())
}

#[cfg(test)]
mod app_tests {
    use super::*;

    #[test]
    fn only_plain_desktop_file_names_are_launched() {
        assert!(valid_app_id("google-chrome.desktop"));
        assert!(valid_app_id("org.gnome.TextEditor.desktop"));
        for bad in [
            "../x.desktop",
            "a b.desktop",
            "x;rm -rf ~.desktop",
            "chrome",
            ".hidden.desktop",
            "$(id).desktop",
        ] {
            assert!(!valid_app_id(bad), "{bad}");
        }
    }

    #[test]
    fn the_display_is_vnc_at_sixty_frames_on_every_os() {
        for os in crate::os::RELEASES {
            let cfg = crate::config::build_config(
                &json!({"name": "desk", "os": os.id, "desktop_env": "sway", "desktop_chrome": true}),
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            )
            .unwrap();
            let script = install_script(&cfg).unwrap();
            assert!(script.contains("wayvnc"), "{}", os.id);
            assert!(script.contains("dpkg --configure -a"), "{}", os.id);
            assert!(!script.contains("qxl") && !script.contains("policykit-1-gnome"));
            assert_eq!(
                script.contains("linux-modules-extra"),
                os.id == "ubuntu-24.04"
            );
        }
    }
}
