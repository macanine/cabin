//! First-screen readiness: what the host needs before the VM can be
//! installed, in plain language, each problem with a fix when one exists.
//!
//! Nothing here exposes developer paths; a missing bundled tool is a broken
//! install ("Reinstall Cabin"), never something the user installs by hand.

use std::path::Path;
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::accel::{self, AccelProblem};
use crate::arch::GuestArch;
use crate::{Result, util};

const GIB: u64 = 1024 * 1024 * 1024;
/// Room for the downloaded cloud image next to the disk.
const IMAGE_HEADROOM_GB: u64 = 3;
/// Memory the host keeps for itself beyond the VM.
const HOST_HEADROOM_MB: u64 = 2048;
const VIRTUALIZATION_HELP: &str =
    "https://support.microsoft.com/windows/enable-virtualization-on-windows-c5578302-6e43-4b4b-a449-8ced115f58e1";

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

fn item(id: &str, label: &str, ok: bool, optional: bool, detail: impl Into<String>, fix: Option<Fix>) -> Readiness {
    Readiness { id: id.into(), label: label.into(), ok, optional, detail: detail.into(), fix: if ok { None } else { fix } }
}

/// Plain-language text and fix for an acceleration problem.
pub fn accel_problem_text(problem: &AccelProblem) -> (String, Option<Fix>) {
    const SLOW: &str = "Cabin still works without it, but the VM runs several times slower and installing can take an hour or more.";
    match problem {
        AccelProblem::WhpxFeatureOff => (
            format!("Windows Hypervisor Platform is turned off. {SLOW} Turn it on, then restart Windows."),
            Some(Fix::EnableWhpx),
        ),
        AccelProblem::VirtualizationOff => (
            format!("Hardware virtualization is off. Turn on virtualization (VT-x or AMD-V) in your computer's firmware settings. {SLOW}"),
            Some(Fix::OpenUrl { url: VIRTUALIZATION_HELP.into() }),
        ),
        AccelProblem::HypervisorConflict => (
            format!("Another program is using the virtualization hardware (for example another virtual machine app or Hyper-V settings). Close it, or restart Windows. {SLOW}"),
            Some(Fix::RestartHost),
        ),
        AccelProblem::ProbeFailed(reason) => (format!("Hardware acceleration could not start: {reason}. {SLOW}"), None),
    }
}

fn acceleration() -> Readiness {
    let label = if cfg!(target_os = "macos") { "Hardware acceleration (HVF)" } else { "Hardware acceleration" };
    let status = accel::status();
    match (&status.problem, status.emulated) {
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
        (None, false) => item("accel", label, true, false, "The VM runs at near-native speed.", None),
    }
}

fn bundled_tools(home: &Path) -> Vec<Readiness> {
    let qemu = crate::qemu_engine::require(home);
    let reinstall = Some(Fix::ReinstallApp);
    let mut out = vec![item(
        "qemu",
        "Virtual machine engine",
        qemu.is_ok() || cfg!(target_os = "macos"),
        false,
        match &qemu {
            Ok(_) => "Included with Cabin.".to_string(),
            Err(_) if cfg!(target_os = "macos") => "Downloaded once during the install.".to_string(),
            Err(_) => "Cabin's bundled QEMU is missing. Reinstall Cabin to repair it.".to_string(),
        },
        reinstall.clone(),
    )];
    let ssh = util::which("ssh").is_some();
    out.push(item(
        "ssh",
        "Secure shell",
        ssh,
        false,
        if ssh { "Available.".to_string() } else { "Cabin's bundled SSH client is missing. Reinstall Cabin to repair it.".to_string() },
        reinstall,
    ));
    out
}

fn disk_check(home: &Path, disk_gb: u32) -> Readiness {
    let needed = (u64::from(disk_gb) + IMAGE_HEADROOM_GB) * GIB;
    let mut probe = home.to_path_buf();
    while !probe.exists() && probe.pop() {}
    let free = crate::util::disk_free_bytes(&probe);
    let ok = free >= needed;
    item(
        "disk",
        "Free disk space",
        ok,
        false,
        if ok {
            format!("{} free, {} needed.", util::format_bytes(Some(free)), util::format_bytes(Some(needed)))
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
            true,
            format!(
                "This computer has {} of memory and the chosen size wants {} for the VM plus {} for everything else. Pick a smaller size under Advanced.",
                gb_text(host),
                gb_text(u64::from(memory_mb)),
                gb_text(HOST_HEADROOM_MB)
            ),
            None,
        ),
        Some(host) => item("memory", "Memory", true, false, format!("{} installed.", gb_text(host)), None),
        None => item("memory", "Memory", true, false, "Could not be read; assuming enough.", None),
    }
}

fn gb_text(mb: u64) -> String {
    let gb = mb as f64 / 1024.0;
    if (gb - gb.round()).abs() < 0.05 { format!("{} GB", gb.round() as u64) } else { format!("{gb:.1} GB") }
}

/// The bundled guest daemon check; the host knows where the daemon lives.
pub fn service_check(present: bool) -> Readiness {
    item(
        "agentd",
        "Cabin's VM service",
        present,
        false,
        if present { "Included with Cabin.".to_string() } else { "Cabin's VM service is missing. Reinstall Cabin to repair it.".to_string() },
        Some(Fix::ReinstallApp),
    )
}

/// Every readiness item for an install of `disk_gb` GB and `memory_mb` MB.
/// The host adds `service_check` for the guest daemon it bundles.
pub fn host_readiness(home: &Path, disk_gb: u32, memory_mb: u32) -> Vec<Readiness> {
    let mut items = vec![acceleration()];
    items.extend(bundled_tools(home));
    items.push(disk_check(home, disk_gb));
    items.push(memory_check(memory_mb, physical_memory_mb()));
    items
}

pub fn host_facts() -> HostFacts {
    HostFacts {
        memory_mb: physical_memory_mb().unwrap_or(0).min(u64::from(u32::MAX)) as u32,
        cpus: std::thread::available_parallelism().map_or(2, |n| n.get() as u32),
        username: std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok().filter(|u| !u.is_empty()),
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
    ["powershell", "-NoProfile", "-NonInteractive", "-Command", script].map(String::from).to_vec()
}

fn run(argv: &[String]) -> Result<std::process::ExitStatus> {
    Ok(Command::new(&argv[0]).args(&argv[1..]).status()?)
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
                Some(0) | Some(3010) => Ok(FixOutcome { restart_required: true }),
                _ => util::message(
                    "Windows Hypervisor Platform was not turned on. Approve the administrator prompt and try again.",
                ),
            }
        }
        Fix::RestartHost => {
            if !cfg!(windows) {
                return util::message("Restarting is only offered on Windows; restart this computer yourself");
            }
            run(&["shutdown".into(), "/r".into(), "/t".into(), "5".into()])?;
            Ok(FixOutcome { restart_required: false })
        }
        Fix::OpenUrl { url } => {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return util::message("Only web pages can be opened");
            }
            let argv: Vec<String> = if cfg!(windows) {
                vec!["rundll32".into(), "url.dll,FileProtocolHandler".into(), url.clone()]
            } else if cfg!(target_os = "macos") {
                vec!["open".into(), url.clone()]
            } else {
                vec!["xdg-open".into(), url.clone()]
            };
            run(&argv)?;
            Ok(FixOutcome { restart_required: false })
        }
        Fix::ReinstallApp => util::message("Download Cabin again and reinstall it to repair the missing files"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceleration_problems_have_plain_text_and_fixes() {
        let (text, fix) = accel_problem_text(&AccelProblem::WhpxFeatureOff);
        assert_eq!(fix, Some(Fix::EnableWhpx));
        assert!(text.contains("slower"));
        let (_, fix) = accel_problem_text(&AccelProblem::VirtualizationOff);
        assert!(matches!(fix, Some(Fix::OpenUrl { url }) if url.starts_with("https://")));
        let (text, fix) = accel_problem_text(&AccelProblem::ProbeFailed("boom".into()));
        assert!(text.contains("boom") && fix.is_none());
        assert!(accel_problem_text(&AccelProblem::HypervisorConflict).0.contains("Another program"));
    }

    #[test]
    fn memory_warns_but_never_blocks() {
        let low = memory_check(8192, Some(8192));
        assert!(!low.ok && low.optional);
        assert!(memory_check(4096, Some(16384)).ok);
        assert!(memory_check(4096, None).ok);
    }

    #[test]
    fn disk_blocks_when_too_small() {
        let dir = std::env::temp_dir();
        assert!(!disk_check(&dir, u32::MAX / 2).ok);
        assert!(disk_check(&dir, 1).ok);
    }

    #[test]
    fn fixes_serialize_tagged() {
        let json = serde_json::to_value(Fix::OpenUrl { url: "https://x".into() }).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "openUrl", "url": "https://x" }));
        assert_eq!(serde_json::to_value(Fix::EnableWhpx).unwrap(), serde_json::json!({ "kind": "enableWhpx" }));
    }

    #[test]
    fn elevated_dism_enables_the_hypervisor_platform() {
        let argv = enable_whpx_argv();
        assert!(argv.last().unwrap().contains("HypervisorPlatform"));
        assert!(argv.last().unwrap().contains("RunAs"));
    }

    #[test]
    fn readiness_lists_every_item_without_paths() {
        let items = host_readiness(&std::env::temp_dir(), 20, 4096);
        for id in ["accel", "qemu", "ssh", "disk", "memory"] {
            assert!(items.iter().any(|i| i.id == id), "{id}");
        }
        assert!(items.iter().all(|i| !i.detail.contains("cabind")));
        assert!(!service_check(false).ok && service_check(true).ok);
    }
}
