//! Rules shared by more than one verification group.

use regex::Regex;
use serde_json::{Value, json};

use crate::config::Config;
use crate::py_json;

use super::DEFAULT_FORBIDDEN;

pub(super) fn severity_order(severity: &str) -> u8 {
    match severity {
        "error" => 0,
        "unverified" => 1,
        "warning" => 2,
        _ => 3,
    }
}

#[derive(Default)]
pub(super) struct FindingCounts {
    pub(super) errors: usize,
    pub(super) warnings: usize,
    pub(super) unverified: usize,
}

impl FindingCounts {
    pub(super) fn from_findings(findings: &[Value]) -> Self {
        let mut counts = Self::default();
        for item in findings {
            match item["severity"].as_str() {
                Some("error") => counts.errors += 1,
                Some("warning") => counts.warnings += 1,
                Some("unverified") => counts.unverified += 1,
                _ => {}
            }
        }
        counts
    }

    pub(super) fn status(&self) -> &'static str {
        match (self.errors, self.unverified, self.warnings) {
            (0, 0, 0) => "pass",
            (0, 0, _) => "warn",
            (0, _, _) => "unverified",
            _ => "fail",
        }
    }
}

pub(super) struct Report {
    pub(super) findings: Vec<Value>,
}

impl Report {
    pub(super) fn new() -> Self {
        Self {
            findings: Vec::new(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn add(
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

pub(super) fn out(raw: &Value, name: &str) -> String {
    cmd(raw, name)
        .get("out")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn rc(raw: &Value, name: &str) -> i64 {
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

pub(super) fn file_text(raw: &Value, path: &str) -> String {
    file_entry(raw, path)
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(super) fn field(text: &str, label: &str) -> String {
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

pub(super) fn forbidden(patterns: &[String], text: &str) -> Vec<String> {
    let low = text.to_lowercase();
    patterns
        .iter()
        .filter(|pattern| low.contains(&pattern.to_lowercase()))
        .cloned()
        .collect()
}

pub(super) fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

pub(super) fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub(super) fn repr_string(text: &str) -> String {
    py_json::py_repr_string(text)
}

/// Inputs and accumulated findings shared by every group.
pub(super) struct Context<'a> {
    pub(super) raw: &'a Value,
    pub(super) cfg: &'a Config,
    pub(super) verify: &'a Value,
    pub(super) patterns: Vec<String>,
    pub(super) allow_user_network: bool,
    pub(super) network: &'a Value,
    pub(super) smbios: &'a Value,
    pub(super) cpu: &'a Value,
    pub(super) guest: &'a Value,
    pub(super) devices: &'a Value,
    pub(super) topology: &'a Value,
    pub(super) midr: i64,
    pub(super) expected_model: String,
    pub(super) dmi_text: String,
    pub(super) lscpu: String,
    pub(super) cpuinfo: String,
    pub(super) report: Report,
}

impl<'a> Context<'a> {
    pub(super) fn new(raw: &'a Value, resolved: &'a Value, cfg: &'a Config) -> Self {
        let verify = resolved.get("verify").unwrap_or(&Value::Null);
        let patterns: Vec<String> = match verify.get("forbidden_strings").and_then(Value::as_array)
        {
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
        Self {
            raw,
            cfg,
            verify,
            patterns,
            allow_user_network,
            network,
            smbios,
            cpu,
            guest,
            devices,
            topology,
            midr,
            expected_model,
            dmi_text: out(raw, "dmidecode"),
            lscpu: out(raw, "lscpu"),
            cpuinfo: out(raw, "cpuinfo"),
            report: Report::new(),
        }
    }
}
