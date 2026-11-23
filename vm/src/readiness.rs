//! First-screen readiness: what the host needs before the VM can be
//! installed, in plain language, each problem with a fix when one exists.
//!
//! Nothing here exposes developer paths; a missing bundled tool is a broken
//! install ("Reinstall Cabin"), never something the user installs by hand.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::accel::{self, AccelProblem};
use crate::arch::GuestArch;
use crate::{Result, platform, qemu_engine, util};

const GIB: u64 = 1024 * 1024 * 1024;
/// Room for the downloaded cloud image next to the disk.
const IMAGE_HEADROOM_GB: u64 = 3;
/// Memory the host keeps for itself beyond the VM.
const HOST_HEADROOM_MB: u64 = 2048;
const VIRTUALIZATION_HELP: &str = "https://support.microsoft.com/windows/enable-virtualization-on-windows-c5578302-6e43-4b4b-a449-8ced115f58e1";

/// What a readiness item can do about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Fix {
    /// Turn on the Windows Hypervisor Platform feature (elevated).
    EnableWhpx,
    /// Restart the computer.
    RestartHost,
    /// The bundled tools are damaged or missing: install Cabin again.
    ReinstallApp,
    /// Open a help page.
    OpenUrl { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct Readiness {
    pub id: String,
    pub label: String,
    pub ok: bool,
    /// A warning: the install can proceed.
    pub optional: bool,
    pub detail: String,
    pub fix: Option<Fix>,
}

/// Facts the setup screen uses to pick defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct HostFacts {
    pub memory_mb: u32,
    pub cpus: u32,
    /// The host account name, as the OS reports it (not yet sanitized).
    pub username: Option<String>,
    pub device_profile_supported: bool,
    /// macos | windows | linux
    pub platform: String,
    // Whole GiB left for the VM disk after reserving space for Ubuntu.
    pub max_disk_gb: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct ReadinessReport {
    pub items: Vec<Readiness>,
    pub host: HostFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct FixOutcome {
    pub restart_required: bool,
}

fn item(
    id: &str,
    label: &str,
    ok: bool,
    optional: bool,
    detail: impl Into<String>,
    fix: Option<Fix>,
) -> Readiness {
    Readiness {
        id: id.into(),
        label: label.into(),
        ok,
        optional,
        detail: detail.into(),
        fix: if ok { None } else { fix },
    }
}

/// Plain-language text and fix for an acceleration problem.
pub fn accel_problem_text(problem: &AccelProblem) -> (String, Option<Fix>) {
    const SLOW: &str = "Cabin still works without it, but the VM runs several times slower and installing can take an hour or more.";
    match problem {
        AccelProblem::WhpxFeatureOff => (
            format!(
                "Windows Hypervisor Platform is turned off. {SLOW} Turn it on, then restart Windows."
            ),
            Some(Fix::EnableWhpx),
        ),
        AccelProblem::VirtualizationOff => (
            format!(
                "Hardware virtualization is unavailable. Turn on virtualization (VT-x or AMD-V) in your computer's firmware settings. If it is already on, check Windows Hypervisor Platform and restart Windows. {SLOW}"
            ),
            Some(Fix::OpenUrl {
                url: VIRTUALIZATION_HELP.into(),
            }),
        ),
        AccelProblem::HypervisorConflict => (
            format!(
                "Another program or a Windows virtualization setting may be preventing hardware acceleration. Close other virtual machine apps and restart Windows. For managed Hyper-V or Virtualization-Based Security settings, contact your administrator. {SLOW}"
            ),
            Some(Fix::RestartHost),
        ),
        AccelProblem::ProbeFailed(reason) => (
            format!("Hardware acceleration could not start: {reason}. {SLOW}"),
            None,
        ),
    }
}

fn acceleration(status: accel::AccelStatus) -> Readiness {
    let label = if cfg!(target_os = "macos") {
        "Hardware acceleration (HVF)"
    } else {
        "Hardware acceleration"
    };
    let problem = match status.problem {
        Some(AccelProblem::WhpxFeatureOff) if platform::virtualization_disabled_in_firmware() => {
            Some(AccelProblem::VirtualizationOff)
        }
        other => other,
    };
    match (&problem, status.emulated) {
        (Some(problem), _) => {
            let (detail, fix) = accel_problem_text(problem);
            item("accel", label, false, true, detail, fix)
        }
        (None, true) => item(
            "accel",
            label,
            false,
            true,
            "Hardware acceleration is not available on this computer. Cabin still works, but the VM runs several times slower and installing can take an hour or more.",
            None,
        ),
        (None, false) => item(
            "accel",
            label,
            true,
            false,
            "The VM runs at near-native speed.",
            None,
        ),
    }
}

fn supported_host_problem(os: &str, arch: &str, version: Option<&str>) -> Option<&'static str> {
    let version = version.and_then(|value| value.split('.').next()?.parse::<u32>().ok());
    match (os, arch) {
        ("macos", "x86" | "x86_64") => Some(
            "Cabin needs a Mac with Apple silicon (M1 or later). This Mac uses an Intel processor.",
        ),
        ("macos", _) if version.is_some_and(|major| major < 14) => Some(
            "Cabin needs macOS 14 Sonoma or later. Update macOS in System Settings → General → Software Update.",
        ),
        ("windows", "aarch64" | "arm" | "arm64") => Some(
            "Cabin needs an Intel or AMD x64 PC. Windows on ARM is unsupported, including through x64 emulation.",
        ),
        ("windows", _) if arch != "x86_64" => Some(
            "Cabin needs 64-bit Windows on an x64 PC. Install 64-bit Windows 10 22H2 or Windows 11 on a compatible PC.",
        ),
        ("windows", _) if version.is_some_and(|build| build < 19045) => Some(
            "Cabin needs Windows 10 22H2 (build 19045) or Windows 11. Open Settings → Windows Update to upgrade.",
        ),
        _ => None,
    }
}

pub fn host_os_version(os: &str) -> Option<String> {
    if os == "windows" {
        sysinfo::System::kernel_version()
    } else {
        sysinfo::System::os_version()
    }
}

fn mac_translocated(path: &Path) -> bool {
    path.components()
        .any(|part| part.as_os_str() == "AppTranslocation")
}

fn windows_path_checks(
    home: &Path,
    appdata: Option<&Path>,
    profile: Option<&Path>,
    onedrive: &[PathBuf],
    mapped_network_drive: bool,
) -> Vec<Readiness> {
    let appdata = appdata.unwrap_or(home).to_string_lossy().replace('/', "\\");
    let data_path = home.to_string_lossy();
    let normalized = appdata.to_lowercase();
    let in_onedrive = normalized
        .split('\\')
        .any(|part| part == "onedrive" || part.starts_with("onedrive - "))
        || onedrive.iter().any(|root| {
            let root = root.to_string_lossy().replace('/', "\\").to_lowercase();
            !root.is_empty()
                && (normalized == root
                    || normalized
                        .strip_prefix(root.trim_end_matches('\\'))
                        .is_some_and(|suffix| suffix.starts_with('\\')))
        });
    // Roaming AppData is normal on a local disk. Flag redirected network storage.
    let network = mapped_network_drive
        || normalized.starts_with(r"\\?\unc\")
        || (normalized.starts_with(r"\\") && !normalized.starts_with(r"\\?\"));
    let mut issues = Vec::new();
    if in_onedrive || network {
        issues.push(item("appDataLocation", "Windows app data is redirected", false, true,
            "Use a Windows profile with local AppData outside OneDrive and network storage, or ask your administrator to restore local AppData before installing.", None));
    }
    // Leave room in MAX_PATH for instance, step, and log file names.
    if appdata.encode_utf16().count() >= 180 || data_path.encode_utf16().count() >= 180 {
        issues.push(item("appDataLength", "Windows app data path is long", false, true,
            "Use a Windows account with a shorter local profile path, or ask your administrator to shorten the AppData location, to avoid path-length errors.", None));
    }
    if !appdata.is_ascii()
        || !data_path.is_ascii()
        || profile.is_some_and(|path| !path.to_string_lossy().is_ascii())
    {
        issues.push(item("profileEncoding", "Windows profile path contains non-ASCII characters", false, true,
            "If setup reports a path error, use a Windows account whose local profile and AppData paths use only ASCII characters. A display-name change does not move the profile folder.", None));
    }
    issues
}

fn qemu_launch_check(os: &str, error: &str) -> Readiness {
    let error = error.to_ascii_lowercase();
    let policy = [
        "(os error 577)",
        "(os error 1260)",
        "exit -1073740760",
        "smartscreen",
        "group policy",
    ]
    .iter()
    .any(|marker| error.contains(marker));
    let detail = if os == "windows" && policy {
        "Windows blocked the bundled QEMU by security policy. Check Windows Security or ask your administrator to allow the official Cabin build, then recheck."
    } else if os == "windows"
        && (error.contains("(os error 225)") || error.contains("(os error 226)"))
    {
        "Windows antivirus blocked the bundled QEMU. Open Windows Security → Protection history, or check your antivirus, to review the block on your official Cabin download. Reinstall Cabin if a file was removed."
    } else if os == "windows" {
        "Cabin found its bundled QEMU but could not run it. Check Windows Security → Protection history or your antivirus for a block. If there is no block, reinstall Cabin and recheck."
    } else {
        "Cabin found its QEMU but could not run it. Install a fresh copy of Cabin in Applications and reopen it."
    };
    item(
        "qemuLaunch",
        "The virtual machine engine could not run",
        false,
        false,
        detail,
        Some(Fix::ReinstallApp),
    )
}

fn ssh_check(os: &str) -> Readiness {
    let available = platform::required_tools().iter().all(|name| {
        if os == "windows" {
            platform::bundled_ssh_tool(name).is_some()
        } else {
            platform::ssh_tool(name).is_some()
        }
    });
    let detail = if available {
        "Available."
    } else if os == "windows" {
        "Cabin's bundled SSH client is missing. Reinstall Cabin to repair it."
    } else {
        "The computer's SSH tools are missing. Restore its SSH client and key generator, then recheck."
    };
    item(
        "ssh",
        "Secure shell",
        available,
        false,
        detail,
        (os == "windows").then_some(Fix::ReinstallApp),
    )
}

fn qemu_checks(home: &Path, os: &str) -> Vec<Readiness> {
    let qemu = |ok, detail: &str| {
        item(
            "qemu",
            "Virtual machine engine",
            ok,
            false,
            detail,
            Some(Fix::ReinstallApp),
        )
    };
    match qemu_engine::require(home) {
        Ok(engine) => {
            let timeout = Duration::from_secs(4);
            let version = qemu_engine::tool_version(&engine.qemu, timeout).and_then(|version| {
                qemu_engine::tool_version(&engine.qemu_img, timeout).map(|_| version)
            });
            match version {
                Ok(version) => vec![
                    acceleration(accel::status_for_engine(&engine.qemu)),
                    qemu(true, &version),
                ],
                Err(error) => vec![qemu_launch_check(os, &error.to_string())],
            }
        }
        Err(_) if qemu_engine::can_download_on_first_run() => vec![
            acceleration(accel::status()),
            qemu(true, "Downloaded once during the install."),
        ],
        Err(_) if os == "windows" => vec![qemu(
            false,
            "Cabin's bundled QEMU is missing. Check Windows Security → Protection history for a quarantined file, then reinstall Cabin.",
        )],
        Err(_) => vec![qemu(
            false,
            "Cabin's bundled QEMU is missing. Reinstall Cabin to repair it.",
        )],
    }
}

fn free_disk_bytes(home: &Path) -> u64 {
    let mut probe = home.to_path_buf();
    while !probe.exists() && probe.pop() {}
    crate::util::disk_free_bytes(&probe)
}

fn disk_check(free: u64, disk_gb: u32) -> Readiness {
    let needed = (u64::from(disk_gb) + IMAGE_HEADROOM_GB) * GIB;
    let ok = free >= needed;
    item(
        "disk",
        "Free disk space",
        ok,
        false,
        if ok {
            format!(
                "{} free, {} needed.",
                util::format_bytes(Some(free)),
                util::format_bytes(Some(needed))
            )
        } else {
            format!(
                "Only {} is free and the VM needs about {}. Free up space or choose a smaller disk under Advanced.",
                util::format_bytes(Some(free)),
                util::format_bytes(Some(needed))
            )
        },
        None,
    )
}

fn memory_check(memory_mb: u32, host_mb: Option<u64>) -> Readiness {
    let needed = u64::from(memory_mb) + HOST_HEADROOM_MB;
    match host_mb {
        Some(host) if host < needed => item(
            "memory",
            "Memory",
            false,
            false,
            format!(
                "This computer has {} of memory and the chosen size wants {} for the VM plus {} for everything else. Pick a smaller size under Advanced.",
                gb_text(host),
                gb_text(u64::from(memory_mb)),
                gb_text(HOST_HEADROOM_MB)
            ),
            None,
        ),
        Some(host) => item(
            "memory",
            "Memory",
            true,
            false,
            format!("{} installed.", gb_text(host)),
            None,
        ),
        None => item(
            "memory",
            "Memory",
            true,
            false,
            "Could not be read; assuming enough.",
            None,
        ),
    }
}

fn gb_text(mb: u64) -> String {
    let gb = mb as f64 / 1024.0;
    if (gb - gb.round()).abs() < 0.05 {
        format!("{} GB", gb.round() as u64)
    } else {
        format!("{gb:.1} GB")
    }
}

/// The bundled guest daemon check; the host knows where the daemon lives.
pub fn service_check(present: bool) -> Readiness {
    item(
        "agentd",
        "Cabin's VM service",
        present,
        false,
        if present {
            "Included with Cabin.".to_string()
        } else {
            "Cabin's VM service is missing. Reinstall Cabin to repair it.".to_string()
        },
        Some(Fix::ReinstallApp),
    )
}

/// Every readiness item for an install of `disk_gb` GB and `memory_mb` MB.
/// The host adds `service_check` for the guest daemon it bundles.
pub fn host_readiness(home: &Path, disk_gb: u32, memory_mb: u32) -> Vec<Readiness> {
    let os = std::env::consts::OS;
    let version = host_os_version(os);
    let arch = if std::env::consts::ARCH == "x86" {
        "x86"
    } else {
        platform::native_arch()
    };
    if let Some(problem) = supported_host_problem(os, arch, version.as_deref()) {
        return vec![item(
            "supportedHost",
            "Unsupported computer",
            false,
            false,
            problem,
            None,
        )];
    }
    let mut items = Vec::new();
    if os == "macos" && std::env::current_exe().is_ok_and(|path| mac_translocated(&path)) {
        return vec![item(
            "appLocation",
            "Cabin is running from an isolated disk image location",
            false,
            false,
            "Quit Cabin, drag Cabin.app from the disk image into Applications, then open the copy in Applications.",
            None,
        )];
    }
    if os == "windows" {
        let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
        let profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
        let onedrive = ["OneDrive", "OneDriveCommercial", "OneDriveConsumer"]
            .into_iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect::<Vec<_>>();
        let mapped = platform::mapped_network_drive(home)
            || appdata
                .as_deref()
                .is_some_and(platform::mapped_network_drive);
        items.extend(windows_path_checks(
            home,
            appdata.as_deref(),
            profile.as_deref(),
            &onedrive,
            mapped,
        ));
    }
    items.extend(qemu_checks(home, os));
    items.push(ssh_check(os));
    items.push(disk_check(free_disk_bytes(home), disk_gb));
    items.push(memory_check(memory_mb, physical_memory_mb()));
    items
}

pub fn host_facts(home: &Path) -> HostFacts {
    HostFacts {
        memory_mb: physical_memory_mb().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
        cpus: std::thread::available_parallelism().map_or(2, |n| n.get() as u32),
        max_disk_gb: (free_disk_bytes(home) / GIB)
            .saturating_sub(IMAGE_HEADROOM_GB)
            .min(u64::from(u32::MAX)) as u32,
        username: std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok()
            .filter(|u| !u.is_empty()),
        device_profile_supported: GuestArch::current().device_profile_supported(),
        platform: std::env::consts::OS.into(),
    }
}

fn physical_memory_mb() -> Option<u64> {
    let mut system = sysinfo::System::new();
    system.refresh_memory();
    Some(system.total_memory() / (1024 * 1024)).filter(|&mb| mb > 0)
}

/// The argv that enables the Windows Hypervisor Platform from an elevated
/// PowerShell child. Exit code 3010 means a restart is pending.
pub fn enable_whpx_argv() -> Vec<String> {
    let script = "$p = Start-Process dism.exe -Verb RunAs -Wait -PassThru -ArgumentList \
        '/online','/enable-feature','/featurename:HypervisorPlatform','/all','/norestart'; exit $p.ExitCode";
    [
        "powershell",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]
    .map(String::from)
    .to_vec()
}

fn run(argv: &[String]) -> Result<std::process::ExitStatus> {
    Ok(Command::new(&argv[0]).args(&argv[1..]).status()?)
}

/// Read bounded diagnostic tails without following paths supplied by the UI
/// or loading an entire console log into memory.
pub fn diagnostic_history(home: &Path) -> serde_json::Value {
    use crate::paths::StatePaths;
    use serde_json::{Value, json};

    let paths = StatePaths::new(home);
    let instance = paths.instance_logs(crate::machine::NAME);
    let recent = |dir: &Path| {
        let mut files: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if !entry.file_type().ok()?.is_file() || path.extension()? != "log" {
                    return None;
                }
                Some((entry.metadata().ok()?.modified().ok()?, path))
            })
            .collect();
        files.sort_unstable();
        files.into_iter().rev().map(|(_, path)| path)
    };
    let last_start = recent(&instance).find(|path| {
        // Startup attempts use start-<milliseconds>-<pid>-<sequence>.log.
        path.file_name().and_then(|name| name.to_str()).and_then(|name| name.strip_prefix("start-"))
            .and_then(|name| name.split('-').next())
            .is_some_and(|stamp| stamp.parse::<u64>().is_ok())
    }).map(|path| {
        let log = diagnostic_tail(&path, 24 * 1024);
        let state = if log.contains("Startup failed:") { "failed" }
            else if log.contains("Startup complete") { "succeeded" }
            else { "incomplete" };
        json!({ "state": state, "logTail": log.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>() })
    });
    let last_apply = recent(&instance)
        .chain(recent(&paths.logs().join("global")))
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("-apply-"))
        })
        .filter_map(|path| Some((path.metadata().ok()?.modified().ok()?, path)))
        .max();
    let journal = diagnostic_tail(&paths.journal(), 128 * 1024);
    let records: Vec<Value> = journal
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    // Apply is also used by startup repairs. Anchor the install fallback to
    // the latest *install* step log, which uses "-apply-" instead of "-start-".
    let last_apply_ms = last_apply
        .and_then(|(time, _)| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64);
    let apply_start = records.iter().rposition(|row| {
        row["event"] == "apply_start"
            && row["ts"]
                .as_u64()
                .zip(last_apply_ms)
                .is_some_and(|(ts, latest)| ts <= latest)
    });
    let last_install: Vec<_> = apply_start
        .into_iter()
        .flat_map(|index| {
            let end = records[index + 1..]
                .iter()
                .position(|row| row["event"] == "apply_start" || row["event"] == "preflight")
                .map_or(records.len(), |end| index + end + 1);
            &records[index..end]
        })
        .collect();
    let global = recent(&paths.logs().join("global")).take(3);
    let console = recent(paths.instance(crate::machine::NAME).root()).filter(|path| {
        path.file_name().is_some_and(|name| {
            name == "serial.log" || name == "provision-serial.log" || name == "display.log"
        })
    });
    let logs: Vec<_> = recent(&instance)
        .take(7)
        .chain(global)
        .chain(console)
        .map(|path| {
            let log = diagnostic_tail(&path, 12 * 1024);
            json!({ "file": path.file_name().unwrap_or_default().to_string_lossy(), "tail": log })
        })
        .collect();
    json!({ "lastStart": last_start, "lastInstall": last_install, "logTails": logs })
}

fn diagnostic_tail(path: &Path, max_bytes: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};

    let read = || -> std::io::Result<String> {
        let mut file = std::fs::File::open(path)?;
        let offset = file.metadata()?.len().saturating_sub(max_bytes);
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        file.take(max_bytes).read_to_end(&mut bytes)?;
        let text = String::from_utf8_lossy(&bytes);
        // Discard the first partial line, including any partial home paths.
        let text = if offset > 0 {
            text.split_once('\n').map_or("", |(_, rest)| rest)
        } else {
            &text
        };
        Ok(text.to_owned())
    };
    read().unwrap_or_default()
}

/// Redact home roots and account names in every string, including nested log
/// entries. Do this before encoding JSON so escaped Windows paths are covered.
pub fn redact_diagnostics(value: &mut serde_json::Value, roots: &[&Path], users: &[&str]) {
    use regex::Regex;

    let mut roots: Vec<_> = roots
        .iter()
        .filter(|root| root.parent().is_some())
        .flat_map(|root| {
            let root = root.to_string_lossy();
            [
                root.to_string(),
                root.replace('\\', "/"),
                root.replace('\\', "\\\\"),
            ]
        })
        .collect();
    roots.sort_unstable_by_key(|root| std::cmp::Reverse(root.len()));
    roots.dedup();
    let paths: Vec<_> = roots
        .iter()
        .filter_map(|root| Regex::new(&format!("(?i){}", regex::escape(root))).ok())
        .collect();
    let users: Vec<_> = users
        .iter()
        .flat_map(|name| [*name, name.rsplit('\\').next().unwrap_or(name)])
        .flat_map(|name| [name, name.split('@').next().unwrap_or(name)])
        .filter(|name| !name.trim().is_empty())
        .filter_map(|name| Regex::new(&format!(r"(?i)\b{}\b", regex::escape(name))).ok())
        .collect();
    let home =
        Regex::new(r#"(?i)(?:[a-z]:[/\\]+Users[/\\]+|/(?:Users|home)/)[^/\\\r\n"'`<>]+"#).unwrap();
    let ssh_user = Regex::new(r"(?i)\b[\w.-]+@((?:\d{1,3}\.){3}\d{1,3}|localhost)\b").unwrap();
    let user_field = Regex::new(r#"(?i)(\b(?:username|user)\s*[=:]\s*["']?|\b(?:creating|adding|created) user\s+["']?)[\w.-]+"#).unwrap();
    let url_credentials = Regex::new(r"(?i)(\b[a-z][a-z0-9+.-]*://)[^/\s@]+@").unwrap();
    fn walk(value: &mut serde_json::Value, redact: &impl Fn(&str) -> String) {
        use serde_json::Value;
        match value {
            Value::String(text) => *text = redact(text),
            Value::Array(values) => values.iter_mut().for_each(|value| walk(value, redact)),
            Value::Object(map) => map.values_mut().for_each(|value| walk(value, redact)),
            _ => {}
        }
    }
    walk(value, &|text| {
        let mut text = text.to_owned();
        for path in &paths {
            text = path.replace_all(&text, "<home>").into_owned();
        }
        text = home.replace_all(&text, "<home>").into_owned();
        text = ssh_user.replace_all(&text, "<user>@${1}").into_owned();
        text = user_field.replace_all(&text, "${1}<user>").into_owned();
        text = url_credentials
            .replace_all(&text, "${1}<credentials>@")
            .into_owned();
        for user in &users {
            text = user.replace_all(&text, "<user>").into_owned();
        }
        text
    });
}

/// Carry out a fix. Windows-only fixes say so elsewhere.
pub fn apply_fix(fix: &Fix) -> Result<FixOutcome> {
    match fix {
        Fix::EnableWhpx => {
            if !cfg!(windows) {
                return util::message("Windows Hypervisor Platform is only available on Windows");
            }
            let status = run(&enable_whpx_argv())?;
            match status.code() {
                Some(0) | Some(3010) => Ok(FixOutcome {
                    restart_required: true,
                }),
                _ => util::message(
                    "Windows Hypervisor Platform was not turned on. Approve the administrator prompt and try again.",
                ),
            }
        }
        Fix::RestartHost => {
            if !cfg!(windows) {
                return util::message(
                    "Restarting is only offered on Windows; restart this computer yourself",
                );
            }
            run(&["shutdown".into(), "/r".into(), "/t".into(), "5".into()])?;
            Ok(FixOutcome {
                restart_required: false,
            })
        }
        Fix::OpenUrl { url } => {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return util::message("Only web pages can be opened");
            }
            let argv: Vec<String> = if cfg!(windows) {
                vec![
                    "rundll32".into(),
                    "url.dll,FileProtocolHandler".into(),
                    url.clone(),
                ]
            } else if cfg!(target_os = "macos") {
                vec!["open".into(), url.clone()]
            } else {
                vec!["xdg-open".into(), url.clone()]
            };
            run(&argv)?;
            Ok(FixOutcome {
                restart_required: false,
            })
        }
        Fix::ReinstallApp => {
            util::message("Download Cabin again and reinstall it to repair the missing files")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_reserves_two_gib_for_the_host() {
        let low = memory_check(8192, Some(8192));
        assert!(!low.ok && !low.optional);
        assert!(memory_check(2048, Some(4096)).ok);
        assert!(memory_check(4096, Some(16384)).ok);
        assert!(memory_check(4096, None).ok);
    }

    #[test]
    fn disk_blocks_when_too_small() {
        assert!(!disk_check(11 * GIB - 1, 8).ok);
        assert!(disk_check(11 * GIB, 8).ok);
    }

    #[test]
    fn unsupported_hosts_get_specific_upgrade_instructions() {
        for (os, arch, version, expected) in [
            ("macos", "x86_64", "15.1", "Intel processor"),
            ("macos", "aarch64", "13.6.9", "macOS 14 Sonoma"),
            ("windows", "aarch64", "26100", "Windows on ARM"),
            ("windows", "x86", "19045", "64-bit Windows"),
            ("windows", "x86_64", "19044", "22H2 (build 19045)"),
        ] {
            assert!(
                supported_host_problem(os, arch, Some(version))
                    .unwrap()
                    .contains(expected)
            );
        }
        for (os, arch, version) in [
            ("macos", "aarch64", "14.0"),
            ("windows", "x86_64", "19045"),
            ("windows", "x86_64", "26100"),
        ] {
            assert_eq!(supported_host_problem(os, arch, Some(version)), None);
        }
    }

    #[test]
    fn mac_translocation_requires_a_path_component() {
        assert!(mac_translocated(Path::new(
            "/private/var/folders/a/T/AppTranslocation/id/d/Cabin.app/Contents/MacOS/cabin"
        )));
        assert!(!mac_translocated(Path::new(
            "/Applications/Cabin.app/Contents/MacOS/cabin"
        )));
        assert!(!mac_translocated(Path::new(
            "/Users/AppTranslocation Test/Cabin.app"
        )));
    }

    #[test]
    fn windows_detects_redirected_long_and_non_ascii_profile_paths() {
        let home = Path::new(r"C:\Users\ascii\AppData\Roaming\cabin\vm");
        assert!(windows_path_checks(home, None, None, &[], false).is_empty());
        assert_eq!(
            windows_path_checks(home, None, None, &[], true)[0].id,
            "appDataLocation"
        );
        let onedrive = [PathBuf::from(r"D:\Company cloud")];
        let paths = [
            (r"d:\company cloud\AppData", "appDataLocation"),
            (
                r"C:\Users\ascii\OneDrive - Company\Roaming",
                "appDataLocation",
            ),
            (r"\\server\profiles\ascii\AppData", "appDataLocation"),
            (r"\\?\UNC\server\profiles\ascii\AppData", "appDataLocation"),
            (r"C:\Users\杨\AppData", "profileEncoding"),
        ];
        for (path, id) in paths {
            let items = windows_path_checks(home, Some(Path::new(path)), None, &onedrive, false);
            assert_eq!(items.len(), 1, "{path}");
            assert_eq!(items[0].id, id);
            assert!(!items[0].detail.contains(path));
        }
        let long = PathBuf::from(format!(r"C:\{}\AppData", "x".repeat(180)));
        assert_eq!(
            windows_path_checks(home, Some(&long), None, &[], false)[0].id,
            "appDataLength"
        );
        let profile = Path::new(r"C:\Users\杨");
        assert_eq!(
            windows_path_checks(home, None, Some(profile), &[], false)[0].id,
            "profileEncoding"
        );
        assert!(
            windows_path_checks(
                home,
                Some(Path::new(r"D:\Company Cloud Backup")),
                None,
                &onedrive,
                false,
            )
            .is_empty()
        );
    }

    #[test]
    fn windows_qemu_execution_errors_lead_to_protection_history_or_policy() {
        let antivirus = qemu_launch_check(
            "windows",
            "Operation did not complete successfully (os error 225)",
        );
        assert!(!antivirus.ok && !antivirus.optional);
        assert!(antivirus.detail.contains("Protection history"));
        let policy = qemu_launch_check(
            "windows",
            "This program is blocked by group policy (os error 1260)",
        );
        assert!(policy.detail.contains("security policy"));
        assert_eq!(policy.fix, Some(Fix::ReinstallApp));
    }

    #[test]
    fn diagnostics_redact_paths_and_user_names_before_json_encoding() {
        use serde_json::json;
        let roots = [
            Path::new(r"D:\profiles\Host Name"),
            Path::new("/private/local/Custom Home"),
        ];
        let mut report = json!({
            "readiness": [{ "detail": r"d:\PROFILES\Host Name\AppData\Roaming\cabin" }],
            "lastInstall": {
                "error": r#"could not write "D:\\profiles\\Host Name\\Temp\\file""#,
                "steps": [{ "logTail": ["Host Name started the machine", "/home/guestname/.ssh", "GuestName@127.0.0.1: SSH timed out"] }],
            },
            "lastStart": { "facts": [{ "value": "/private/local/Custom Home/.cabin/qemu" }] },
            "logTails": [
                r"C:\Users\Archived Profile\AppData\log",
                "/Users/Another Name/Library/log",
                "Cloud-init: Creating user 'PreviousGuest'",
                "Network: https://ProxyUser:Secret@mirror.example/path",
            ],
        });
        redact_diagnostics(&mut report, &roots, &[r"CORP\Host Name", "guestname"]);
        let encoded = report.to_string();
        for private in [
            "Host Name",
            "GuestName",
            "guestname",
            "Archived Profile",
            "Another Name",
            "Custom Home",
            "PreviousGuest",
            "ProxyUser",
            "Secret",
        ] {
            assert!(!encoded.contains(private), "{private}: {encoded}");
        }
        assert!(encoded.contains("<home>") && encoded.contains("<user>@127.0.0.1: SSH timed out"));
    }

    #[test]
    fn diagnostic_history_recovers_reports_and_bounds_log_tails() {
        use crate::paths::StatePaths;
        let home = std::env::temp_dir().join(format!("diagnostic-history-{}", std::process::id()));
        let paths = StatePaths::new(&home);
        let instance = paths.instance_logs(crate::machine::NAME);
        let global = paths.logs().join("global");
        std::fs::create_dir_all(&instance).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        let log = format!(
            "first boot began\n{}\nStartup failed: QEMU exited\n",
            "x".repeat(30_000)
        );
        std::fs::write(instance.join("start-1200-10-0.log"), log).unwrap();
        std::fs::write(instance.join("disk-start-20260702.log"), "not an attempt").unwrap();
        std::fs::write(
            global.join("image-apply-20260702.log"),
            "image could not be downloaded",
        )
        .unwrap();
        std::fs::write(
            paths.journal(),
            concat!(
                "{\"event\":\"apply_start\",\"ts\":1}\n",
                "{\"event\":\"step_failed\",\"step\":\"image\",\"error\":\"connection reset\"}\n",
                "{\"event\":\"preflight\",\"ts\":18446744073709551614}\n",
                "{\"event\":\"apply_start\",\"ts\":18446744073709551614}\n",
                "{\"event\":\"step_ok\",\"step\":\"disk\"}\n",
            ),
        )
        .unwrap();
        let report = diagnostic_history(&home);
        assert_eq!(report["lastInstall"].as_array().unwrap().len(), 2);
        assert_eq!(report["lastInstall"][1]["step"], "image");
        assert_eq!(report["lastStart"]["state"], "failed");
        assert_eq!(
            report["lastStart"]["logTail"][0],
            "Startup failed: QEMU exited"
        );
        assert!(
            report["logTails"]
                .as_array()
                .unwrap()
                .iter()
                .any(|log| log["tail"] == "image could not be downloaded")
        );
        assert!(report.to_string().len() < 16_000);
        std::fs::remove_dir_all(home).unwrap();
    }
}
