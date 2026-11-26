//! Host-side detector evaluation and report generation.
//!
//! The guest probe only collects evidence.  These rules turn that evidence into
//! a stable, per-aspect report; every finding has an id, severity, observed
//! evidence and a remediation hint so failures are reproducible.

mod cpu;
mod devices;
mod guest;
mod network;
mod shared;
mod smbios;

use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::util::now_millis;

use shared::{FindingCounts, severity_order};

pub const GROUPS: [&str; 5] = ["cpu", "smbios", "devices", "network", "guest"];

pub const DEFAULT_FORBIDDEN: [&str; 13] = [
    "QEMU",
    "KVM",
    "Bochs",
    "VirtualBox",
    "VMware",
    "Xen",
    "innotek",
    "OpenStack",
    "Amazon EC2",
    "Google Compute",
    "Microsoft Corporation",
    "Hyper-V",
    "Virtual Machine",
];

pub fn evaluate(raw: &Value, resolved: &Value, cfg: &Config) -> Value {
    let mut ctx = shared::Context::new(raw, resolved, cfg);
    let cpu_out = cpu::run(&mut ctx);
    let smbios_out = smbios::run(&mut ctx);
    let devices_out = devices::run(&mut ctx, cpu_out.dmesg);
    let network_out = network::run(&mut ctx);
    let guest_out = guest::run(&mut ctx);
    let verify = ctx.verify;
    let allow_user_network = ctx.allow_user_network;
    let report = ctx.report;

    // ------------------------------------------------------------- SUMMARY
    let mut groups = Map::new();
    let totals = FindingCounts::from_findings(&report.findings);
    for group in GROUPS {
        let mut findings: Vec<Value> = report
            .findings
            .iter()
            .filter(|item| item["group"].as_str() == Some(group))
            .cloned()
            .collect();
        let counts = FindingCounts::from_findings(&findings);
        findings.sort_by(|a, b| {
            let order = |item: &Value| severity_order(item["severity"].as_str().unwrap_or("info"));
            order(a)
                .cmp(&order(b))
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });
        groups.insert(
            group.into(),
            json!({
                "status": counts.status(), "errors": counts.errors, "warnings": counts.warnings,
                "unverified": counts.unverified, "findings": findings,
            }),
        );
    }
    let fail_on = verify["fail_on"].as_str().unwrap_or("error");
    let acceptable = totals.errors == 0
        && totals.unverified == 0
        && (totals.warnings == 0 || fail_on != "warning");
    let residuals: Vec<Value> = report
        .findings
        .iter()
        .filter(|item| {
            item.get("residual")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .map(|item| {
            json!({
                "id": item.get("id").cloned().unwrap_or(Value::Null),
                "title": item.get("title").cloned().unwrap_or(Value::Null),
                "detail": item.get("detail").cloned().unwrap_or(Value::Null),
                "evidence": item.get("evidence").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    let mut evidence_summary = Map::new();
    evidence_summary.insert("systemd_detect_virt".to_string(), json!(cpu_out.detect));
    evidence_summary.insert("virt_what".to_string(), json!(cpu_out.virt_what));
    evidence_summary.insert("dmi".to_string(), smbios_out.dmi_vendor_fields);
    evidence_summary.insert(
        "pci".to_string(),
        json!(devices_out.pci.iter().take(30).collect::<Vec<_>>()),
    );
    evidence_summary.insert(
        "root_disk".to_string(),
        devices_out.root.unwrap_or(Value::Null),
    );
    evidence_summary.insert("macs".to_string(), json!(network_out.macs));
    evidence_summary.insert("addresses".to_string(), json!(network_out.addresses));
    evidence_summary.insert("gateways".to_string(), json!(network_out.gateways));
    evidence_summary.insert("dns".to_string(), json!(network_out.dns));
    evidence_summary.insert("timezone".to_string(), json!(guest_out.timezone));
    evidence_summary.insert("locale".to_string(), json!(guest_out.effective));
    evidence_summary.insert("packages".to_string(), json!(guest_out.installed_cloud));

    json!({
        "version": 1,
        "instance": cfg.name,
        "hostname": cfg.current_hostname(),
        "username": cfg.current_username(),
        "generated_at": now_millis(),
        "profile_fingerprint": cfg.device_fingerprint(None).unwrap_or_default(),
        "lab_mode": allow_user_network,
        "status": totals.status(),
        "acceptable": acceptable,
        "fail_on": fail_on,
        "errors": totals.errors,
        "warnings": totals.warnings,
        "unverified": totals.unverified,
        "checks": report.findings.len(),
        "groups": Value::Object(groups),
        "residuals": residuals,
        "evidence_summary": Value::Object(evidence_summary),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_parsers_preserve_values_from_guest_output() {
        let text = "Handle 0x0001, DMI type 1, 27 bytes\n  Product Name: Example\nHandle 0x0002, DMI type 2, 15 bytes\n  Product Name: Board\n";
        assert_eq!(shared::field(text, "Product Name"), "Example");
        assert!(smbios::dmi_type(text, 2).contains("Product Name: Board"));
        assert!(smbios::dmi_type(text, 99).is_empty());
        let raw = json!({"files": {"/etc/resolv.conf": {"text": "# comment\n nameserver 1.1.1.1\nnameserver 8.8.8.8\n"}}});
        assert_eq!(network::dns_servers(&raw), ["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn gateway_network_warnings_and_missing_evidence_are_reported() {
        let cfg = crate::config::build_config(
            &json!({"network_mode": "gateway", "device": {"enabled": true}}),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .unwrap();
        let resolved = cfg.device_resolved().unwrap();
        let raw = json!({
            "acpi": {"_error": "access denied"},
            "commands": {"lspci": {"rc": 1}},
            "pci": ["00:00.0 PCI bridge"],
            "net": {
                "interfaces": [{"ifname":"data0", "address":"52:54:00:0a:03:0f"}],
                "addresses": [{"ifname":"mgmt0", "addr_info":[{"family":"inet", "local":"10.0.2.15"}]}]
            }
        });
        let report = evaluate(&raw, &resolved, &cfg);
        let findings = report["groups"]["network"]["findings"].as_array().unwrap();
        for id in ["network.mac", "network.slirp_ip"] {
            let finding = findings.iter().find(|finding| finding["id"] == id).unwrap();
            assert_eq!(finding["severity"], "warning", "{id}");
        }
        for (group, id) in [
            ("cpu", "cpu.detect_virt"),
            ("cpu", "cpu.virt_what"),
            ("smbios", "smbios.acpi_evidence"),
            ("devices", "devices.virtio"),
        ] {
            let findings = report["groups"][group]["findings"].as_array().unwrap();
            let finding = findings.iter().find(|finding| finding["id"] == id).unwrap();
            assert_eq!(finding["severity"], "unverified", "{id}");
        }
        assert_eq!(report["acceptable"], false);
    }

    fn run_groups() -> (Value, Vec<(&'static str, Vec<Value>)>) {
        let cfg = crate::config::build_config(
            &json!({"network_mode": "gateway", "device": {"enabled": true}}),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .unwrap();
        let resolved = cfg.device_resolved().unwrap();
        let raw = json!({
            "acpi": {"_error": "access denied"},
            "commands": {"lspci": {"rc": 1}},
            "pci": ["00:00.0 PCI bridge"],
            "net": {
                "interfaces": [{"ifname":"data0", "address":"52:54:00:0a:03:0f"}],
                "addresses": [{"ifname":"mgmt0", "addr_info":[{"family":"inet", "local":"10.0.2.15"}]}]
            }
        });
        let full = evaluate(&raw, &resolved, &cfg);

        let mut ctx = shared::Context::new(&raw, &resolved, &cfg);
        let mut by_group: Vec<(&'static str, Vec<Value>)> = Vec::new();
        let mut mark = ctx.report.findings.len();
        let cpu_out = cpu::run(&mut ctx);
        by_group.push(("cpu", ctx.report.findings[mark..].to_vec()));
        mark = ctx.report.findings.len();
        let _smbios_out = smbios::run(&mut ctx);
        by_group.push(("smbios", ctx.report.findings[mark..].to_vec()));
        mark = ctx.report.findings.len();
        let _devices_out = devices::run(&mut ctx, cpu_out.dmesg);
        by_group.push(("devices", ctx.report.findings[mark..].to_vec()));
        mark = ctx.report.findings.len();
        let _network_out = network::run(&mut ctx);
        by_group.push(("network", ctx.report.findings[mark..].to_vec()));
        mark = ctx.report.findings.len();
        let _guest_out = guest::run(&mut ctx);
        by_group.push(("guest", ctx.report.findings[mark..].to_vec()));
        (full, by_group)
    }

    /// Assert that one group's findings are a subset of the full report and that all
    /// groups together reproduce the full report exactly.
    fn assert_group_reproduces_report(
        full: &Value,
        by_group: &mut [(&'static str, Vec<Value>)],
        group: &str,
    ) {
        let mut total = 0;
        for (name, findings) in by_group.iter_mut() {
            let expected = full["groups"][*name]["findings"].as_array().unwrap();
            if *name == group {
                for finding in findings.iter() {
                    assert!(expected.contains(finding), "{name}: {}", finding["id"]);
                }
            }
            findings.sort_by(|a, b| {
                let order = |item: &Value| {
                    shared::severity_order(item["severity"].as_str().unwrap_or("info"))
                };
                order(a)
                    .cmp(&order(b))
                    .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
            });
            assert_eq!(findings, expected, "{name} findings differ");
            total += findings.len();
        }
        assert_eq!(full["checks"].as_u64().unwrap() as usize, total);
    }

    #[test]
    fn cpu_group_findings_match_the_full_report() {
        let (full, mut by_group) = run_groups();
        assert_group_reproduces_report(&full, &mut by_group, "cpu");
    }

    #[test]
    fn smbios_group_findings_match_the_full_report() {
        let (full, mut by_group) = run_groups();
        assert_group_reproduces_report(&full, &mut by_group, "smbios");
    }

    #[test]
    fn devices_group_findings_match_the_full_report() {
        let (full, mut by_group) = run_groups();
        assert_group_reproduces_report(&full, &mut by_group, "devices");
    }

    #[test]
    fn network_group_findings_match_the_full_report() {
        let (full, mut by_group) = run_groups();
        assert_group_reproduces_report(&full, &mut by_group, "network");
    }

    #[test]
    fn guest_group_findings_match_the_full_report() {
        let (full, mut by_group) = run_groups();
        assert_group_reproduces_report(&full, &mut by_group, "guest");
    }
}
