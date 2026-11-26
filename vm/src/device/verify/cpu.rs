//! CPU checks: vendor, model, MIDR, topology, hypervisor flags and logs.

use std::collections::BTreeSet;

use regex::Regex;
use serde_json::{Value, json};

use super::shared::{Context, as_text, field, out, rc, repr_string};

/// Values the later groups and the summary need from this section.
pub(super) struct Output {
    pub(super) detect: String,
    pub(super) virt_what: String,
    pub(super) dmesg: String,
}

pub(super) fn run(ctx: &mut Context<'_>) -> Output {
    let raw = ctx.raw;
    let cfg = ctx.cfg;
    let cpu = ctx.cpu;
    let topology = ctx.topology;
    let midr = ctx.midr;
    let expected_model = ctx.expected_model.clone();
    let lscpu = ctx.lscpu.clone();
    let cpuinfo = ctx.cpuinfo.clone();
    let report = &mut ctx.report;

    // ------------------------------------------------------------------ CPU
    let lscpu_vendor = field(&lscpu, "Vendor ID");
    let lscpu_model = field(&lscpu, "Model name");
    let expected_vendor = cpu.get("vendor").and_then(Value::as_str).unwrap_or("ARM");
    if lscpu_vendor.is_empty() {
        report.add(
            "cpu",
            "cpu.lscpu_vendor",
            "error",
            "CPU vendor is not readable",
            "lscpu did not report a Vendor ID",
            Some(&json!(lscpu)),
            "",
            false,
        );
    } else if lscpu_vendor.to_lowercase() != expected_vendor.to_lowercase() {
        report.add(
            "cpu",
            "cpu.lscpu_vendor",
            "error",
            "CPU vendor is not the profile vendor",
            &format!(
                "lscpu Vendor ID is {}; expected {}",
                repr_string(&lscpu_vendor),
                repr_string(expected_vendor)
            ),
            Some(&json!(lscpu)),
            "Set device.cpu.midr to a supported ARM server MIDR.",
            false,
        );
    } else {
        report.add(
            "cpu",
            "cpu.lscpu_vendor",
            "info",
            "CPU vendor matches",
            &format!("Vendor ID: {lscpu_vendor}"),
            Some(&json!(lscpu_vendor)),
            "",
            false,
        );
    }

    if lscpu_model.is_empty() || lscpu_model == "-" {
        report.add(
            "cpu",
            "cpu.model",
            "error",
            "CPU model name is missing",
            "lscpu Model name is empty or '-'",
            Some(&json!(lscpu)),
            "",
            false,
        );
    } else if !expected_model.is_empty()
        && !lscpu_model
            .to_lowercase()
            .contains(&expected_model.to_lowercase())
    {
        report.add(
            "cpu",
            "cpu.model",
            "error",
            "CPU model name does not match the profile",
            &format!(
                "lscpu Model name is {}; expected to contain {}",
                repr_string(&lscpu_model),
                repr_string(&expected_model)
            ),
            Some(&json!(lscpu)),
            "",
            false,
        );
    } else {
        report.add(
            "cpu",
            "cpu.model",
            "info",
            "CPU model matches",
            &format!("Model name: {lscpu_model}"),
            None,
            "",
            false,
        );
    }

    let implementer = parse_hex(&field(&cpuinfo, "CPU implementer"));
    let part_field = field(&cpuinfo, "CPU part");
    let part = if part_field.is_empty() {
        0
    } else {
        parse_hex(&part_field)
    };
    if midr != 0 {
        let exp_impl = (midr >> 24) & 0xFF;
        let exp_part = (midr >> 4) & 0xFFF;
        if implementer != exp_impl || part != exp_part {
            report.add(
                "cpu",
                "cpu.midr",
                "error",
                "Guest MIDR does not match the profile",
                &format!(
                    "implementer=0x{implementer:02x} part=0x{part:03x}; expected implementer=0x{exp_impl:02x} part=0x{exp_part:03x}"
                ),
                Some(&json!(cpuinfo)),
                "",
                false,
            );
        } else {
            report.add(
                "cpu",
                "cpu.midr",
                "info",
                "Guest MIDR matches the profile",
                &format!("implementer=0x{implementer:02x} part=0x{part:03x}"),
                None,
                "",
                false,
            );
        }
    }

    let cpus = field(&lscpu, "CPU(s)");
    let sockets = field(&lscpu, "Socket(s)");
    if !cpus.is_empty() && cpus != cfg.cpus.to_string() {
        report.add(
            "cpu",
            "cpu.topology",
            "warning",
            "Guest vCPU count differs",
            &format!("lscpu reports {cpus} CPUs; configuration says {}", cfg.cpus),
            Some(&json!(lscpu)),
            "",
            false,
        );
    } else if !sockets.is_empty()
        && topology.get("sockets").is_some()
        && sockets != as_text(&topology["sockets"])
    {
        report.add(
            "cpu",
            "cpu.topology",
            "warning",
            "Guest socket count differs",
            &format!(
                "lscpu reports {sockets} sockets; profile says {}",
                as_text(&topology["sockets"])
            ),
            Some(&json!(lscpu)),
            "",
            false,
        );
    } else {
        report.add(
            "cpu",
            "cpu.topology",
            "info",
            "CPU topology matches",
            &format!(
                "CPUs={} sockets={}",
                if cpus.is_empty() { "?" } else { cpus.as_str() },
                if sockets.is_empty() {
                    "?"
                } else {
                    sockets.as_str()
                }
            ),
            None,
            "",
            false,
        );
    }

    if cpuinfo.lines().any(|line| line.trim() == "hypervisor")
        || format!(" {cpuinfo} ").contains(" hypervisor ")
    {
        report.add(
            "cpu",
            "cpu.hypervisor_flag",
            "error",
            "x86 hypervisor CPU flag is present",
            "/proc/cpuinfo exposes the 'hypervisor' flag",
            Some(&json!(cpuinfo)),
            "",
            false,
        );
    }

    let detect = out(raw, "detect_virt");
    let detect = detect.trim();
    let detect_rc = rc(raw, "detect_virt");
    let (severity, title) = match (detect_rc, detect) {
        (0 | 1, "none") => ("info", "systemd-detect-virt found no VM"),
        (0, value) if !matches!(value, "" | "unknown") => {
            ("error", "systemd-detect-virt reports a VM")
        }
        _ => (
            "unverified",
            "systemd-detect-virt did not return usable evidence",
        ),
    };
    report.add(
        "cpu",
        "cpu.detect_virt",
        severity,
        title,
        &format!("Exit status {detect_rc}, output {}", repr_string(detect)),
        None,
        "Confirm the detector is installed and completes in the guest.",
        false,
    );

    let virt_what = out(raw, "virt_what");
    let virt_what = virt_what.trim();
    let virt_what_rc = rc(raw, "virt_what");
    let (severity, title) = match (virt_what_rc, virt_what.is_empty()) {
        (0, true) => ("info", "virt-what found no VM"),
        (0, false) => ("error", "virt-what detects a VM"),
        _ => ("unverified", "virt-what did not return usable evidence"),
    };
    report.add(
        "cpu",
        "cpu.virt_what",
        severity,
        title,
        &format!(
            "Exit status {virt_what_rc}, output {}",
            repr_string(virt_what)
        ),
        None,
        "Confirm the detector is installed and completes in the guest.",
        false,
    );

    let dmesg = format!("{}\n{}", out(raw, "dmesg"), out(raw, "journal_kernel"));
    if dmesg.to_lowercase().contains("hyp mode not available") {
        let lines: Vec<String> = dmesg
            .lines()
            .filter(|line| line.to_lowercase().contains("hyp mode"))
            .take(3)
            .map(ToOwned::to_owned)
            .collect();
        report.add(
            "cpu",
            "cpu.hyp_mode",
            "warning",
            "HVF exposes EL1 only",
            "The guest kernel logs 'kvm: HYP mode not available'; a real server at EL2 would initialize KVM.",
            Some(&json!(lines)),
            "Unavoidable with Apple HVF; keep kernel.dmesg_restrict=1. A TCG profile can expose EL2 but is much slower.",
            true,
        );
    }
    let feature_line = Regex::new(r"(?m)^Features\s*:\s*(.*)$")
        .ok()
        .and_then(|pattern| {
            pattern
                .captures(&cpuinfo)
                .map(|captures| captures[1].to_string())
        })
        .unwrap_or_default();
    let features: BTreeSet<&str> = feature_line.split_whitespace().collect();
    let extra_allowed: BTreeSet<&str> = [
        "sve", "sve2", "sme", "bf16", "ebf16", "i8mm", "afp", "rpres", "flagm2",
    ]
    .into_iter()
    .collect();
    let extra_features: Vec<&&str> = features.intersection(&extra_allowed).collect();
    if !extra_features.is_empty() {
        report.add(
            "cpu",
            "cpu.feature_superset",
            "warning",
            "CPU feature set is newer than the model name",
            &format!(
                "{} does not define: {}. HVF passes the host feature registers through and QEMU does not expose per-feature off switches for these bits.",
                if expected_model.is_empty() {
                    "The profile CPU"
                } else {
                    expected_model.as_str()
                },
                extra_features.iter().map(|item| **item).collect::<Vec<_>>().join(", ")
            ),
            Some(&json!(feature_line)),
            "Use a CPU preset that matches the host generation, or accept this residual.",
            true,
        );
    }

    Output {
        detect: detect.to_string(),
        virt_what: virt_what.to_string(),
        dmesg,
    }
}

fn parse_hex(text: &str) -> i64 {
    let stripped = text.trim();
    i64::from_str_radix(
        stripped.trim_start_matches("0x").trim_start_matches("0X"),
        16,
    )
    .unwrap_or(0)
}
