//! Device checks: PCI, disks, optical, USB, sound and kernel modules.

use regex::Regex;
use serde_json::{Value, json};

use crate::py_json;

use super::shared::{Context, forbidden, out, rc, repr_string, truncate_chars};

/// Values the summary needs from this section.
pub(super) struct Output {
    pub(super) pci: Vec<String>,
    pub(super) root: Option<Value>,
}

pub(super) fn run(ctx: &mut Context<'_>, dmesg: String) -> Output {
    let raw = ctx.raw;
    let devices = ctx.devices;
    let patterns = ctx.patterns.clone();
    let report = &mut ctx.report;

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
            "devices",
            "devices.virtio",
            "unverified",
            "PCI devices were not fully observed",
            "The guest did not supply a successful PCI device listing.",
            None,
            "Confirm lspci works in the guest.",
            false,
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

    Output { pci, root }
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

fn has_virtio_line(text: &str) -> bool {
    let low = text.to_lowercase();
    low.contains("virtio") || low.contains("qemu")
}
