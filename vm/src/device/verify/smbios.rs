//! SMBIOS, DMI and ACPI checks.

use regex::Regex;
use serde_json::{Map, Value, json};

use crate::py_json;

use super::shared::{Context, as_text, field, forbidden, out, repr_string};

/// Values the summary needs from this section.
pub(super) struct Output {
    pub(super) dmi_vendor_fields: Value,
}

pub(super) fn run(ctx: &mut Context<'_>) -> Output {
    let raw = ctx.raw;
    let smbios = ctx.smbios;
    let patterns = ctx.patterns.clone();
    let midr = ctx.midr;
    let dmi_text = ctx.dmi_text.clone();
    let report = &mut ctx.report;

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
    let tables_read = raw["acpi"]
        .as_object()
        .into_iter()
        .flat_map(Map::values)
        .filter(|table| table["strings"].is_array())
        .count();
    if tables_read == 0 {
        report.add(
            "smbios",
            "smbios.acpi_evidence",
            "unverified",
            "No ACPI tables were read",
            "The guest did not supply raw ACPI tables for this verification.",
            None,
            "Check firmware configuration and guest read permissions.",
            false,
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
            "smbios",
            "smbios.acpi",
            "info",
            "No configured signatures in the ACPI scan",
            &format!("{tables_read} ACPI tables scanned"),
            None,
            "",
            false,
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
    } else if acpi_oem
        .values()
        .any(|info| info["oem_id"].as_str().is_some_and(|id| !id.is_empty()))
    {
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

    Output { dmi_vendor_fields }
}

fn dmi(raw: &Value, key: &str) -> String {
    raw.get("dmi")
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

pub(super) fn dmi_type(text: &str, number: i64) -> String {
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

fn json_str(value: &Value) -> String {
    py_json::dumps_default(value, true)
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
