//! Host-side detector evaluation and report generation.
//!
//! The guest probe only collects evidence.  These rules turn that evidence into
//! a stable, per-aspect report; every finding has an id, severity, observed
//! evidence and a remediation hint so failures are reproducible.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::config::Config;
use crate::py_json;
use crate::util::now_millis;

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

fn severity_order(severity: &str) -> u8 {
    match severity {
        "error" => 0,
        "unverified" => 1,
        "warning" => 2,
        _ => 3,
    }
}

#[derive(Default)]
struct FindingCounts {
    errors: usize,
    warnings: usize,
    unverified: usize,
}

impl FindingCounts {
    fn from_findings(findings: &[Value]) -> Self {
        let mut counts = Self::default();
        for item in findings {
            match item["severity"].as_str() {
                Some("error") => counts.errors += 1,
                Some("warning") => counts.warnings += 1,
                Some("unverified") => counts.unverified += 1,
                _ => {},
            }
        }
        counts
    }

    fn status(&self) -> &'static str {
        match (self.errors, self.unverified, self.warnings) {
            (0, 0, 0) => "pass",
            (0, 0, _) => "warn",
            (0, _, _) => "unverified",
            _ => "fail",
        }
    }
}

struct Report {
    findings: Vec<Value>,
}

impl Report {
    fn new() -> Self {
        Self {
            findings: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        group: &str,
        check_id: &str,
        severity: &str,
        title: &str,
        detail: &str,
        evidence: Option<&Value>,
        remediation: &str,
        residual: bool,
    ) {
        let evidence_text = match evidence {
            Some(value @ (Value::Object(_) | Value::Array(_))) => py_json::dumps_pretty(value),
            Some(Value::Null) | None => String::new(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        let evidence_text = truncate_evidence(&evidence_text);
        self.findings.push(json!({
            "group": group,
            "id": check_id,
            "severity": severity,
            "title": title,
            "detail": detail,
            "evidence": evidence_text,
            "remediation": remediation,
            "residual": residual,
        }));
    }
}

fn truncate_evidence(text: &str) -> String {
    if text.chars().count() > 2400 {
        let mut truncated: String = text.chars().take(2400).collect();
        truncated.push_str("\n... (truncated)");
        truncated
    } else {
        text.to_string()
    }
}

fn cmd<'a>(raw: &'a Value, name: &str) -> &'a Value {
    raw.get("commands")
        .and_then(|value| value.get(name))
        .unwrap_or(&Value::Null)
}

fn out(raw: &Value, name: &str) -> String {
    cmd(raw, name)
        .get("out")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn rc(raw: &Value, name: &str) -> i64 {
    cmd(raw, name)
        .get("rc")
        .and_then(Value::as_i64)
        .unwrap_or(127)
}

fn file_entry<'a>(raw: &'a Value, path: &str) -> &'a Value {
    raw.get("files")
        .and_then(|value| value.get(path))
        .unwrap_or(&Value::Null)
}

fn file_text(raw: &Value, path: &str) -> String {
    file_entry(raw, path)
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn dmi(raw: &Value, key: &str) -> String {
    raw.get("dmi")
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn field(text: &str, label: &str) -> String {
    let Ok(pattern) = Regex::new(&format!(r"^\s*{}\s*:\s*(.*)$", regex::escape(label))) else {
        return String::new();
    };
    for line in text.lines() {
        if let Some(captures) = pattern.captures(line) {
            return captures[1].trim().to_string();
        }
    }
    String::new()
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

fn dmi_type(text: &str, number: i64) -> String {
    let mut chunks: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if line.starts_with("Handle 0x") && line.contains(", DMI type ") {
            if !current.is_empty() {
                chunks.push(current);
            }
            current = String::new();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    let Ok(type_re) = Regex::new(r"DMI type (\d+)") else {
        return String::new();
    };
    for chunk in chunks {
        if let Some(captures) = type_re.captures(&chunk)
            && captures[1].parse::<i64>().ok() == Some(number)
        {
            return chunk;
        }
    }
    String::new()
}

fn flatten_lsblk(raw: &Value) -> Vec<Value> {
    let text = out(raw, "lsblk");
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    fn walk(items: &[Value], parent: Option<&str>, result: &mut Vec<Value>) {
        for item in items {
            let mut node = item.as_object().cloned().unwrap_or_default();
            node.insert(
                "parent".to_string(),
                parent
                    .map(|value| Value::String(value.to_string()))
                    .unwrap_or(Value::Null),
            );
            let name = node
                .get("name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            result.push(Value::Object(node.clone()));
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                walk(children, name.as_deref(), result);
            }
        }
    }
    if let Some(items) = data.get("blockdevices").and_then(Value::as_array) {
        walk(items, None, &mut result);
    }
    result
}

fn root_device(raw: &Value) -> Option<Value> {
    let source = out(raw, "findmnt_root");
    let first = source.split_whitespace().next()?;
    let name = first.replace("/dev/", "");
    let devices = flatten_lsblk(raw);
    let node = devices
        .iter()
        .find(|device| device.get("name").and_then(Value::as_str) == Some(name.as_str()))
        .cloned();
    if let Some(node) = node {
        if node.get("type").and_then(Value::as_str) == Some("part")
            && let Some(parent) = node.get("parent").and_then(Value::as_str)
            && let Some(parent_node) = devices
                .iter()
                .find(|device| device.get("name").and_then(Value::as_str) == Some(parent))
        {
            return Some(parent_node.clone());
        }
        return Some(node);
    }
    for device in &devices {
        if let Some(children) = device.get("children").and_then(Value::as_array)
            && children
                .iter()
                .any(|child| child.get("name").and_then(Value::as_str) == Some(name.as_str()))
        {
            return Some(device.clone());
        }
    }
    devices.first().cloned()
}

fn pci_lines(raw: &Value) -> Vec<String> {
    raw.get("pci")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|line| !line.trim().is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn forbidden(patterns: &[String], text: &str) -> Vec<String> {
    let low = text.to_lowercase();
    patterns
        .iter()
        .filter(|pattern| low.contains(&pattern.to_lowercase()))
        .cloned()
        .collect()
}

fn iface_macs(raw: &Value) -> Map<String, Value> {
    let mut result = Map::new();
    if let Some(items) = raw
        .get("net")
        .and_then(|value| value.get("interfaces"))
        .and_then(Value::as_array)
    {
        for item in items {
            let name = item.get("ifname").and_then(Value::as_str).unwrap_or("");
            let address = item.get("address").and_then(Value::as_str).unwrap_or("");
            if !name.is_empty() && !address.is_empty() && name != "lo" {
                result.insert(name.to_string(), json!(address.to_lowercase()));
            }
        }
    }
    result
}

fn iface_addresses(raw: &Value, family: &str) -> Vec<(String, String)> {
    let mut result = Vec::new();
    if let Some(items) = raw
        .get("net")
        .and_then(|value| value.get("addresses"))
        .and_then(Value::as_array)
    {
        for item in items {
            let name = item.get("ifname").and_then(Value::as_str).unwrap_or("");
            if let Some(infos) = item.get("addr_info").and_then(Value::as_array) {
                for info in infos {
                    if info.get("family").and_then(Value::as_str) == Some(family)
                        && let Some(local) = info.get("local").and_then(Value::as_str)
                    {
                        result.push((name.to_string(), local.to_string()));
                    }
                }
            }
        }
    }
    result
}

fn routes(raw: &Value) -> Vec<Value> {
    raw.get("net")
        .and_then(|value| value.get("routes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn default_gateways(raw: &Value) -> Vec<String> {
    let mut values = Vec::new();
    for route in routes(raw) {
        let dst = route.get("dst").and_then(Value::as_str).unwrap_or("");
        if (dst == "default"
            || dst == "0.0.0.0/0"
            || route.get("dst").map(Value::is_null).unwrap_or(true))
            && route.get("gateway").is_some_and(|value| !value.is_null())
        {
            values.push(route["gateway"].to_string().trim_matches('"').to_string());
        }
    }
    values
}

fn dns_servers(raw: &Value) -> Vec<String> {
    let text = file_text(raw, "/etc/resolv.conf");
    let Ok(re) = Regex::new(r"^\s*nameserver\s+(\S+)") else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for line in text.lines() {
        if let Some(captures) = re.captures(line) {
            values.push(captures[1].to_string());
        }
    }
    values
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

fn has_virtio_line(text: &str) -> bool {
    let low = text.to_lowercase();
    low.contains("virtio") || low.contains("qemu")
}

fn json_str(value: &Value) -> String {
    py_json::dumps_default(value, true)
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
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

fn valid_uuid(text: &str) -> bool {
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 5
        || parts.iter().map(|part| part.len()).collect::<Vec<_>>() != vec![8, 4, 4, 4, 12]
        || !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        return false;
    }
    let compact: String = parts.concat();
    let numeric = u128::from_str_radix(&compact, 16).unwrap_or(0);
    numeric != 0 && numeric != u128::MAX
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn repr_string(text: &str) -> String {
    py_json::py_repr_string(text)
}

fn repr_tuples(values: &[(String, String)]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|(name, address)| format!("({}, {})", repr_string(name), repr_string(address)))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn onoff(value: &str) -> Option<bool> {
    match value.trim().to_lowercase().as_str() {
        "yes" | "true" | "on" | "1" => Some(true),
        "no" | "false" | "off" | "0" => Some(false),
        _ => None,
    }
}

pub fn evaluate(raw: &Value, resolved: &Value, cfg: &Config) -> Value {
    let mut report = Report::new();
    let verify = resolved.get("verify").unwrap_or(&Value::Null);
    let patterns: Vec<String> = match verify.get("forbidden_strings").and_then(Value::as_array) {
        Some(items) if !items.is_empty() => items
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => DEFAULT_FORBIDDEN
            .iter()
            .map(|item| item.to_string())
            .collect(),
    };
    let allow_user_network = verify
        .get("allow_user_network")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let network = resolved.get("network").unwrap_or(&Value::Null);
    let smbios = resolved.get("smbios").unwrap_or(&Value::Null);
    let cpu = resolved.get("cpu").unwrap_or(&Value::Null);
    let guest = resolved.get("guest").unwrap_or(&Value::Null);
    let devices = resolved.get("devices").unwrap_or(&Value::Null);
    let topology = cpu.get("topology").unwrap_or(&Value::Null);
    let midr = cpu.get("midr_int").and_then(Value::as_u64).unwrap_or(0) as i64;
    let expected_model = cpu
        .get("model_name")
        .or_else(|| cpu.get("model"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let dmi_text = out(raw, "dmidecode");
    let lscpu = out(raw, "lscpu");
    let cpuinfo = out(raw, "cpuinfo");

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
        (0, value) if !matches!(value, "" | "unknown") => ("error", "systemd-detect-virt reports a VM"),
        _ => ("unverified", "systemd-detect-virt did not return usable evidence"),
    };
    report.add(
        "cpu", "cpu.detect_virt", severity, title,
        &format!("Exit status {detect_rc}, output {}", repr_string(detect)),
        None, "Confirm the detector is installed and completes in the guest.", false,
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
        "cpu", "cpu.virt_what", severity, title,
        &format!("Exit status {virt_what_rc}, output {}", repr_string(virt_what)),
        None, "Confirm the detector is installed and completes in the guest.", false,
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

    // --------------------------------------------------------------- SMBIOS
    let dmi_vendor_fields = json!({
        "sys_vendor": dmi(raw, "sys_vendor"),
        "product_name": dmi(raw, "product_name"),
        "board_vendor": dmi(raw, "board_vendor"),
        "chassis_vendor": dmi(raw, "chassis_vendor"),
    });
    let mut hits = Vec::new();
    if let Some(map) = dmi_vendor_fields.as_object() {
        for (field_name, value) in map {
            let text = value.as_str().unwrap_or("");
            let matched = forbidden(&patterns, text);
            if !matched.is_empty() {
                hits.push(json!({"field": field_name, "value": text, "matched": matched}));
            }
        }
    }
    if !hits.is_empty() {
        report.add(
            "smbios",
            "smbios.vendor",
            "error",
            "DMI contains a hypervisor vendor string",
            "One or more DMI vendor fields match a forbidden hypervisor signature.",
            Some(&json!(hits)),
            "Set device.smbios.system/baseboard/chassis manufacturer to a real OEM and regenerate SMBIOS.",
            false,
        );
    } else {
        report.add(
            "smbios",
            "smbios.vendor",
            "info",
            "DMI vendor strings are clean",
            &json_str(&dmi_vendor_fields),
            Some(&dmi_vendor_fields),
            "",
            false,
        );
    }

    let serial_fields = json!({
        "product_serial": dmi(raw, "product_serial"),
        "board_serial": dmi(raw, "board_serial"),
        "chassis_serial": dmi(raw, "chassis_serial"),
    });
    let bad_serial_values = [
        "none",
        "not specified",
        "to be filled by o.e.m.",
        "system serial number",
        "default string",
        "0",
        "unknown",
    ];
    let mut bad_serials = Map::new();
    if let Some(map) = serial_fields.as_object() {
        for (field_name, value) in map {
            let text = value.as_str().unwrap_or("");
            if text.is_empty() || bad_serial_values.contains(&text.to_lowercase().as_str()) {
                bad_serials.insert(field_name.clone(), value.clone());
            }
        }
    }
    if !bad_serials.is_empty() {
        report.add(
            "smbios",
            "smbios.serials",
            "error",
            "DMI serial numbers are missing or generic",
            "A real machine has unique, non-placeholder serials.",
            Some(&Value::Object(bad_serials.clone())),
            "Set device.smbios.*.serial values or leave them null for deterministic generation.",
            false,
        );
    } else {
        report.add(
            "smbios",
            "smbios.serials",
            "info",
            "DMI serial numbers are populated",
            &json_str(&serial_fields),
            Some(&serial_fields),
            "",
            false,
        );
    }

    let product_uuid = dmi(raw, "product_uuid");
    if !valid_uuid(&product_uuid) {
        report.add(
            "smbios",
            "smbios.uuid",
            "error",
            "DMI system UUID is missing or a sentinel",
            &format!("product_uuid={}", repr_string(&product_uuid)),
            Some(&json!(product_uuid)),
            "",
            false,
        );
    } else {
        report.add(
            "smbios",
            "smbios.uuid",
            "info",
            "DMI system UUID is valid",
            &product_uuid,
            Some(&json!(product_uuid)),
            "",
            false,
        );
    }

    if dmi_text
        .to_lowercase()
        .contains("system is a virtual machine")
    {
        let evidence = dmi_type(&dmi_text, 0);
        report.add(
            "smbios",
            "smbios.vm_bit",
            "error",
            "SMBIOS BIOS characteristics says 'virtual machine'",
            "The type 0 BIOS characteristics bit 44 is set.",
            Some(&json!(evidence)),
            "Use the generated SMBIOS table (device.smbios.enabled=true).",
            false,
        );
    } else {
        report.add(
            "smbios",
            "smbios.vm_bit",
            "info",
            "SMBIOS virtual-machine bit is clear",
            "dmidecode type 0 has no 'System is a virtual machine' bit.",
            None,
            "",
            false,
        );
    }

    let chassis = dmi(raw, "chassis_type");
    let expected_chassis = smbios
        .get("chassis")
        .and_then(|value| value.get("type"))
        .map(as_text)
        .unwrap_or_default();
    let hostnamectl = out(raw, "hostnamectl");
    if chassis == "1" || chassis == "2" {
        report.add(
            "smbios",
            "smbios.chassis",
            "error",
            "Chassis type is Other/Unknown",
            &format!("chassis_type={chassis}"),
            Some(&json!(hostnamectl)),
            "Set device.smbios.chassis.type to 17 (rack mount) or 11 (main server).",
            false,
        );
    } else if !expected_chassis.is_empty() && !chassis.is_empty() && chassis != expected_chassis {
        report.add(
            "smbios",
            "smbios.chassis",
            "warning",
            "Chassis type differs from the profile",
            &format!("chassis_type={chassis}; profile says {expected_chassis}"),
            Some(&json!(hostnamectl)),
            "",
            false,
        );
    } else {
        report.add(
            "smbios",
            "smbios.chassis",
            "info",
            "Chassis type is realistic",
            &format!(
                "chassis_type={}",
                if chassis.is_empty() {
                    "?"
                } else {
                    chassis.as_str()
                }
            ),
            Some(&json!(hostnamectl)),
            "",
            false,
        );
    }
    let hostnamectl_low = hostnamectl.to_lowercase();
    if hostnamectl_low.contains("chassis: vm") || hostnamectl_low.contains("computer-vm") {
        report.add(
            "smbios",
            "smbios.hostnamectl",
            "error",
            "hostnamectl identifies a VM chassis",
            &hostnamectl,
            Some(&json!(hostnamectl)),
            "",
            false,
        );
    }

    let system = smbios.get("system").unwrap_or(&Value::Null);
    let expected_manufacturer = system
        .get("manufacturer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let expected_product = system
        .get("product")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let sys_vendor = dmi(raw, "sys_vendor");
    let product_name = dmi(raw, "product_name");
    if !expected_manufacturer.is_empty()
        && !sys_vendor
            .to_lowercase()
            .contains(&expected_manufacturer.to_lowercase())
    {
        report.add(
            "smbios",
            "smbios.system_expected",
            "error",
            "System vendor does not match the profile",
            &format!(
                "sys_vendor={}; expected {}",
                repr_string(&sys_vendor),
                repr_string(&expected_manufacturer)
            ),
            Some(&json!(dmi_type(&dmi_text, 1))),
            "",
            false,
        );
    } else if !expected_product.is_empty()
        && !product_name
            .to_lowercase()
            .contains(&expected_product.to_lowercase())
    {
        report.add(
            "smbios",
            "smbios.system_expected",
            "error",
            "System product does not match the profile",
            &format!(
                "product_name={}; expected {}",
                repr_string(&product_name),
                repr_string(&expected_product)
            ),
            Some(&json!(dmi_type(&dmi_text, 1))),
            "",
            false,
        );
    } else {
        report.add(
            "smbios",
            "smbios.system_expected",
            "info",
            "System product matches the profile",
            &format!("{expected_manufacturer} / {expected_product}"),
            None,
            "",
            false,
        );
    }

    let type4 = dmi_type(&dmi_text, 4);
    let family = field(&type4, "Family");
    let signature = field(&type4, "Signature");
    if !family.is_empty() && family != "ARMv8" {
        report.add(
            "smbios",
            "smbios.processor",
            "warning",
            "SMBIOS processor family differs",
            &format!("Family: {family}; expected ARMv8"),
            Some(&json!(type4)),
            "",
            false,
        );
    } else if midr != 0 && !signature.is_empty() {
        let exp_impl = (midr >> 24) & 0xFF;
        let exp_part = (midr >> 4) & 0xFFF;
        if !signature.contains(&format!("Implementor 0x{exp_impl:02x}"))
            || !signature.contains(&format!("Part 0x{exp_part:03x}"))
        {
            report.add(
                "smbios",
                "smbios.processor",
                "warning",
                "SMBIOS processor signature differs from MIDR",
                &format!("Signature: {signature}"),
                Some(&json!(type4)),
                "",
                false,
            );
        } else {
            report.add(
                "smbios",
                "smbios.processor",
                "info",
                "SMBIOS processor signature matches MIDR",
                &signature,
                None,
                "",
                false,
            );
        }
    } else if !type4.is_empty() {
        report.add(
            "smbios",
            "smbios.processor",
            "info",
            "SMBIOS processor structure present",
            &family,
            None,
            "",
            false,
        );
    }

    let type17 = dmi_type(&dmi_text, 17);
    let memory_type = field(&type17, "Type");
    let locator = field(&type17, "Locator");
    if !type17.is_empty()
        && (memory_type == "Unknown"
            || memory_type == "RAM"
            || locator == "DIMM 0"
            || locator == "Not Specified"
            || locator.is_empty())
    {
        report.add(
            "smbios",
            "smbios.memory",
            "warning",
            "SMBIOS memory device looks generic",
            &format!(
                "Type={} Locator={}",
                repr_string(&memory_type),
                repr_string(&locator)
            ),
            Some(&json!(type17)),
            "Use the generated SMBIOS table and set memory.memory_type=26 (DDR4), memory.form_factor=9 (DIMM) and a real locator prefix.",
            false,
        );
    } else if !type17.is_empty() {
        report.add(
            "smbios",
            "smbios.memory",
            "info",
            "SMBIOS memory device is detailed",
            &format!("Type={memory_type} Locator={locator}"),
            Some(&json!(type17)),
            "",
            false,
        );
    }

    let acpi_hits = raw
        .get("acpi_hits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let hard_tokens = [
        "bochs",
        "kvm",
        "virtualbox",
        "vmware",
        "xen",
        "innotek",
        "virtual machine",
    ];
    let hard_acpi_hits: Vec<Value> = acpi_hits
        .iter()
        .filter(|hit| {
            let needle = hit
                .get("needle")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            hard_tokens.iter().any(|token| needle.contains(token))
        })
        .cloned()
        .collect();
    let qemu_acpi_hits: Vec<Value> = acpi_hits
        .iter()
        .filter(|hit| !hard_acpi_hits.contains(hit))
        .cloned()
        .collect();
    let tables_read = raw["acpi"].as_object().into_iter().flat_map(Map::values)
        .filter(|table| table["strings"].is_array()).count();
    if tables_read == 0 {
        report.add(
            "smbios", "smbios.acpi_evidence", "unverified", "No ACPI tables were read",
            "The guest did not supply raw ACPI tables for this verification.",
            None, "Check firmware configuration and guest read permissions.", false,
        );
    }
    if !hard_acpi_hits.is_empty() {
        report.add(
            "smbios",
            "smbios.acpi",
            "error",
            "ACPI/SMBIOS strings contain hypervisor signatures",
            "The raw SMBIOS/ACPI tables contain a forbidden hypervisor string.",
            Some(&json!(hard_acpi_hits)),
            "Override -machine x-oem-id/x-oem-table-id and regenerate the SMBIOS table.",
            false,
        );
    } else if !qemu_acpi_hits.is_empty() {
        let evidence: Vec<Value> = qemu_acpi_hits.iter().take(6).cloned().collect();
        report.add(
            "smbios",
            "smbios.acpi",
            "warning",
            "ACPI DSDT still contains QEMU platform identifiers",
            "The virt machine DSDT contains the QEMU0002 fw_cfg ACPI device. The driver/module and /sys/firmware/qemu_fw_cfg are removed at boot, but the ACPI namespace identifier cannot be changed without rebuilding QEMU.",
            Some(&json!(evidence)),
            "Documented residual; the common lsmod/sysfs checks are clean.",
            true,
        );
    } else if tables_read > 0 {
        report.add(
            "smbios", "smbios.acpi", "info", "No configured signatures in the ACPI scan",
            &format!("{tables_read} ACPI tables scanned"), None, "", false,
        );
    }
    let oem = smbios.get("oem").unwrap_or(&Value::Null);
    let mut acpi_oem = Map::new();
    if let Some(acpi) = raw.get("acpi").and_then(Value::as_object) {
        for (name, info) in acpi {
            if name.starts_with('_') || !info.is_object() {
                continue;
            }
            acpi_oem.insert(
                name.clone(),
                json!({
                    "oem_id": info.get("oem_id").cloned().unwrap_or(Value::Null),
                    "oem_table_id": info.get("oem_table_id").cloned().unwrap_or(Value::Null),
                }),
            );
        }
    }
    let expected_oem_id = oem
        .get("acpi_oem_id")
        .and_then(Value::as_str)
        .unwrap_or("Ampere");
    let mut bad_oem = Map::new();
    for (name, value) in &acpi_oem {
        let oem_id = value.get("oem_id").and_then(Value::as_str).unwrap_or("");
        if !oem_id.is_empty() && oem_id != expected_oem_id {
            bad_oem.insert(name.clone(), value.clone());
        }
    }
    if !bad_oem.is_empty() {
        report.add(
            "smbios",
            "smbios.acpi_oem",
            "warning",
            "Some ACPI tables use a different OEM ID",
            "The ACPI header OEM ID should match the profile.",
            Some(&Value::Object(bad_oem)),
            "",
            false,
        );
    } else if acpi_oem.values().any(|info| info["oem_id"].as_str().is_some_and(|id| !id.is_empty())) {
        let ids: Map<String, Value> = acpi_oem
            .iter()
            .map(|(name, value)| {
                (
                    name.clone(),
                    value.get("oem_id").cloned().unwrap_or(Value::Null),
                )
            })
            .collect();
        report.add(
            "smbios",
            "smbios.acpi_oem",
            "info",
            "ACPI OEM IDs match the profile",
            &py_json::dumps_default(&Value::Object(ids), true),
            Some(&Value::Object(acpi_oem)),
            "",
            false,
        );
    }

    // -------------------------------------------------------------- DEVICES
    let pci = pci_lines(raw);
    let virtio: Vec<String> = pci
        .iter()
        .filter(|line| line.contains("1af4:") || line.to_lowercase().contains("virtio"))
        .cloned()
        .collect();
    let qemu: Vec<String> = pci
        .iter()
        .filter(|line| line.to_lowercase().contains("qemu"))
        .cloned()
        .collect();
    let host_bridge: Vec<String> = pci
        .iter()
        .filter(|line| line.contains("1b36:0008"))
        .cloned()
        .collect();
    let other_1b36: Vec<String> = pci
        .iter()
        .filter(|line| line.contains("1b36:") && !line.contains("1b36:0008"))
        .cloned()
        .collect();
    if !virtio.is_empty() {
        report.add(
            "devices",
            "devices.virtio",
            "error",
            "Virtio PCI devices are visible",
            "The guest sees one or more virtio PCI functions.",
            Some(&json!(virtio)),
            "Use the runtime phase (device-seal) and the NVMe/AHCI device profile.",
            false,
        );
    } else if pci.is_empty() || rc(raw, "lspci") != 0 {
        report.add(
            "devices", "devices.virtio", "unverified", "PCI devices were not fully observed",
            "The guest did not supply a successful PCI device listing.",
            None, "Confirm lspci works in the guest.", false,
        );
    } else {
        report.add(
            "devices",
            "devices.virtio",
            "info",
            "No virtio PCI devices",
            &format!("{} PCI devices", pci.len()),
            None,
            "",
            false,
        );
    }
    if !qemu.is_empty() {
        report.add(
            "devices",
            "devices.qemu_strings",
            "error",
            "QEMU strings appear in lspci",
            "The local PCI database or a device name contains QEMU.",
            Some(&json!(qemu)),
            "The alignment script patches pci.ids; verify /usr/share/misc/pci.ids.",
            false,
        );
    }
    if !other_1b36.is_empty() {
        report.add(
            "devices",
            "devices.qemu_ids",
            "error",
            "QEMU PCI vendor IDs are visible",
            "A PCI function uses the Red Hat/QEMU vendor ID 1b36.",
            Some(&json!(other_1b36)),
            "",
            false,
        );
    }
    if !host_bridge.is_empty() {
        report.add(
            "devices",
            "devices.host_bridge",
            "warning",
            "QEMU PCIe host bridge is visible",
            "The virt machine root complex is 1b36:0008. Its numeric ID cannot be changed without rebuilding QEMU; pci.ids hides the QEMU name.",
            Some(&json!(host_bridge)),
            "Documented residual. The host bridge is a platform device, not a disk/NIC.",
            true,
        );
    }

    let root = root_device(raw);
    let lsblk_text = out(raw, "lsblk");
    if let Some(root) = &root {
        let model = root
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let serial = root
            .get("serial")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let tran = root
            .get("tran")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !forbidden(&patterns, &model).is_empty() || !forbidden(&patterns, &serial).is_empty() {
            report.add(
                "devices",
                "devices.storage_model",
                "error",
                "Root disk model/serial contains a VM signature",
                &format!(
                    "model={} serial={}",
                    repr_string(&model),
                    repr_string(&serial)
                ),
                Some(root),
                "",
                false,
            );
        } else if tran == "virtio" {
            report.add(
                "devices",
                "devices.storage_model",
                "error",
                "Root disk uses the virtio transport",
                &format!("transport={tran}"),
                Some(root),
                "",
                false,
            );
        } else if model.is_empty() || serial.is_empty() {
            report.add(
                "devices",
                "devices.storage_model",
                "warning",
                "Root disk model/serial is incomplete",
                &format!(
                    "model={} serial={}",
                    repr_string(&model),
                    repr_string(&serial)
                ),
                Some(root),
                "",
                false,
            );
        } else {
            report.add(
                "devices",
                "devices.storage_model",
                "info",
                "Root disk identity is realistic",
                &format!("{model} / {serial} ({tran})"),
                Some(root),
                "",
                false,
            );
        }
    } else {
        report.add(
            "devices",
            "devices.storage_model",
            "error",
            "Cannot identify the root block device",
            "findmnt / or lsblk returned no usable device",
            Some(&json!(lsblk_text)),
            "",
            false,
        );
    }

    let flattened = flatten_lsblk(raw);
    let optical: Vec<Value> = flattened
        .iter()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("rom")
                || item
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| name.starts_with("sr"))
                    .unwrap_or(false)
        })
        .cloned()
        .collect();
    let optical_cfg = devices.get("optical").unwrap_or(&Value::Null);
    if optical_cfg
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        if optical.is_empty() {
            report.add(
                "devices",
                "devices.optical",
                "warning",
                "Optical drive is not visible",
                "The profile enables an optical drive but no /dev/sr* device was found.",
                Some(&json!(lsblk_text)),
                "",
                false,
            );
        } else {
            let bad: Vec<Value> = optical
                .iter()
                .filter(|item| {
                    !forbidden(
                        &patterns,
                        item.get("model").and_then(Value::as_str).unwrap_or(""),
                    )
                    .is_empty()
                })
                .cloned()
                .collect();
            if !bad.is_empty() {
                report.add(
                    "devices",
                    "devices.optical",
                    "error",
                    "Optical drive model contains a VM signature",
                    &py_json::dumps_pretty_with(&json!(bad), false),
                    Some(&json!(bad)),
                    "",
                    false,
                );
            } else {
                report.add(
                    "devices",
                    "devices.optical",
                    "info",
                    "Optical drive is present with a real model",
                    &py_json::dumps_pretty_with(&optical[0], false),
                    Some(&optical[0]),
                    "",
                    false,
                );
            }
        }
    } else {
        report.add(
            "devices",
            "devices.optical",
            "info",
            "Optical drive disabled by profile",
            "no /dev/sr* expected",
            None,
            "",
            false,
        );
    }

    let usb_out = out(raw, "lsusb");
    let usb_bad: Vec<String> = usb_out
        .lines()
        .filter(|line| !forbidden(&patterns, line).is_empty())
        .map(ToOwned::to_owned)
        .collect();
    let usb_controller: Vec<String> = pci
        .iter()
        .filter(|line| line.contains("0c03"))
        .cloned()
        .collect();
    if !usb_bad.is_empty() {
        report.add(
            "devices",
            "devices.usb",
            "error",
            "USB descriptors contain a VM signature",
            "lsusb lists a QEMU/VirtualBox USB device.",
            Some(&json!(usb_bad)),
            "",
            false,
        );
    } else if usb_controller.is_empty() {
        report.add(
            "devices",
            "devices.usb",
            "warning",
            "No USB controller visible",
            "lspci has no USB controller",
            Some(&json!(pci)),
            "",
            false,
        );
    } else {
        report.add(
            "devices",
            "devices.usb",
            "info",
            "USB controller is a real model",
            &usb_controller
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
            Some(&json!(truncate_chars(&usb_out, 3000))),
            "",
            false,
        );
    }

    let sound_cfg = devices.get("sound").unwrap_or(&Value::Null);
    let audio_pci: Vec<String> = pci
        .iter()
        .filter(|line| {
            line.contains("0403")
                || line.contains("0401")
                || line.to_lowercase().contains("virtio-sound")
        })
        .cloned()
        .collect();
    let aplay = out(raw, "aplay");
    if sound_cfg
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        if audio_pci
            .iter()
            .any(|line| line.to_lowercase().contains("virtio-sound"))
        {
            report.add(
                "devices",
                "devices.sound",
                "error",
                "Virtio sound device is visible",
                &audio_pci.join("\n"),
                Some(&json!(audio_pci)),
                "",
                false,
            );
        } else if audio_pci.is_empty() {
            report.add(
                "devices",
                "devices.sound",
                "warning",
                "No audio controller visible",
                "The profile enables sound but lspci has no audio function.",
                Some(&json!(pci)),
                "",
                false,
            );
        } else if aplay.to_lowercase().contains("no soundcards found")
            || !Regex::new(r"(?m)^card \d+:").is_ok_and(|pattern| pattern.is_match(&aplay))
        {
            report.add(
                "devices",
                "devices.sound",
                "warning",
                "HDA controller has no loaded driver",
                "lspci shows an Intel HDA function but aplay reports no card. Install linux-modules-extra for the running kernel.",
                Some(&json!({"pci": audio_pci, "aplay": aplay})),
                "",
                false,
            );
        } else {
            report.add(
                "devices",
                "devices.sound",
                "info",
                "Sound card is present with a real driver",
                &audio_pci.join("\n"),
                Some(&json!(truncate_chars(&aplay, 2000))),
                "",
                false,
            );
        }
    } else {
        report.add(
            "devices",
            "devices.sound",
            "info",
            "Sound disabled by profile",
            "no audio function expected",
            None,
            "",
            false,
        );
    }

    if has_virtio_line(&dmesg) {
        let lines: Vec<String> = dmesg
            .lines()
            .filter(|line| has_virtio_line(line))
            .take(8)
            .map(ToOwned::to_owned)
            .collect();
        report.add(
            "devices",
            "devices.kernel_virtio",
            "error",
            "Kernel log mentions virtio/QEMU",
            "The sealed runtime must have no virtio or QEMU kernel messages.",
            Some(&json!(lines)),
            "",
            false,
        );
    } else {
        report.add(
            "devices",
            "devices.kernel_virtio",
            "info",
            "Kernel log has no virtio/QEMU lines",
            "runtime boot is clean",
            None,
            "",
            false,
        );
    }
    let lsmod = out(raw, "lsmod");
    let virtio_mods: Vec<String> = lsmod
        .lines()
        .filter(|line| line.to_lowercase().contains("virtio"))
        .map(ToOwned::to_owned)
        .collect();
    if !virtio_mods.is_empty() {
        report.add(
            "devices",
            "devices.virtio_modules",
            "error",
            "Virtio kernel modules are loaded",
            &virtio_mods.join("\n"),
            Some(&json!(virtio_mods)),
            "",
            false,
        );
    } else {
        report.add(
            "devices",
            "devices.virtio_modules",
            "info",
            "No virtio modules loaded",
            "lsmod is clean",
            None,
            "",
            false,
        );
    }

    // -------------------------------------------------------------- NETWORK
    let macs = iface_macs(raw);
    let expected_mac = network
        .get("mac")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let bad_macs: Map<String, Value> = macs
        .iter()
        .filter(|(_, mac)| {
            let text = mac.as_str().unwrap_or("");
            text.starts_with("52:54:00") || text.starts_with("52:55:0a")
        })
        .map(|(name, mac)| (name.clone(), mac.clone()))
        .collect();
    // Gateway mode intentionally has a restricted slirp management NIC and a
    // gateway NIC with a QEMU OUI. Keep these fingerprints visible as warnings.
    let lab_severity = if allow_user_network || cfg.isolated() {
        "warning"
    } else {
        "error"
    };
    if !bad_macs.is_empty() {
        report.add(
            "network",
            "network.mac",
            lab_severity,
            "A QEMU default MAC prefix is visible",
            "52:54:00 is the QEMU OUI and 52:55:0a is the slirp gateway OUI.",
            Some(&json!(bad_macs)),
            "Set device.network.mac_oui to a real NIC vendor OUI.",
            false,
        );
    } else if !expected_mac.is_empty()
        && !macs
            .values()
            .any(|mac| mac.as_str() == Some(expected_mac.as_str()))
    {
        report.add(
            "network",
            "network.mac",
            "warning",
            "Expected MAC is not present",
            &format!(
                "expected {expected_mac}; observed {}",
                py_json::py_repr(&json!(macs))
            ),
            Some(&json!(macs)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.mac",
            "info",
            "NIC MAC uses a real vendor OUI",
            &py_json::dumps_default(&json!(macs), true),
            Some(&json!(macs)),
            "",
            false,
        );
    }

    let interface = network
        .get("interface")
        .and_then(Value::as_str)
        .unwrap_or("");
    let mut ethtool_driver = String::new();
    let names: Vec<&str> = (if interface.is_empty() {
        Vec::new()
    } else {
        vec![interface]
    })
    .into_iter()
    .chain(macs.keys().map(|name| name.as_str()))
    .collect();
    let driver_re = Regex::new(r"(?m)^driver:\s*(\S+)").ok();
    for name in names {
        let data = raw
            .get("net")
            .and_then(|value| value.get("ethtool"))
            .and_then(|value| value.get(name))
            .unwrap_or(&Value::Null);
        let text = data.get("out").and_then(Value::as_str).unwrap_or("");
        if let Some(captures) = driver_re
            .as_ref()
            .and_then(|pattern| pattern.captures(text))
        {
            ethtool_driver = captures[1].to_string();
            break;
        }
    }
    if ethtool_driver == "virtio_net" {
        report.add(
            "network",
            "network.driver",
            "error",
            "NIC driver is virtio_net",
            "ethtool reports virtio_net",
            Some(&json!(ethtool_driver)),
            "",
            false,
        );
    } else if !ethtool_driver.is_empty() {
        report.add(
            "network",
            "network.driver",
            "info",
            "NIC driver is a real model",
            &format!("driver: {ethtool_driver}"),
            Some(&json!(ethtool_driver)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.driver",
            "warning",
            "Cannot read the NIC driver",
            "ethtool -i did not report a driver",
            Some(
                raw.get("net")
                    .and_then(|value| value.get("ethtool"))
                    .unwrap_or(&Value::Null),
            ),
            "",
            false,
        );
    }

    let addresses = iface_addresses(raw, "inet");
    let qemu_ips: Vec<(String, String)> = addresses
        .iter()
        .filter(|(_, address)| address.starts_with("10.0.2."))
        .cloned()
        .collect();
    if !qemu_ips.is_empty() {
        report.add(
            "network",
            "network.slirp_ip",
            lab_severity,
            "Slirp 10.0.2.0/24 address is visible",
            "QEMU user networking hands out 10.0.2.15/24 with gateway 10.0.2.2.",
            Some(&json!(qemu_ips)),
            "Use --network socket-vmnet with a bridged socket_vmnet daemon and a static LAN address, or a vmnet bridge.",
            false,
        );
    } else {
        report.add(
            "network",
            "network.slirp_ip",
            "info",
            "No slirp 10.0.2.x address",
            &py_json::dumps_default(&json!(addresses), false),
            Some(&json!(addresses)),
            "",
            false,
        );
    }
    if let Some(expected_address) = network
        .get("address")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    {
        let expected_ip = expected_address.split('/').next().unwrap_or("").to_string();
        if !addresses.iter().any(|(_, address)| address == &expected_ip) {
            report.add(
                "network",
                "network.static_ip",
                "error",
                "Configured static address is not present",
                &format!(
                    "expected {expected_ip}; observed {}",
                    repr_tuples(&addresses)
                ),
                Some(&json!(addresses)),
                "",
                false,
            );
        } else {
            report.add(
                "network",
                "network.static_ip",
                "info",
                "Configured static address is active",
                &expected_ip,
                None,
                "",
                false,
            );
        }
    }

    let gateways = default_gateways(raw);
    let bad_gateways: Vec<String> = gateways
        .iter()
        .filter(|gateway| gateway.starts_with("10.0.2.") || gateway.starts_with("192.168.105."))
        .cloned()
        .collect();
    if !bad_gateways.is_empty() {
        report.add(
            "network",
            "network.gateway",
            lab_severity,
            "QEMU/vmnet gateway is visible",
            &format!("default gateways: {}", py_json::py_repr(&json!(gateways))),
            Some(&json!(bad_gateways)),
            "Bridge the guest onto the physical LAN so the real router is the gateway.",
            false,
        );
    } else if !gateways.is_empty() {
        report.add(
            "network",
            "network.gateway",
            "info",
            "Default gateway is not a QEMU signature",
            &py_json::dumps_default(&json!(gateways), false),
            Some(&json!(gateways)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.gateway",
            "warning",
            "No default gateway",
            "ip route has no default route",
            None,
            "",
            false,
        );
    }

    let dns = dns_servers(raw);
    let bad_dns: Vec<String> = dns
        .iter()
        .filter(|server| server.starts_with("10.0.2.3") || server.starts_with("192.168.105.1"))
        .cloned()
        .collect();
    if !bad_dns.is_empty() {
        report.add(
            "network",
            "network.dns",
            lab_severity,
            "QEMU/vmnet DNS server is visible",
            &format!("nameservers: {}", py_json::py_repr(&json!(dns))),
            Some(&json!(bad_dns)),
            "Use the LAN DNS server through a bridged NIC.",
            false,
        );
    } else if !dns.is_empty() {
        report.add(
            "network",
            "network.dns",
            "info",
            "DNS servers are not QEMU signatures",
            &py_json::dumps_default(&json!(dns), false),
            Some(&json!(dns)),
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.dns",
            "warning",
            "No DNS server configured",
            "resolv.conf is empty",
            None,
            "",
            false,
        );
    }

    let arp = file_text(raw, "/proc/net/arp");
    let mut arp_rows = Vec::new();
    for line in arp.lines().skip(1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 {
            arp_rows.push(json!({
                "ip": parts[0],
                "mac": parts[3].to_lowercase(),
                "device": if parts.len() > 5 { parts[5] } else { "" },
            }));
        }
    }
    let bad_arp: Vec<Value> = arp_rows
        .iter()
        .filter(|row| {
            let mac = row.get("mac").and_then(Value::as_str).unwrap_or("");
            mac.starts_with("52:55:0a") || mac.starts_with("52:54:00")
        })
        .cloned()
        .collect();
    if !bad_arp.is_empty() {
        report.add(
            "network",
            "network.arp",
            lab_severity,
            "QEMU gateway MAC is visible in the ARP table",
            "The slirp gateway uses 52:55:0a:00:02:02.",
            Some(&json!(bad_arp)),
            "Bridge the guest onto the physical LAN.",
            false,
        );
    } else {
        report.add(
            "network",
            "network.arp",
            "info",
            "ARP table has no QEMU gateway MAC",
            &py_json::dumps_default(&json!(arp_rows.iter().take(10).collect::<Vec<_>>()), false),
            Some(&json!(arp_rows.iter().take(20).collect::<Vec<_>>())),
            "",
            false,
        );
    }

    let lease_text = raw
        .get("leases")
        .and_then(Value::as_object)
        .map(|leases| {
            leases
                .values()
                .map(|info| {
                    info.get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if lease_text.contains("10.0.2.2")
        || lease_text.contains("10.0.2.3")
        || lease_text.contains("10.0.2.15")
    {
        report.add(
            "network",
            "network.dhcp_lease",
            lab_severity,
            "DHCP lease identifies QEMU slirp",
            "A systemd-networkd/DHCP lease contains 10.0.2.2/10.0.2.3/10.0.2.15.",
            Some(&json!(truncate_chars(&lease_text, 2000))),
            "",
            false,
        );
    } else if !lease_text.is_empty() {
        report.add(
            "network",
            "network.dhcp_lease",
            "info",
            "DHCP lease does not identify QEMU",
            &truncate_chars(&lease_text, 1000),
            None,
            "",
            false,
        );
    } else {
        report.add(
            "network",
            "network.dhcp_lease",
            "info",
            "No DHCP lease on disk (static addressing)",
            "",
            None,
            "",
            false,
        );
    }

    let ipv6 = iface_addresses(raw, "inet6");
    let eui64: Vec<(String, String)> = ipv6
        .iter()
        .filter(|(_, address)| {
            !address.to_lowercase().starts_with("fe80:") && address.to_lowercase().contains("ff:fe")
        })
        .cloned()
        .collect();
    if !eui64.is_empty() {
        report.add(
            "network",
            "network.ipv6_eui64",
            "warning",
            "IPv6 EUI-64 address may leak the MAC",
            "A link-local/global IPv6 address contains the MAC-derived ff:fe IID.",
            Some(&json!(eui64)),
            "netplan ipv6-privacy: true and addr-gen-mode: stable-privacy.",
            false,
        );
    } else {
        report.add(
            "network",
            "network.ipv6_eui64",
            "info",
            "No MAC-derived IPv6 address",
            &py_json::dumps_default(&json!(ipv6.iter().take(8).collect::<Vec<_>>()), false),
            None,
            "",
            false,
        );
    }
    let non_lo: Vec<String> = macs.keys().cloned().collect();
    if non_lo.len() > 1 {
        report.add(
            "network",
            "network.interface_count",
            "warning",
            "Multiple guest NICs are present",
            &format!(
                "interfaces: {}; the management NIC can leak 10.0.2.x",
                py_json::py_repr(&json!(non_lo))
            ),
            Some(&json!(non_lo)),
            "",
            false,
        );
    }

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

    // ------------------------------------------------------------- SUMMARY
    let mut groups = Map::new();
    let totals = FindingCounts::from_findings(&report.findings);
    for group in GROUPS {
        let mut findings: Vec<Value> = report.findings.iter()
            .filter(|item| item["group"].as_str() == Some(group))
            .cloned().collect();
        let counts = FindingCounts::from_findings(&findings);
        findings.sort_by(|a, b| {
            let order = |item: &Value| severity_order(item["severity"].as_str().unwrap_or("info"));
            order(a).cmp(&order(b)).then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        });
        groups.insert(group.into(), json!({
            "status": counts.status(), "errors": counts.errors, "warnings": counts.warnings,
            "unverified": counts.unverified, "findings": findings,
        }));
    }
    let fail_on = verify["fail_on"].as_str().unwrap_or("error");
    let acceptable = totals.errors == 0 && totals.unverified == 0
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
    evidence_summary.insert("systemd_detect_virt".to_string(), json!(detect));
    evidence_summary.insert("virt_what".to_string(), json!(virt_what));
    evidence_summary.insert("dmi".to_string(), dmi_vendor_fields);
    evidence_summary.insert(
        "pci".to_string(),
        json!(pci.iter().take(30).collect::<Vec<_>>()),
    );
    evidence_summary.insert("root_disk".to_string(), root.unwrap_or(Value::Null));
    evidence_summary.insert("macs".to_string(), json!(macs));
    evidence_summary.insert("addresses".to_string(), json!(addresses));
    evidence_summary.insert("gateways".to_string(), json!(gateways));
    evidence_summary.insert("dns".to_string(), json!(dns));
    evidence_summary.insert("timezone".to_string(), json!(timezone));
    evidence_summary.insert("locale".to_string(), json!(effective));
    evidence_summary.insert("packages".to_string(), json!(installed_cloud));

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
        assert_eq!(field(text, "Product Name"), "Example");
        assert!(dmi_type(text, 2).contains("Product Name: Board"));
        assert!(dmi_type(text, 99).is_empty());
        let raw = json!({"files": {"/etc/resolv.conf": {"text": "# comment\n nameserver 1.1.1.1\nnameserver 8.8.8.8\n"}}});
        assert_eq!(dns_servers(&raw), ["1.1.1.1", "8.8.8.8"]);
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
}
