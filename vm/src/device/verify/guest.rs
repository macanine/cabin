//! Guest checks: time, locale, identity, residue, users and boot setup.

use std::collections::BTreeMap;

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::py_json;

use super::shared::{Context, file_text, out, repr_string, truncate_chars};

/// Values the summary needs from this section.
pub(super) struct Output {
    pub(super) timezone: String,
    pub(super) effective: String,
    pub(super) installed_cloud: BTreeMap<String, String>,
}

pub(super) fn run(ctx: &mut Context<'_>) -> Output {
    let raw = ctx.raw;
    let guest = ctx.guest;
    let cfg = ctx.cfg;
    let report = &mut ctx.report;

    // ---------------------------------------------------------------- GUEST
    let timedate = out(raw, "timedatectl");
    let timezone = kv(&timedate, "Timezone");
    let expected_timezone = cfg.current_timezone();
    if !expected_timezone.is_empty() && timezone != expected_timezone {
        report.add(
            "guest",
            "guest.timezone",
            "error",
            "Guest timezone does not match the profile",
            &format!(
                "timezone={}; expected {}",
                repr_string(&timezone),
                repr_string(&expected_timezone)
            ),
            Some(&json!(timedate)),
            "Set device.guest.timezone and rerun device-seal.",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.timezone",
            "info",
            "Guest timezone matches",
            if timezone.is_empty() {
                &expected_timezone
            } else {
                &timezone
            },
            None,
            "",
            false,
        );
    }

    let locale_out = out(raw, "locale");
    let expected_locale = cfg.current_locale();
    let lang = kv(&locale_out, "LANG");
    let lc_all = kv(&locale_out, "LC_ALL");
    let effective = if lc_all.is_empty() {
        lang.clone()
    } else {
        lc_all.clone()
    };
    if !expected_locale.is_empty() && !effective.is_empty() && effective != expected_locale {
        report.add(
            "guest",
            "guest.locale",
            "warning",
            "Guest locale differs from the profile",
            &format!(
                "locale={}; expected {}",
                repr_string(&effective),
                repr_string(&expected_locale)
            ),
            Some(&json!(locale_out)),
            "Set device.guest.locale and rerun the alignment step.",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.locale",
            "info",
            "Guest locale matches",
            if effective.is_empty() {
                &expected_locale
            } else {
                &effective
            },
            None,
            "",
            false,
        );
    }

    let ntp_synced = kv(&timedate, "NTPSynchronized");
    let ntp_enabled = kv(&timedate, "NTP");
    if guest
        .get("ntp")
        .and_then(|value| value.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        if ntp_synced.to_lowercase() != "yes" {
            report.add(
                "guest",
                "guest.ntp",
                "warning",
                "NTP is not synchronized yet",
                &format!(
                    "NTP={} NTPSynchronized={}",
                    repr_string(&ntp_enabled),
                    repr_string(&ntp_synced)
                ),
                Some(&json!(timedate)),
                "Allow systemd-timesyncd a few seconds after boot; check UDP/123 egress.",
                false,
            );
        } else {
            report.add(
                "guest",
                "guest.ntp",
                "info",
                "NTP is synchronized",
                &timedate,
                None,
                "",
                false,
            );
        }
    }
    let local_rtc = kv(&timedate, "LocalRTC");
    if local_rtc.to_lowercase() == "yes" {
        report.add(
            "guest",
            "guest.rtc",
            "error",
            "RTC is in local time",
            "LocalRTC=yes is a Windows-style configuration",
            Some(&json!(timedate)),
            "timedatectl set-local-rtc 0",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.rtc",
            "info",
            "RTC is in UTC",
            &format!(
                "LocalRTC={}",
                if local_rtc.is_empty() {
                    "no"
                } else {
                    local_rtc.as_str()
                }
            ),
            None,
            "",
            false,
        );
    }

    let hostname = out(raw, "hostname").trim().to_string();
    let expected_hostname = cfg.current_hostname();
    if !expected_hostname.is_empty() && hostname != expected_hostname {
        report.add(
            "guest",
            "guest.hostname",
            "error",
            "Guest hostname differs from the configuration",
            &format!(
                "hostname={}; expected {}",
                repr_string(&hostname),
                repr_string(&expected_hostname)
            ),
            Some(&json!(hostname)),
            "",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.hostname",
            "info",
            "Guest hostname matches",
            &hostname,
            None,
            "",
            false,
        );
    }

    let package_map = packages(raw);
    let tracked = [
        "cloud-init",
        "qemu-guest-agent",
        "open-vm-tools",
        "virtualbox-guest-utils",
    ];
    let cloud_pkgs: BTreeMap<String, String> = package_map
        .iter()
        .filter(|(name, _)| tracked.contains(&name.as_str()))
        .map(|(name, status)| (name.clone(), status.clone()))
        .collect();
    let installed_cloud: BTreeMap<String, String> = cloud_pkgs
        .iter()
        .filter(|(_, status)| status.contains("install ok installed"))
        .map(|(name, status)| (name.clone(), status.clone()))
        .collect();
    if !installed_cloud.is_empty() {
        report.add(
            "guest",
            "guest.packages",
            "error",
            "Cloud-init or a guest agent is still installed",
            "The package database lists a virtualization/cloud package.",
            Some(&json!(installed_cloud)),
            "purge cloud-init and qemu-guest-agent during device-seal.",
            false,
        );
    } else {
        let detail = py_json::dumps_default(&json!(cloud_pkgs), true);
        report.add(
            "guest",
            "guest.packages",
            "info",
            "No cloud-init/guest-agent packages installed",
            &detail,
            None,
            "",
            false,
        );
    }

    let residue_paths = [
        "/etc/cloud",
        "/var/lib/cloud",
        "/etc/vmctl-instance",
        "/etc/vmctl-proxy",
        "/var/log/cloud-init.log",
        "/var/log/cloud-init-output.log",
        "/etc/netplan/50-cloud-init.yaml",
        "/etc/ssh/sshd_config.d/50-cloud-init.conf",
    ];
    let mut bad_paths = Map::new();
    for path in residue_paths {
        if raw
            .get("paths")
            .and_then(|value| value.get(path))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bad_paths.insert(path.to_string(), json!(true));
        }
    }
    if !bad_paths.is_empty() {
        report.add(
            "guest",
            "guest.residue_paths",
            "error",
            "Cloud-init/vmctl paths remain on disk",
            "These paths identify the provisioning stack.",
            Some(&Value::Object(bad_paths)),
            "Remove them in the alignment script and verify with device-report.",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.residue_paths",
            "info",
            "No cloud-init/vmctl residue paths",
            "checked /etc/cloud, /var/lib/cloud, cloud-init logs and netplan fragments",
            None,
            "",
            false,
        );
    }

    let units = out(raw, "systemctl_units").to_lowercase();
    let bad_units: Vec<String> = units
        .lines()
        .filter(|line| line.contains("cloud-init") || line.contains("qemu-guest-agent"))
        .map(ToOwned::to_owned)
        .collect();
    if !bad_units.is_empty() {
        report.add(
            "guest",
            "guest.units",
            "error",
            "Cloud-init/guest-agent systemd units are present",
            &bad_units
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            Some(&json!(bad_units)),
            "",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.units",
            "info",
            "No cloud-init/guest-agent units",
            "systemctl is clean",
            None,
            "",
            false,
        );
    }

    let ps = out(raw, "ps");
    let bad_procs: Vec<String> = ps
        .lines()
        .filter(|line| {
            line.contains("qemu-ga")
                || line.contains("cloud-init")
                || line.contains("qemu-guest-agent")
        })
        .map(ToOwned::to_owned)
        .collect();
    if !bad_procs.is_empty() {
        report.add(
            "guest",
            "guest.processes",
            "error",
            "Guest-agent/cloud-init processes are running",
            &bad_procs
                .iter()
                .take(10)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            Some(&json!(bad_procs)),
            "",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.processes",
            "info",
            "No guest-agent/cloud-init processes",
            "ps is clean",
            None,
            "",
            false,
        );
    }

    let user_map = users(raw);
    let removed: Vec<String> = guest
        .get("remove_users")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|user| user_map.contains_key(*user) && *user != cfg.username)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if !removed.is_empty() {
        report.add(
            "guest",
            "guest.users",
            "warning",
            "Default cloud user still exists",
            &format!("users: {}", py_json::py_repr(&json!(removed))),
            Some(&json!(user_map.keys().collect::<Vec<_>>())),
            "userdel -r the default user.",
            false,
        );
    } else {
        let mut names: Vec<&String> = user_map.keys().collect();
        names.sort();
        report.add(
            "guest",
            "guest.users",
            "info",
            "Default cloud user is absent",
            &format!("login users: {}", py_json::py_repr(&json!(names))),
            None,
            "",
            false,
        );
    }

    let machine_id = file_text(raw, "/etc/machine-id").trim().to_string();
    let valid_machine_id =
        Regex::new(r"^[0-9a-f]{32}$").is_ok_and(|pattern| pattern.is_match(&machine_id));
    let all_zero = machine_id.chars().all(|ch| ch == '0');
    if !valid_machine_id || all_zero {
        report.add(
            "guest",
            "guest.machine_id",
            "warning",
            "Guest machine-id is invalid or a sentinel",
            &format!("/etc/machine-id={}", repr_string(&machine_id)),
            Some(&json!(machine_id)),
            "",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.machine_id",
            "info",
            "Guest machine-id is valid",
            &format!("{}...", truncate_chars(&machine_id, 8)),
            None,
            "",
            false,
        );
    }

    let ssh_keys = raw
        .get("ssh_keys")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if ssh_keys.is_empty() {
        report.add(
            "guest",
            "guest.ssh_host_keys",
            "error",
            "No SSH host keys",
            "/etc/ssh/ssh_host_*_key is empty",
            Some(&json!(ssh_keys)),
            "",
            false,
        );
    } else {
        let mut old = Vec::new();
        for (path, info) in &ssh_keys {
            let fingerprint = info
                .get("fingerprint")
                .and_then(|value| value.get("out"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if fingerprint.is_empty() {
                old.push(path.clone());
            }
        }
        if !old.is_empty() {
            report.add(
                "guest",
                "guest.ssh_host_keys",
                "warning",
                "SSH host key fingerprint is unreadable",
                &format!("paths: {}", py_json::py_repr(&json!(old))),
                Some(&json!(ssh_keys)),
                "",
                false,
            );
        } else {
            let fingerprints: Vec<String> = ssh_keys
                .values()
                .map(|info| {
                    info.get("fingerprint")
                        .and_then(|value| value.get("out"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .trim()
                        .to_string()
                })
                .collect();
            report.add(
                "guest",
                "guest.ssh_host_keys",
                "info",
                "SSH host keys are present and unique",
                &truncate_chars(&fingerprints.join("\n"), 2000),
                Some(&json!(ssh_keys.keys().collect::<Vec<_>>())),
                "",
                false,
            );
        }
    }

    let sshd = out(raw, "sshd_T");
    let permit_root = kv(&sshd, "permitrootlogin");
    let password_auth = kv(&sshd, "passwordauthentication");
    let guest_ssh = guest.get("ssh").unwrap_or(&Value::Null);
    let expected_root = guest_ssh
        .get("permit_root_login")
        .and_then(Value::as_str)
        .unwrap_or("no")
        .to_lowercase();
    let expected_password = guest_ssh
        .get("password_authentication")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !permit_root.is_empty() && permit_root.to_lowercase() != expected_root {
        report.add(
            "guest",
            "guest.sshd",
            "warning",
            "sshd PermitRootLogin differs from the profile",
            &format!(
                "permitrootlogin={}; expected {}",
                repr_string(&permit_root),
                repr_string(&expected_root)
            ),
            Some(&json!(truncate_chars(&sshd, 2000))),
            "",
            false,
        );
    }
    if let Some(actual_password) = onoff(&password_auth)
        && actual_password != expected_password
    {
        report.add(
            "guest",
            "guest.sshd",
            "warning",
            "sshd PasswordAuthentication differs from the profile",
            &format!(
                "passwordauthentication={}; expected {expected_password}",
                repr_string(&password_auth)
            ),
            Some(&json!(truncate_chars(&sshd, 2000))),
            "",
            false,
        );
    }
    if !permit_root.is_empty() || !password_auth.is_empty() {
        report.add(
            "guest",
            "guest.sshd",
            "info",
            "sshd policy matches the profile",
            &format!(
                "PermitRootLogin={} PasswordAuthentication={}",
                if permit_root.is_empty() {
                    "?"
                } else {
                    &permit_root
                },
                if password_auth.is_empty() {
                    "?"
                } else {
                    &password_auth
                }
            ),
            None,
            "",
            false,
        );
    }

    let sudoers = raw
        .get("sudoers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if sudoers.keys().any(|path| path.contains("cloud-init")) {
        report.add(
            "guest",
            "guest.sudoers",
            "warning",
            "Cloud-init sudoers fragment remains",
            "A /etc/sudoers.d file mentions cloud-init.",
            Some(&json!(sudoers.keys().collect::<Vec<_>>())),
            "",
            false,
        );
    } else if !sudoers.keys().any(|path| path.contains("local-admin")) {
        report.add(
            "guest",
            "guest.sudoers",
            "warning",
            "No local sudoers fragment",
            "The provisioned user needs a stable passwordless sudo policy after cloud-init is purged.",
            Some(&json!(sudoers.keys().collect::<Vec<_>>())),
            "The alignment script writes /etc/sudoers.d/90-local-admin.",
            false,
        );
    } else {
        let mut names: Vec<&String> = sudoers.keys().collect();
        names.sort();
        report.add(
            "guest",
            "guest.sudoers",
            "info",
            "Local sudoers fragment is present",
            &py_json::dumps_default(&json!(names), false),
            None,
            "",
            false,
        );
    }

    let netplan = file_text(raw, "/etc/netplan/50-vmctl.yaml");
    let cloud_netplan = raw
        .get("paths")
        .and_then(|value| value.get("/etc/netplan/50-cloud-init.yaml"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if netplan.is_empty() {
        report.add(
            "guest",
            "guest.netplan",
            "error",
            "Persistent netplan file is missing",
            "/etc/netplan/50-vmctl.yaml was not written",
            Some(&json!(netplan)),
            "",
            false,
        );
    } else if cloud_netplan {
        report.add(
            "guest",
            "guest.netplan",
            "error",
            "Cloud-init netplan file remains",
            "/etc/netplan/50-cloud-init.yaml exists",
            Some(&json!(netplan)),
            "",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.netplan",
            "info",
            "Persistent netplan is present",
            &truncate_chars(&netplan, 1500),
            Some(&json!(netplan)),
            "",
            false,
        );
    }

    let log_hits = raw
        .get("log_hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let cloud_log_hits: Vec<Value> = log_hits
        .iter()
        .filter(|hit| {
            let text = hit
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            ["cloud-init", "qemu-ga", "qemu-guest", "virtio"]
                .iter()
                .any(|token| text.contains(token))
        })
        .cloned()
        .collect();
    if !cloud_log_hits.is_empty() {
        report.add(
            "guest",
            "guest.logs",
            "warning",
            "Guest logs contain provisioning strings",
            "Old cloud-init/qemu-guest-agent lines remain in log files.",
            Some(&json!(cloud_log_hits.iter().take(10).collect::<Vec<_>>())),
            "Rotate/vacuum journald and remove cloud-init logs in device-seal.",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.logs",
            "info",
            "Guest logs are clean of provisioning strings",
            &format!("{} log files scanned", log_hits.len()),
            None,
            "",
            false,
        );
    }

    let journal_cloud = out(raw, "journal_cloud");
    if !journal_cloud.trim().is_empty() {
        report.add(
            "guest",
            "guest.journal",
            "warning",
            "Kernel/journal contains cloud-init or virtio lines",
            &truncate_chars(&journal_cloud, 2000),
            Some(&json!(truncate_chars(&journal_cloud, 2000))),
            "The runtime boot should have no virtio; check the NVMe runtime phase.",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.journal",
            "info",
            "Journal has no cloud-init/virtio lines",
            "",
            None,
            "",
            false,
        );
    }

    let cmdline = out(raw, "cmdline");
    if cmdline.contains("root=UUID=") {
        report.add(
            "guest",
            "guest.cmdline",
            "info",
            "Guest boots through an initramfs root=UUID",
            &cmdline,
            None,
            "",
            false,
        );
    } else {
        report.add(
            "guest",
            "guest.cmdline",
            "info",
            "Guest kernel command line",
            &cmdline,
            None,
            "",
            false,
        );
    }

    let path_flag = |path: &str| {
        raw.get("paths")
            .and_then(|value| value.get(path))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    };
    let fw_cfg_path = path_flag("/sys/firmware/qemu_fw_cfg");
    let acpi_qemu = path_flag("/sys/bus/acpi/devices/QEMU0002:00");
    let lsmod_text = out(raw, "lsmod").to_lowercase();
    let fw_cfg_module = lsmod_text.contains("qemu_fw_cfg");
    if fw_cfg_path || fw_cfg_module {
        report.add(
            "guest",
            "guest.fw_cfg",
            "error",
            "qemu_fw_cfg driver is still visible",
            "The qemu_fw_cfg module or /sys/firmware/qemu_fw_cfg is present.",
            Some(&json!({"module_loaded": fw_cfg_module, "sysfs_path": fw_cfg_path})),
            "Runtime.start removes qemu_fw_cfg after SSH is ready.",
            false,
        );
    } else if acpi_qemu {
        report.add(
            "guest",
            "guest.fw_cfg",
            "warning",
            "ACPI namespace still contains QEMU0002",
            "The fw_cfg driver is unloaded and /sys/firmware/qemu_fw_cfg is gone, but the QEMU0002 ACPI device node remains.",
            Some(&json!("/sys/bus/acpi/devices/QEMU0002:00")),
            "Documented residual; cannot rename the DSDT device without QEMU changes.",
            true,
        );
    } else {
        report.add(
            "guest",
            "guest.fw_cfg",
            "info",
            "qemu_fw_cfg driver and ACPI device are hidden",
            "no qemu_fw_cfg module or sysfs node",
            None,
            "",
            false,
        );
    }

    let efi = out(raw, "efibootmgr");
    if path_flag("/sys/firmware/efi") {
        if !efi.is_empty() && efi.to_lowercase().contains("ubuntu") {
            report.add(
                "guest",
                "guest.efi",
                "info",
                "UEFI boot entry is present",
                &truncate_chars(&efi, 2000),
                None,
                "",
                false,
            );
        } else {
            report.add(
                "guest",
                "guest.efi",
                "warning",
                "No 'ubuntu' UEFI boot entry",
                "efibootmgr did not list an ubuntu entry",
                Some(&json!(truncate_chars(&efi, 2000))),
                "",
                false,
            );
        }
    } else {
        report.add(
            "guest",
            "guest.efi",
            "warning",
            "Guest was not booted through UEFI",
            "/sys/firmware/efi is absent",
            Some(&json!(cmdline)),
            "",
            false,
        );
    }
    Output {
        timezone,
        effective,
        installed_cloud,
    }
}

fn kv(text: &str, key: &str) -> String {
    let low_key = key.to_lowercase();
    for line in text.lines() {
        let stripped = line.trim();
        if let Some((name, value)) = stripped.split_once('=')
            && name.trim().to_lowercase() == low_key
        {
            return value.trim().to_string();
        }
        let mut parts = stripped.splitn(2, char::is_whitespace);
        if let (Some(name), Some(value)) = (parts.next(), parts.next())
            && name.to_lowercase() == low_key
        {
            return value.trim().to_string();
        }
    }
    String::new()
}

fn packages(raw: &Value) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for line in out(raw, "dpkg").lines() {
        let mut parts = line.split('\t');
        if let (Some(name), Some(status)) = (parts.next(), parts.next()) {
            result.insert(name.to_string(), status.to_string());
        }
    }
    result
}

fn users(raw: &Value) -> Map<String, Value> {
    raw.get("users")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

fn onoff(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Some(true),
        "no" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}
