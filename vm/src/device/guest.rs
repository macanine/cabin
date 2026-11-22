//! Guest-side cloud-init additions and the idempotent alignment script.

use base64::Engine;
use serde_json::{Map, Value, json};

use crate::Result;
use crate::config::Config;
use crate::device::render;
use crate::util::shell_quote as q;

fn value_text(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        other => other.to_string(),
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value.and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        None => Vec::new(),
    }
}

fn text<'a>(value: &'a Value, key: &str, default: &'a str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or(default)
}

fn object<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_json(value: &Value) -> String {
    let mut text = crate::py_json::dumps_pretty(value);
    text.push('\n');
    b64(text.as_bytes())
}

pub fn cloud_init_packages(resolved: &Value) -> Vec<String> {
    let guest = object(resolved, "guest");
    let mut packages: Vec<String> = string_list(guest.get("packages"));
    packages.extend(string_list(guest.get("extra_packages")));
    packages.sort();
    packages.dedup();
    packages
}

pub fn cloud_init_write_files(cfg: &Config, resolved: &Value) -> Result<Vec<Value>> {
    let guest = object(resolved, "guest");
    let plan = render::netplan_document(resolved, Some(&cfg.hostname))?;
    let mut files = vec![json!({
        "path": "/etc/netplan/50-vmctl.yaml",
        "permissions": "0600",
        "content": crate::py_json::dumps_pretty(&plan) + "\n",
    })];
    let mut extra_files: Map<String, Value> = guest
        .get("extra_files")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(extra) = resolved
        .get("extra")
        .and_then(|value| value.get("guest_files"))
        .and_then(Value::as_object)
    {
        for (key, value) in extra {
            extra_files.insert(key.clone(), value.clone());
        }
    }
    let mut keys: Vec<&String> = extra_files.keys().collect();
    keys.sort();
    for path in keys {
        let content = &extra_files[path];
        if !matches!(
            content,
            Value::String(_) | Value::Number(_) | Value::Bool(_)
        ) {
            return crate::util::message(format!(
                "invalid device guest extra_files entry: {path:?}"
            ));
        }
        files.push(json!({
            "path": path,
            "permissions": "0644",
            "content": value_text(content),
        }));
    }
    Ok(files)
}

pub fn cloud_init_runcmd(cfg: &Config, resolved: &Value) -> Vec<Value> {
    let guest = object(resolved, "guest");
    let mut commands: Vec<Value> = Vec::new();
    let mut sh = |script: &str| {
        commands.push(json!(["sh", "-c", script]));
    };
    sh(
        "rm -f /etc/default/grub.d/40-force-partuuid.cfg /etc/default/grub.d/40-force-partuuid.cfg.disabled",
    );
    sh("update-initramfs -c -k all");
    sh("update-grub");
    let sound_enabled = object(object(resolved, "devices"), "sound")
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let install_sound = guest
        .get("install_sound_modules")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if install_sound && sound_enabled {
        sh(
            "DEBIAN_FRONTEND=noninteractive apt-get install -y linux-modules-extra-$(uname -r) || true",
        );
    }
    let _ = cfg;
    sh("systemctl enable --now systemd-timesyncd || true");
    commands
}

pub fn alignment_script(cfg: &Config, resolved: &Value) -> Result<String> {
    let guest = object(resolved, "guest");
    let network = object(resolved, "network");
    let timezone = text(guest, "timezone", "Etc/UTC");
    let locale = text(guest, "locale", "en_US.UTF-8");
    let layout = text(guest, "keyboard_layout", "us");
    let variant = text(guest, "keyboard_variant", "");
    let ntp = object(guest, "ntp");
    let ntp_servers = string_list(ntp.get("servers")).join(" ");
    let ntp_fallback = string_list(ntp.get("fallback_servers")).join(" ");
    let rtc_utc = text(guest, "rtc", "utc").to_lowercase() == "utc";
    let packages = cloud_init_packages(resolved);
    let purge = string_list(guest.get("purge_packages"));
    let remove_users = string_list(guest.get("remove_users"));
    let remove_paths = string_list(guest.get("remove_paths"));
    let sysctl = guest
        .get("sysctl")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let ssh = object(guest, "ssh");
    let plan = render::netplan_document(resolved, Some(&cfg.hostname))?;
    let mut extra_files: Map<String, Value> = guest
        .get("extra_files")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(extra) = resolved
        .get("extra")
        .and_then(|value| value.get("guest_files"))
        .and_then(Value::as_object)
    {
        for (key, value) in extra {
            extra_files.insert(key.clone(), value.clone());
        }
    }
    let sound = object(object(resolved, "devices"), "sound");
    let sound_enabled = sound
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let install_sound = guest
        .get("install_sound_modules")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let mut lines: Vec<String> = vec![
        "set -euo pipefail".to_string(),
        "export DEBIAN_FRONTEND=noninteractive".to_string(),
        "say() { printf \"\\n[vmctl-align] %s\\n\" \"$*\"; }".to_string(),
        "say \"starting guest identity alignment\"".to_string(),
        "mkdir -p /etc/ssh/sshd_config.d /etc/sysctl.d /etc/apt/apt.conf.d".to_string(),
    ];

    lines.push(format!(
        "say 'writing persistent netplan for {}'",
        text(network, "interface", "enp0s2")
    ));
    lines.push(format!(
        "printf '%s' {} | base64 -d > /etc/netplan/50-vmctl.yaml",
        q(&base64_json(&plan))
    ));
    lines.push("chmod 600 /etc/netplan/50-vmctl.yaml".to_string());
    lines.push("rm -f /etc/netplan/50-cloud-init.yaml".to_string());
    lines.push("netplan generate".to_string());

    lines.push(format!("say 'setting timezone {timezone}'"));
    lines.push(format!("timedatectl set-timezone {}", q(timezone)));
    lines.push(format!("printf '%s\\n' {} > /etc/timezone", q(timezone)));
    lines.push(format!("say 'setting locale {locale}'"));
    lines.push("[ -f /etc/locale.gen ] || touch /etc/locale.gen".to_string());
    lines.push(format!(
        "grep -q '^{locale} ' /etc/locale.gen 2>/dev/null || echo {} >> /etc/locale.gen",
        q(&format!("{locale} UTF-8"))
    ));
    lines.push("locale-gen".to_string());
    lines.push(format!(
        "update-locale LANG={} LC_ALL={} LANGUAGE={}",
        q(locale),
        q(locale),
        q(locale)
    ));
    lines.push(format!(
        "printf 'LANG=%s\\nLC_ALL=%s\\nLANGUAGE=%s\\n' {} {} {} > /etc/default/locale",
        q(locale),
        q(locale),
        q(locale)
    ));
    lines.push(format!(
        "printf 'XKBLAYOUT=%s\\nXKBVARIANT=%s\\n' {} {} > /etc/default/keyboard",
        q(layout),
        q(variant)
    ));

    if ntp.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
        lines.push("say 'configuring NTP'".to_string());
        lines.push("systemctl enable --now systemd-timesyncd".to_string());
        lines.push("mkdir -p /etc/systemd/timesyncd.conf.d".to_string());
        let mut ntp_conf = vec!["[Time]".to_string()];
        if !ntp_servers.is_empty() {
            ntp_conf.push(format!("NTP={ntp_servers}"));
        }
        if !ntp_fallback.is_empty() {
            ntp_conf.push(format!("FallbackNTP={ntp_fallback}"));
        }
        let ntp_text = ntp_conf.join("\n") + "\n";
        lines.push(format!(
            "printf '%s' {} | base64 -d > /etc/systemd/timesyncd.conf.d/90-vmctl.conf",
            q(&b64(ntp_text.as_bytes()))
        ));
        lines.push("systemctl restart systemd-timesyncd".to_string());
        lines.push("timedatectl set-ntp true".to_string());
    } else {
        lines.push("timedatectl set-ntp false".to_string());
    }

    lines.push("say 'freezing automatic package upgrades'".to_string());
    lines.push("systemctl disable --now unattended-upgrades.service apt-daily.timer apt-daily-upgrade.timer 2>/dev/null || true".to_string());
    lines.push(format!(
        "timedatectl set-local-rtc {}",
        if rtc_utc { "0" } else { "1" }
    ));
    lines.push(
        "if command -v hwclock >/dev/null 2>&1; then hwclock --systohc || true; fi".to_string(),
    );
    lines.push(format!("say 'setting hostname {}'", cfg.hostname));
    lines.push(format!("hostnamectl set-hostname {}", q(&cfg.hostname)));
    lines.push(format!(
        "printf '%s\\n' {} > /etc/hostname",
        q(&cfg.hostname)
    ));
    lines.push("sed -i '/^127\\.0\\.1\\.1[[:space:]]/d' /etc/hosts || true".to_string());
    lines.push(format!(
        "printf '127.0.1.1\\t%s\\n' {} >> /etc/hosts",
        q(&cfg.hostname)
    ));

    if !packages.is_empty() {
        lines.push("say 'installing guest packages'".to_string());
        lines.push("apt-get update -qq || true".to_string());
        let joined: Vec<String> = packages.iter().map(|item| q(item)).collect();
        lines.push(format!("apt-get install -y -qq {}", joined.join(" ")));
    }
    if sound_enabled && install_sound {
        lines.push("say 'installing kernel audio modules'".to_string());
        lines.push("apt-get install -y -qq linux-modules-extra-$(uname -r) || true".to_string());
    }
    if text(
        object(object(resolved, "devices"), "storage"),
        "controller",
        "nvme",
    ) == "nvme"
    {
        lines.push("modprobe nvme 2>/dev/null || true".to_string());
    }

    if !purge.is_empty() {
        lines.push("say 'removing cloud-init and guest-agent packages'".to_string());
        let quoted: Vec<String> = purge.iter().map(|item| q(item)).collect();
        lines.push(format!("for pkg in {}; do", quoted.join(" ")));
        lines.push(
            "  status=$(dpkg-query -W -f='${Status}' \"$pkg\" 2>/dev/null || true)".to_string(),
        );
        lines.push(
            "  if printf '%s' \"$status\" | grep -q 'install ok installed'; then".to_string(),
        );
        lines.push("    apt-get purge -y \"$pkg\" || true".to_string());
        lines.push("  fi".to_string());
        lines.push("done".to_string());
        lines.push("apt-get clean || true".to_string());
    }
    for path in &remove_paths {
        lines.push(format!("rm -rf {}", q(path)));
    }
    lines.extend(
        [
            "rm -f /etc/ssh/sshd_config.d/50-cloud-init.conf",
            "rm -f /etc/ssh/sshd_config.d/60-cloudimg-settings.conf",
            "rm -f /etc/ssh/sshd_config.d/*cloud-init* /etc/ssh/sshd_config.d/*cloudimg* || true",
            "rm -f /var/log/cloud-init.log /var/log/cloud-init-output.log || true",
            "rm -f /var/log/apt/history.log* || true",
            "rm -rf /var/lib/cloud /etc/cloud || true",
        ]
        .iter()
        .map(|line| line.to_string()),
    );

    for user in &remove_users {
        if user == &cfg.username {
            continue;
        }
        lines.push(format!(
            "if id -u {} >/dev/null 2>&1; then userdel -r {} || true; fi",
            q(user),
            q(user)
        ));
    }

    let ssh_conf = [
        format!("PermitRootLogin {}", text(ssh, "permit_root_login", "no")),
        format!(
            "PasswordAuthentication {}",
            if ssh
                .get("password_authentication")
                .and_then(Value::as_bool)
                .unwrap_or(true)
            {
                "yes"
            } else {
                "no"
            }
        ),
    ];
    lines.push("say 'writing local sudo policy'".to_string());
    lines.push("if [ -n \"${SUDO_USER:-}\" ] && [ \"$SUDO_USER\" != \"root\" ]; then".to_string());
    lines.push("  usermod -aG adm,sudo,audio \"$SUDO_USER\" 2>/dev/null || true".to_string());
    lines.push(
        "  printf '%s ALL=(ALL) NOPASSWD:ALL\\n' \"$SUDO_USER\" > /etc/sudoers.d/90-local-admin"
            .to_string(),
    );
    lines.push("  chmod 440 /etc/sudoers.d/90-local-admin".to_string());
    lines.push("  visudo -cf /etc/sudoers.d/90-local-admin >/dev/null 2>&1 || rm -f /etc/sudoers.d/90-local-admin".to_string());
    lines.push("fi".to_string());
    lines.push("rm -f /etc/sudoers.d/90-cloud-init-users".to_string());
    lines.push("say 'writing sshd policy'".to_string());
    let quoted: Vec<String> = ssh_conf.iter().map(|item| q(item)).collect();
    lines.push(format!(
        "printf '%s\\n' {} > /etc/ssh/sshd_config.d/99-vmctl.conf",
        quoted.join(" ")
    ));
    lines.push("chmod 600 /etc/ssh/sshd_config.d/99-vmctl.conf".to_string());
    lines.push("sed -i 's/^[#[:space:]]*PasswordAuthentication .*/PasswordAuthentication yes/' /etc/ssh/sshd_config || true".to_string());
    lines.push("grep -q '^PasswordAuthentication' /etc/ssh/sshd_config || echo 'PasswordAuthentication yes' >> /etc/ssh/sshd_config".to_string());
    lines.push("sed -i 's/^[#[:space:]]*PermitRootLogin .*/PermitRootLogin no/' /etc/ssh/sshd_config || true".to_string());
    lines.push("grep -q '^PermitRootLogin' /etc/ssh/sshd_config || echo 'PermitRootLogin no' >> /etc/ssh/sshd_config".to_string());

    let regenerate_machine_id = guest
        .get("regenerate_machine_id")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if regenerate_machine_id {
        lines.push("say 'regenerating machine-id'".to_string());
        lines.push("rm -f /etc/machine-id /var/lib/dbus/machine-id".to_string());
        lines.push("systemd-machine-id-setup".to_string());
        lines.push("mkdir -p /var/lib/dbus".to_string());
        lines.push("ln -sf /etc/machine-id /var/lib/dbus/machine-id".to_string());
    }
    let regenerate_ssh = guest
        .get("regenerate_ssh_host_keys")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if regenerate_ssh {
        lines.push("say 'regenerating ssh host keys'".to_string());
        lines.push("rm -f /etc/ssh/ssh_host_*".to_string());
        lines.push("ssh-keygen -A".to_string());
    }

    if !sysctl.is_empty() {
        let mut keys: Vec<&String> = sysctl.keys().collect();
        keys.sort();
        let text = keys
            .iter()
            .map(|key| format!("{key} = {}", value_text(&sysctl[*key])))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        lines.push("say 'applying sysctl hardening'".to_string());
        lines.push(format!(
            "printf '%s' {} | base64 -d > /etc/sysctl.d/99-vmctl-hardening.conf",
            q(&b64(text.as_bytes()))
        ));
        lines.push("sysctl --system >/dev/null 2>&1 || true".to_string());
    }

    lines.push("for f in /usr/share/misc/pci.ids /usr/share/hwdata/pci.ids; do".to_string());
    lines.push("  [ -f \"$f\" ] || continue".to_string());
    lines.push("  sed -i 's/QEMU PCIe Host bridge/PCIe Host Bridge/g; s/QEMU NVM Express Controller/NVMe SSD Controller/g; s/QEMU Virtual Machine/Personal Computer/g; s/QEMU USB/Standard USB/g' \"$f\" || true".to_string());
    lines.push("done".to_string());

    let mut extra_keys: Vec<&String> = extra_files.keys().collect();
    extra_keys.sort();
    for path in extra_keys {
        let content = value_text(&extra_files[path]);
        let encoded = b64(content.as_bytes());
        lines.push(format!("mkdir -p $(dirname {})", q(path)));
        lines.push(format!(
            "printf '%s' {} | base64 -d > {}",
            q(&encoded),
            q(path)
        ));
    }
    for command in string_list(guest.get("extra_commands")) {
        lines.push(command);
    }
    if let Some(extra) = resolved
        .get("extra")
        .and_then(|value| value.get("guest_commands"))
    {
        for command in string_list(Some(extra)) {
            lines.push(command);
        }
    }

    if guest
        .get("clean_logs")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        lines.push("say 'rotating guest logs'".to_string());
        lines.push("journalctl --rotate >/dev/null 2>&1 || true".to_string());
        lines.push("journalctl --vacuum-time=1s >/dev/null 2>&1 || true".to_string());
        lines.push(
            "rm -f /var/log/cloud-init* /var/log/vmctl-* /var/log/dpkg.log* || true".to_string(),
        );
    }

    lines.push("update-grub >/dev/null 2>&1 || true".to_string());
    lines.push("systemctl daemon-reload || true".to_string());
    lines.push("say \"guest identity alignment complete\"".to_string());
    let _ = text(guest, "locale_fallback", "C.UTF-8");
    Ok(lines.join("\n") + "\n")
}

pub fn efi_entry_script(cfg: &Config, resolved: &Value) -> &'static str {
    let guest = object(resolved, "guest");
    if !guest
        .get("create_efi_boot_entry")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        return "true\n";
    }
    let storage = object(object(resolved, "devices"), "storage");
    if text(storage, "controller", "nvme") != "nvme" {
        return "true\n";
    }
    let _ = cfg;
    r#"
set -u
command -v efibootmgr >/dev/null 2>&1 || exit 0
[ -d /sys/firmware/efi ] || exit 0
set -e
src=$(findmnt -no SOURCE /boot/efi 2>/dev/null || true)
[ -n "$src" ] || exit 0
disk=$(lsblk -no PKNAME "$src" 2>/dev/null | head -1)
part=$(cat /sys/class/block/$(basename "$src")/partition 2>/dev/null || true)
[ -n "$disk" ] && [ -n "$part" ] || exit 0
# Drop stale entries with the same label, then create one for the NVMe ESP.
for boot in $(efibootmgr | sed -n 's/^Boot\([0-9A-Fa-f]\{4\}\)\*\{0,1\} ubuntu$/\1/p'); do
  efibootmgr -b "$boot" -B >/dev/null 2>&1 || true
done
efibootmgr -c -d "/dev/$disk" -p "$part" -L ubuntu -l '\EFI\ubuntu\grubaa64.efi' >/dev/null 2>&1 || true
efibootmgr >/dev/null 2>&1 || true
"#
}
