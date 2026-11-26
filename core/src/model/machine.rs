use serde::{Deserialize, Serialize};

#[cfg(test)]
use ts_rs::TS;

use super::AppSnapshot;

// --------------------------------------------------------------------- clock

/// The guest's clock. Every guest timestamp is Unix milliseconds on this
/// clock; the UI compares it with the Mac's to place them on one time line.
/// `timezone` is informational: the UI always shows times in the Mac's zone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GuestClock {
    #[cfg_attr(test, ts(type = "number"))]
    pub now_ms: i64,
    pub timezone: Option<String>,
}

/// A read-only observation of the running guest, collected on demand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct GuestMachineObservation {
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub kernel: Option<String>,
    #[cfg_attr(test, ts(type = "number"))]
    pub uptime_seconds: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub root_total_bytes: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub root_available_bytes: Option<u64>,
}

// ------------------------------------------------------------------- monitor

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MonitorSample {
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub cpu_percent: Option<f32>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub memory_used: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub memory_total: Option<u64>,
    pub latency_ms: Option<u32>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub download_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub upload_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub disk_read_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub disk_write_bps: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub swap_used: Option<u64>,
    /// `None` when the machine has no swap.
    #[cfg_attr(test, ts(type = "number | null"))]
    #[serde(default)]
    pub swap_total: Option<u64>,
    /// Load averages over 1, 5 and 15 minutes.
    #[serde(default)]
    pub load: Option<LoadAverage>,
    #[serde(default)]
    pub cpu_count: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

/// One process in the machine, for the performance details.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    /// The command line, shortened; empty for kernel threads.
    pub command: String,
    /// Share of the whole machine (all cores), 0–100.
    pub cpu_percent: f32,
    #[cfg_attr(test, ts(type = "number"))]
    pub memory: u64,
    /// Seconds since the process started.
    #[cfg_attr(test, ts(type = "number"))]
    pub run_time: u64,
}

/// The busiest processes, asked for only while the details are open.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct ProcessList {
    #[cfg_attr(test, ts(type = "number"))]
    pub at: i64,
    pub total: u32,
    pub by_cpu: Vec<ProcessInfo>,
    pub by_memory: Vec<ProcessInfo>,
}

// ------------------------------------------------------------- the machine

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum MachineState {
    NotInstalled,
    Stopped,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum GuestConnection {
    Offline,
    Connecting,
    /// Copying a matching cabind into the VM.
    Updating,
    Connected,
}

/// The VM as seen from the Mac.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MachineStatus {
    pub state: MachineState,
    pub connection: GuestConnection,
    /// What the Mac is doing to the VM right now ("Installing…", "Stopping…").
    pub busy: Option<String>,
    pub ssh_port: Option<u16>,
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_size: Option<String>,
    /// The in-app desktop (headless Sway over VNC) is installed.
    pub display: bool,
    /// Guest operating system id (`os::RELEASES`), once installed.
    pub os: Option<String>,
    pub browser: bool,
    pub device: bool,
    pub guest_version: Option<String>,
    /// The VM's Linux user and hostname (from the install).
    pub username: Option<String>,
    pub hostname: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase", default)]
pub struct MachineInstallOptions {
    pub cpus: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_size: Option<String>,
    pub username: Option<String>,
    pub hostname: Option<String>,
    pub device: bool,
    /// Archive mirror for apt in the VM, matching the OS (`None`: the
    /// image's default archive).
    pub apt_mirror: Option<String>,
    /// Guest operating system id (`None`: the default, Ubuntu 24.04).
    pub os: Option<String>,
}

/// An operating system the installer offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct OsChoice {
    pub id: String,
    pub name: String,
    /// A development release with daily images.
    pub testing: bool,
    /// Archive mirrors offered for it, the default first.
    pub mirrors: Vec<String>,
}

/// Result of testing one ARM64 package mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct MirrorResult {
    pub uri: String,
    pub host: String,
    /// The archive the image uses when no mirror is chosen.
    pub default: bool,
    /// ok | noArm64 | unreachable
    pub status: String,
    pub latency_ms: Option<u32>,
    pub speed_kbps: Option<u32>,
    pub detail: String,
}

/// One pipeline step during a machine install.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct InstallStep {
    pub id: String,
    pub title: String,
    /// pending | running | done | cached | failed | rolledBack
    pub state: String,
    pub detail: Option<String>,
    pub log_tail: Vec<String>,
    pub log_path: Option<String>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub seconds: Option<u64>,
    /// Unix milliseconds.
    #[cfg_attr(test, ts(type = "number | null"))]
    pub applied_at: Option<i64>,
    pub fingerprint: Option<String>,
    /// Bytes, total and speed while the step downloads (the cloud image).
    #[serde(default)]
    pub download: Option<DownloadProgress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    #[cfg_attr(test, ts(type = "number"))]
    pub done_bytes: u64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub total_bytes: Option<u64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub bytes_per_sec: Option<u64>,
    /// All bytes are here and the checksum is being verified.
    pub verifying: bool,
}

/// What an install will do (the wizard's "automation hints").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    pub os: String,
    pub os_name: String,
    pub os_choices: Vec<OsChoice>,
    /// Where the newest build is found; it is pinned when the install starts.
    pub image_index: String,
    pub qemu_release: String,
    pub qemu_installed: bool,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_size: String,
    pub network: String,
    pub accel: String,
    pub firmware: Option<String>,
    pub disk_path: String,
    pub seed_path: String,
    pub username: String,
    pub hostname: String,
    pub ssh_port: u16,
    pub apt_mirror: Option<String>,
    pub steps: Vec<InstallStep>,
}

/// What `app_subscribe` returns on the Mac: the machine, plus the guest's
/// snapshot when cabind is connected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct HostSnapshot {
    pub machine: MachineStatus,
    pub guest: Option<AppSnapshot>,
    /// The running or most recent machine start.
    pub start: Option<StartReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StartFact {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum StartPhaseState {
    Pending,
    Running,
    Done,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StartPhase {
    pub id: String,
    pub title: String,
    pub state: StartPhaseState,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub started_at: Option<i64>,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub finished_at: Option<i64>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub enum StartState {
    Running,
    Succeeded,
    Failed,
}

/// Live report of a machine start, from the Mac app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(TS))]
#[serde(rename_all = "camelCase")]
pub struct StartReport {
    pub state: StartState,
    #[cfg_attr(test, ts(type = "number"))]
    pub started_at: i64,
    #[cfg_attr(test, ts(type = "number | null"))]
    pub finished_at: Option<i64>,
    pub error: Option<String>,
    /// QEMU path and version, accelerator, firmware, resources, ports, pid…
    pub facts: Vec<StartFact>,
    pub phases: Vec<StartPhase>,
    pub log_path: Option<String>,
    pub log_tail: Vec<String>,
    /// The guest's serial console (only output from this boot).
    pub serial_path: Option<String>,
    pub serial_tail: Vec<String>,
}
