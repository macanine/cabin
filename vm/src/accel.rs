//! Acceleration status: what the host can run the guest with, why it cannot
//! use hardware acceleration, and the Windows WHPX probe.
//!
//! The classifier is platform-neutral so it is unit-tested on every host; the
//! probe launches QEMU and is only consulted on Windows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::util::capture;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase", tag = "kind", content = "detail")]
pub enum AccelProblem {
    /// The Windows Hypervisor Platform optional feature is not enabled.
    WhpxFeatureOff,
    /// Hardware virtualization is off in firmware, or the hypervisor is not running.
    VirtualizationOff,
    /// Another hypervisor holds the virtualization hardware.
    HypervisorConflict,
    /// The probe failed for another reason (first line of QEMU's message).
    ProbeFailed(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct AccelStatus {
    pub accel: &'static str,
    pub cpu: String,
    pub emulated: bool,
    pub problem: Option<AccelProblem>,
}

/// Map QEMU's WHPX diagnostics to a problem. `None` means no known WHPX
/// failure appears in the text.
pub fn classify_whpx_output(output: &str) -> Option<AccelProblem> {
    let lower = output.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    // The optional feature is off: the accelerator or its DLL is missing.
    if has("winhvplatform")
        || has("could not load library")
        || has("could not load function")
        || has("invalid accelerator whpx")
        || has("unsupported accelerator")
        || has("accelerator does not exist")
        || has("windows hypervisor platform")
    {
        return Some(AccelProblem::WhpxFeatureOff);
    }
    // Another hypervisor owns the hardware.
    if has("whvcreatepartition failed")
        || has("failed to create partition")
        || has("hypervisor is already in use")
        || has("0x80070005")
    {
        return Some(AccelProblem::HypervisorConflict);
    }
    // The platform loaded but no hypervisor answered.
    if has("no accelerator found")
        || has("hypervisor is not present")
        || has("hypervisor not present")
        || has("virtualization is not enabled")
    {
        return Some(AccelProblem::VirtualizationOff);
    }
    if has("whpx:") || has("whpx ") || has("whpx_init_vcpu failed") {
        let line = output
            .lines()
            .map(str::trim)
            .find(|line| line.to_ascii_lowercase().contains("whpx"))
            .unwrap_or("WHPX failed");
        return Some(AccelProblem::ProbeFailed(line.to_string()));
    }
    None
}

/// Classify a QEMU start failure for a fallback log line.
pub fn classify_failure(output: &str) -> AccelProblem {
    classify_whpx_output(output).unwrap_or_else(|| {
        AccelProblem::ProbeFailed(
            output
                .lines()
                .map(str::trim)
                .rfind(|line| !line.is_empty())
                .unwrap_or("QEMU exited")
                .to_string(),
        )
    })
}

fn probes() -> &'static Mutex<HashMap<PathBuf, Result<(), AccelProblem>>> {
    static PROBES: OnceLock<Mutex<HashMap<PathBuf, Result<(), AccelProblem>>>> = OnceLock::new();
    PROBES.get_or_init(Default::default)
}

fn fallback() -> &'static Mutex<Option<AccelProblem>> {
    static FALLBACK: OnceLock<Mutex<Option<AccelProblem>>> = OnceLock::new();
    FALLBACK.get_or_init(Default::default)
}

/// Require a QMP status reply from a paused guest under WHPX.
pub fn probe_whpx(qemu: &Path) -> Result<(), AccelProblem> {
    let argv: Vec<String> = [
        &qemu.display().to_string(),
        "-accel",
        "whpx,kernel-irqchip=off",
        "-machine",
        "q35",
        "-nodefaults",
        "-display",
        "none",
        "-S",
        "-monitor",
        "none",
        "-serial",
        "none",
        "-qmp",
        "stdio",
    ]
    .iter()
    .map(|part| part.to_string())
    .collect();
    let requests = concat!(
        "{\"execute\":\"qmp_capabilities\"}\n",
        "{\"execute\":\"query-status\",\"id\":\"status\"}\n",
        "{\"execute\":\"quit\"}\n",
    );
    let result = capture(&argv, Duration::from_secs(8), Some(requests));
    if result.code == 0 && probe_status_received(&result.output) {
        return Ok(());
    }
    Err(
        classify_whpx_output(&result.output).unwrap_or_else(|| match result.code {
            124 => {
                AccelProblem::ProbeFailed("QEMU timed out before completing the WHPX probe".into())
            }
            0 => AccelProblem::ProbeFailed("QEMU did not return WHPX monitor status".into()),
            _ => classify_failure(&result.output),
        }),
    )
}

fn probe_status_received(output: &str) -> bool {
    output.lines().any(|line| {
        serde_json::from_str::<serde_json::Value>(line)
            .is_ok_and(|reply| reply["id"] == "status" && reply["return"]["status"].is_string())
    })
}

/// `probe_whpx`, cached per engine path.
pub fn probe_whpx_cached(qemu: &Path) -> Result<(), AccelProblem> {
    if let Some(known) = probes().lock().unwrap_or_else(|e| e.into_inner()).get(qemu) {
        return known.clone();
    }
    let result = probe_whpx(qemu);
    probes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(qemu.to_path_buf(), result.clone());
    result
}

/// The most recent cached WHPX probe failure, if any engine failed it.
pub(crate) fn cached_whpx_problem() -> Option<AccelProblem> {
    probes()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .find_map(|result| result.as_ref().err().cloned())
}

/// Why a WHPX start was abandoned for TCG in this process, if it was.
pub fn fallback_reason() -> Option<AccelProblem> {
    fallback().lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Record the first real boot failure and keep using TCG in this process.
fn fall_back_to_tcg(reason: AccelProblem) {
    fallback()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert(reason);
}

pub(crate) fn retry_start_with_tcg<T>(
    mut start: impl FnMut() -> Result<T, (crate::Error, Option<AccelProblem>)>,
    fall_back: impl FnOnce(&crate::Error, AccelProblem),
) -> crate::Result<T> {
    match start() {
        Ok(value) => Ok(value),
        Err((error, Some(reason))) => {
            fall_back(&error, reason);
            start().map_err(|(error, _)| error)
        }
        Err((error, None)) => Err(error),
    }
}

fn start_fallback_path(home: &Path) -> PathBuf {
    crate::paths::StatePaths::new(home)
        .global_state()
        .join("whpx-start.json")
}

fn start_fallback_key(engine: &Path) -> Option<serde_json::Value> {
    let meta = std::fs::metadata(engine).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some(serde_json::json!({
        "engine": engine,
        "size": meta.len(),
        "modified": [modified.as_secs(), u64::from(modified.subsec_nanos())],
    }))
}

fn saved_start_fallback(home: &Path, engine: &Path, boot_time: u64) -> Option<AccelProblem> {
    let key = start_fallback_key(engine)?;
    let mut saved = crate::util::read_json(&start_fallback_path(home))?;
    // On Windows sysinfo estimates boot time from wall time and tick count.
    if saved["key"] != key || saved["boot"].as_u64()?.abs_diff(boot_time) > 2 {
        return None;
    }
    serde_json::from_value(saved["reason"].take()).ok()
}

/// Try WHPX again after a host reboot or a QEMU update, when the failure
/// may have been fixed. Across Cabin restarts on the same boot, keep the reason.
pub(crate) fn restore_start_fallback(home: &Path, engine: &Path) {
    if cfg!(windows)
        && fallback_reason().is_none()
        && let Some(reason) = saved_start_fallback(home, engine, sysinfo::System::boot_time())
    {
        fall_back_to_tcg(reason);
    }
}

pub(crate) fn remember_start_fallback(
    home: &Path,
    engine: &Path,
    reason: AccelProblem,
    log: &std::sync::Arc<Mutex<std::fs::File>>,
) {
    if let Some(key) = start_fallback_key(engine)
        && let Err(error) = write_start_fallback(home, key, sysinfo::System::boot_time(), &reason)
    {
        crate::util::log_line(
            log,
            &format!("Could not save the WHPX fallback reason: {error}"),
        );
    }
    fall_back_to_tcg(reason);
}

fn write_start_fallback(
    home: &Path,
    key: serde_json::Value,
    boot_time: u64,
    reason: &AccelProblem,
) -> crate::Result<()> {
    crate::util::atomic_write_json(
        &start_fallback_path(home),
        &serde_json::json!({"key": key, "boot": boot_time, "reason": reason}),
        0o600,
    )
}

/// Current acceleration, with the reason hardware acceleration is unavailable.
pub fn status() -> AccelStatus {
    let accel = crate::platform::accel();
    let problem = if accel.emulated && cfg!(windows) {
        fallback_reason()
            .or_else(cached_whpx_problem)
            .or(Some(AccelProblem::WhpxFeatureOff))
    } else {
        None
    };
    AccelStatus {
        accel: accel.accel,
        cpu: accel.cpu.to_string(),
        emulated: accel.emulated,
        problem,
    }
}

/// Probe the engine's QEMU first (Windows only), then report `status()`.
pub fn status_for_engine(qemu: &Path) -> AccelStatus {
    if cfg!(windows) && crate::platform::whpx_available() {
        let _ = probe_whpx_cached(qemu);
    }
    status()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whpx_probe_requires_the_requested_monitor_status() {
        assert!(!probe_status_received(r#"{"QMP":{"version":{}}}"#));
        assert!(!probe_status_received(
            r#"{"id":"status","error":{"desc":"not ready"}}"#
        ));
        assert!(probe_status_received(
            "WHPX initialized\n{\"id\":\"status\",\"return\":{\"status\":\"prelaunch\"}}\n"
        ));
    }

    #[test]
    fn whpx_output_is_classified() {
        for (text, expected) in [
            (
                "qemu-system-x86_64.exe: invalid accelerator whpx",
                Some(AccelProblem::WhpxFeatureOff),
            ),
            (
                "Could not load library WinHvPlatform.dll.",
                Some(AccelProblem::WhpxFeatureOff),
            ),
            (
                "WHPX: No accelerator found, hr=00000000\nqemu: failed to initialize whpx: Operation not supported",
                Some(AccelProblem::VirtualizationOff),
            ),
            (
                "WHvCreatePartition failed with 0x80070005",
                Some(AccelProblem::HypervisorConflict),
            ),
            (
                "noise\nWHPX: Failed to setup partition, hr=c0350005\n",
                Some(AccelProblem::ProbeFailed(
                    "WHPX: Failed to setup partition, hr=c0350005".into(),
                )),
            ),
            ("qemu: unrelated error", None),
        ] {
            assert_eq!(classify_whpx_output(text), expected, "{text}");
        }
        assert_eq!(
            classify_failure("one\nqemu: unrelated error\n"),
            AccelProblem::ProbeFailed("qemu: unrelated error".into())
        );
    }

    #[test]
    fn real_start_failure_retries_once_and_returns_the_second_error() {
        let mut launches = 0;
        let mut fallbacks = 0;
        let result = retry_start_with_tcg(
            || {
                launches += 1;
                let error = crate::Error::Message(format!("failed launch {launches}"));
                Err::<(), _>((error, Some(AccelProblem::VirtualizationOff)))
            },
            |error, reason| {
                fallbacks += 1;
                assert_eq!(error.to_string(), "failed launch 1");
                assert_eq!(reason, AccelProblem::VirtualizationOff);
            },
        );
        assert_eq!(launches, 2);
        assert_eq!(fallbacks, 1);
        assert_eq!(result.unwrap_err().to_string(), "failed launch 2");
    }

    #[test]
    fn remembered_fallback_is_bound_to_boot_and_engine() {
        let dir = crate::sockets::test_dir();
        let engine = dir.join("qemu");
        std::fs::write(&engine, "old engine").unwrap();
        let reason = AccelProblem::HypervisorConflict;
        write_start_fallback(&dir, start_fallback_key(&engine).unwrap(), 42, &reason).unwrap();
        assert_eq!(saved_start_fallback(&dir, &engine, 42), Some(reason));
        assert_eq!(saved_start_fallback(&dir, &engine, 60), None);
        std::fs::write(&engine, "fixed engine with a different size").unwrap();
        assert_eq!(saved_start_fallback(&dir, &engine, 42), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
