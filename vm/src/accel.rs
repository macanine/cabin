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
    if has("whpx:") || has("whpx ") {
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

/// Start `qemu` under WHPX with no devices for about three seconds. A process
/// still alive at the deadline means the accelerator initialised.
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
    ]
    .iter()
    .map(|part| part.to_string())
    .collect();
    let result = capture(&argv, Duration::from_secs(3), None);
    match result.code {
        124 => Ok(()),
        127 | 125 => Err(AccelProblem::ProbeFailed(result.output.trim().to_string())),
        0 => Ok(()),
        _ => Err(classify_failure(&result.output)),
    }
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

/// Run under TCG from now on, recording the reason.
pub fn fall_back_to_tcg(reason: AccelProblem) {
    *fallback().lock().unwrap_or_else(|e| e.into_inner()) = Some(reason);
}

/// Forget a recorded fallback (tests).
#[cfg(test)]
pub(crate) fn reset_fallback() {
    *fallback().lock().unwrap_or_else(|e| e.into_inner()) = None;
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
    fn missing_feature() {
        for text in [
            "qemu-system-x86_64.exe: invalid accelerator whpx",
            "Could not load library WinHvPlatform.dll.",
            "qemu-system-x86_64.exe: -accel whpx: Windows Hypervisor Platform is not available",
        ] {
            assert_eq!(classify_whpx_output(text), Some(AccelProblem::WhpxFeatureOff), "{text}");
        }
    }

    #[test]
    fn virtualization_off() {
        assert_eq!(
            classify_whpx_output("WHPX: No accelerator found, hr=00000000\nqemu: failed to initialize whpx: Operation not supported"),
            Some(AccelProblem::VirtualizationOff)
        );
    }

    #[test]
    fn hypervisor_conflict() {
        for text in [
            "WHPX: Failed to create partition, hr=80070005",
            "WHvCreatePartition failed with 0x80070005",
        ] {
            assert_eq!(classify_whpx_output(text), Some(AccelProblem::HypervisorConflict), "{text}");
        }
    }

    #[test]
    fn other_whpx_failures_keep_their_line() {
        assert_eq!(
            classify_whpx_output("noise\nWHPX: Failed to setup partition, hr=c0350005\n"),
            Some(AccelProblem::ProbeFailed("WHPX: Failed to setup partition, hr=c0350005".into()))
        );
        assert_eq!(classify_whpx_output("qemu: unrelated error"), None);
        assert_eq!(
            classify_failure("one\nqemu: unrelated error\n"),
            AccelProblem::ProbeFailed("qemu: unrelated error".into())
        );
    }

    #[test]
    fn status_serializes_camel_case() {
        let status = AccelStatus {
            accel: "tcg",
            cpu: "max".into(),
            emulated: true,
            problem: Some(AccelProblem::ProbeFailed("x".into())),
        };
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["problem"]["kind"], "probeFailed");
        assert_eq!(value["problem"]["detail"], "x");
        assert_eq!(
            serde_json::to_value(AccelProblem::WhpxFeatureOff).unwrap()["kind"],
            "whpxFeatureOff"
        );
    }

    #[test]
    fn fallback_is_recorded_and_visible() {
        reset_fallback();
        assert_eq!(fallback_reason(), None);
        fall_back_to_tcg(AccelProblem::VirtualizationOff);
        assert_eq!(fallback_reason(), Some(AccelProblem::VirtualizationOff));
        reset_fallback();
    }
}
