//! SMBIOS table generator.
//!
//! QEMU's `-smbios file=` accepts a raw sequence of SMBIOS structures.  Building
//! the table ourselves removes QEMU's hard-coded "virtual machine" BIOS bit and
//! exposes fields (`-smbios type=`) does not: chassis type, DIMM locator, DDR4
//! type, cache descriptors, OEM ACPI IDs.

use std::collections::HashSet;

use serde_json::Value;

use crate::Result;
use crate::device::profile as sprofile;
use crate::util::message;

struct Strings {
    items: Vec<String>,
}

impl Strings {
    fn new() -> Self {
        Self { items: Vec::new() }
    }

    fn add(&mut self, value: Option<&Value>) -> u8 {
        let text = match value {
            None | Some(Value::Null) => return 0,
            Some(Value::String(text)) if text.is_empty() => return 0,
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        if let Some(index) = self.items.iter().position(|item| item == &text) {
            return index as u8 + 1;
        }
        if self.items.len() >= 254 {
            return 0;
        }
        self.items.push(text);
        self.items.len() as u8
    }

    fn add_str(&mut self, value: &str) -> u8 {
        self.add(Some(&Value::String(value.to_string())))
    }

    fn blob(&self) -> Vec<u8> {
        if self.items.is_empty() {
            return vec![0, 0];
        }
        let mut output = Vec::new();
        for item in &self.items {
            output.extend_from_slice(item.as_bytes());
            output.push(0);
        }
        output.push(0);
        output
    }
}

fn structure(stype: u8, handle: u16, formatted: &[u8], strings: &Strings) -> Result<Vec<u8>> {
    let length = 4 + formatted.len();
    if length > 255 {
        return message(format!(
            "SMBIOS type {stype} structure is too long ({length} bytes)"
        ));
    }
    let mut output = Vec::with_capacity(length + strings.blob().len());
    output.push(stype);
    output.push(length as u8);
    output.push((handle & 0xFF) as u8);
    output.push((handle >> 8) as u8);
    output.extend_from_slice(formatted);
    output.extend_from_slice(&strings.blob());
    Ok(output)
}

fn bits(names: &[String], mapping: &[(&str, u8)]) -> Result<u64> {
    let mut value: u64 = 0;
    for name in names {
        let Some((_, bit)) = mapping.iter().find(|(known, _)| known == name) else {
            return message(format!("unknown SMBIOS characteristic bit {name:?}"));
        };
        value |= 1u64 << bit;
    }
    Ok(value)
}

fn cache_size(kb: u64) -> Result<u16> {
    if kb == 0 {
        return Ok(0);
    }
    if kb <= 0x7FFF {
        return Ok(kb as u16);
    }
    let kb = if !kb.is_multiple_of(64) {
        ((kb / 64) + 1) * 64
    } else {
        kb
    };
    let units = kb / 64;
    if units > 0x7FFF {
        return message("cache size is too large for SMBIOS");
    }
    Ok(0x8000 | units as u16)
}

fn ascii(value: Option<&Value>) -> Result<String> {
    let text = match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    if text.chars().count() > 64 {
        return message(format!(
            "SMBIOS string is longer than 64 characters: {text:?}"
        ));
    }
    Ok(text)
}

fn uuid_bytes(value: Option<&Value>) -> Result<[u8; 16]> {
    let Some(Value::String(text)) = value else {
        return message("SMBIOS system UUID is empty");
    };
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() != 5
        || parts.iter().map(|part| part.len()).collect::<Vec<_>>() != vec![8, 4, 4, 4, 12]
        || !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_hexdigit()))
    {
        return message(format!("invalid SMBIOS UUID: {text:?}"));
    }
    let compact: String = parts.concat();
    let raw = u128::from_str_radix(&compact, 16)
        .map_err(|_| crate::Error::Message(format!("invalid SMBIOS UUID: {text:?}")))?;
    let bytes = raw.to_be_bytes();
    // uuid.bytes_le: time_low/time_mid/time_hi little-endian, clock_seq/node BE.
    let mut output = [0u8; 16];
    output[0..4].copy_from_slice(&[bytes[3], bytes[2], bytes[1], bytes[0]]);
    output[4..6].copy_from_slice(&[bytes[5], bytes[4]]);
    output[6..8].copy_from_slice(&[bytes[7], bytes[6]]);
    output[8..16].copy_from_slice(&bytes[8..16]);
    Ok(output)
}

fn serial_for(name: &str, kind: &str, index: usize) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("vmctl-device:{name}:{kind}:{index}").as_bytes());
    let digest = hasher.finalize();
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ0123456789";
    digest[..10]
        .iter()
        .map(|byte| ALPHABET[(*byte as usize) % ALPHABET.len()] as char)
        .collect()
}

fn int(value: &Value, key: &str, default: i64) -> i64 {
    value
        .get(key)
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|v| v as i64)))
        .unwrap_or(default)
}

fn object<'a>(value: &'a Value, key: &str) -> &'a Value {
    value.get(key).unwrap_or(&Value::Null)
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value.and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        None => Vec::new(),
    }
}

fn push_u8(output: &mut Vec<u8>, value: i64) {
    output.push((value & 0xFF) as u8);
}
fn push_u16(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&((value & 0xFFFF) as u16).to_le_bytes());
}
fn push_u32(output: &mut Vec<u8>, value: i64) {
    output.extend_from_slice(&((value & 0xFFFF_FFFF) as u32).to_le_bytes());
}
fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub fn build(resolved: &Value, name: &str, cpus: u32, memory_mb: u32) -> Result<Vec<u8>> {
    let smbios = resolved.get("smbios").unwrap_or(&Value::Null);
    let cpu = resolved.get("cpu").unwrap_or(&Value::Null);
    let topology = object(cpu, "topology");
    let sockets = int(topology, "sockets", 1).max(1);
    let cores = int(topology, "cores", cpus as i64).max(1);
    let threads = int(topology, "threads", 1).max(1);
    let midr = match cpu.get("midr_int").and_then(Value::as_u64) {
        Some(value) => value as u32,
        None => sprofile::parse_midr(cpu.get("midr"), cpu.get("model").and_then(Value::as_str))?,
    };

    let mut out: Vec<Vec<u8>> = Vec::new();

    // ---- type 0: BIOS information
    let bios = object(smbios, "bios");
    let mut strings = Strings::new();
    let vendor = strings.add(Some(&Value::String(ascii(bios.get("vendor"))?)));
    let version = strings.add(Some(&Value::String(ascii(bios.get("version"))?)));
    let release = strings.add(Some(&Value::String(ascii(bios.get("release_date"))?)));
    let chars = bits(
        &string_list(bios.get("characteristics")),
        &[
            ("pci", 7),
            ("upgradeable", 11),
            ("cd_boot", 15),
            ("selectable_boot", 16),
            ("edd", 19),
        ],
    )?;
    let ext1 = bits(
        &string_list(bios.get("extension1")),
        &[
            ("acpi", 0),
            ("usb_legacy", 1),
            ("agp", 2),
            ("i2o", 3),
            ("ls120", 4),
            ("zip", 5),
            ("ieee1394", 6),
            ("smart_battery", 7),
        ],
    )?;
    let ext2 = bits(
        &string_list(bios.get("extension2")),
        &[
            ("bios_boot_spec", 0),
            ("network_boot", 1),
            ("targeted_content", 2),
            ("uefi", 3),
            ("virtual_machine", 4),
            ("manufacturing_supported", 5),
            ("manufacturing_enabled", 6),
            ("crash_dump", 7),
        ],
    )?;
    let mut formatted = Vec::new();
    push_u8(&mut formatted, vendor as i64);
    push_u8(&mut formatted, version as i64);
    push_u16(&mut formatted, 0xE800);
    push_u8(&mut formatted, release as i64);
    push_u8(&mut formatted, int(bios, "rom_size", 255));
    push_u64(&mut formatted, chars);
    push_u8(&mut formatted, ext1 as i64);
    push_u8(&mut formatted, ext2 as i64);
    push_u8(&mut formatted, int(bios, "major", 2));
    push_u8(&mut formatted, int(bios, "minor", 19));
    push_u8(&mut formatted, int(bios, "ec_major", 255));
    push_u8(&mut formatted, int(bios, "ec_minor", 255));
    out.push(structure(0, 0x0000, &formatted, &strings)?);

    // ---- type 1: system information
    let system = object(smbios, "system");
    let mut strings = Strings::new();
    let manufacturer = strings.add(Some(&Value::String(ascii(system.get("manufacturer"))?)));
    let product = strings.add(Some(&Value::String(ascii(system.get("product"))?)));
    let sys_version = strings.add(Some(&Value::String(ascii(system.get("version"))?)));
    let serial = strings.add(Some(&Value::String(ascii(system.get("serial"))?)));
    let sku = strings.add(Some(&Value::String(ascii(system.get("sku"))?)));
    let family = strings.add(Some(&Value::String(ascii(system.get("family"))?)));
    let mut formatted = Vec::new();
    push_u8(&mut formatted, manufacturer as i64);
    push_u8(&mut formatted, product as i64);
    push_u8(&mut formatted, sys_version as i64);
    push_u8(&mut formatted, serial as i64);
    formatted.extend_from_slice(&uuid_bytes(system.get("uuid"))?);
    push_u8(&mut formatted, int(system, "wakeup_type", 6));
    push_u8(&mut formatted, sku as i64);
    push_u8(&mut formatted, family as i64);
    out.push(structure(1, 0x0100, &formatted, &strings)?);

    // ---- type 2: baseboard
    let board = object(smbios, "baseboard");
    let mut strings = Strings::new();
    let b_man = strings.add(Some(&Value::String(ascii(board.get("manufacturer"))?)));
    let b_prod = strings.add(Some(&Value::String(ascii(board.get("product"))?)));
    let b_ver = strings.add(Some(&Value::String(ascii(board.get("version"))?)));
    let b_serial = strings.add(Some(&Value::String(ascii(board.get("serial"))?)));
    let b_asset = strings.add(Some(&Value::String(ascii(board.get("asset"))?)));
    let b_loc = strings.add(Some(&Value::String(ascii(board.get("location"))?)));
    let mut formatted = Vec::new();
    push_u8(&mut formatted, b_man as i64);
    push_u8(&mut formatted, b_prod as i64);
    push_u8(&mut formatted, b_ver as i64);
    push_u8(&mut formatted, b_serial as i64);
    push_u8(&mut formatted, b_asset as i64);
    push_u8(&mut formatted, int(board, "feature_flags", 1));
    push_u8(&mut formatted, b_loc as i64);
    push_u16(&mut formatted, 0x0300);
    push_u8(&mut formatted, int(board, "board_type", 10));
    push_u8(&mut formatted, 0);
    out.push(structure(2, 0x0200, &formatted, &strings)?);

    // ---- type 3: chassis
    let chassis = object(smbios, "chassis");
    let mut strings = Strings::new();
    let c_man = strings.add(Some(&Value::String(ascii(chassis.get("manufacturer"))?)));
    let c_ver = strings.add(Some(&Value::String(ascii(chassis.get("version"))?)));
    let c_serial = strings.add(Some(&Value::String(ascii(chassis.get("serial"))?)));
    let c_asset = strings.add(Some(&Value::String(ascii(chassis.get("asset"))?)));
    let c_sku = strings.add(Some(&Value::String(ascii(chassis.get("sku"))?)));
    let mut formatted = Vec::new();
    push_u8(&mut formatted, c_man as i64);
    push_u8(&mut formatted, int(chassis, "type", 17));
    push_u8(&mut formatted, c_ver as i64);
    push_u8(&mut formatted, c_serial as i64);
    push_u8(&mut formatted, c_asset as i64);
    push_u8(&mut formatted, 0x03);
    push_u8(&mut formatted, 0x03);
    push_u8(&mut formatted, 0x03);
    push_u8(&mut formatted, int(chassis, "security_status", 2));
    push_u32(&mut formatted, int(chassis, "oem_defined", 0));
    push_u8(&mut formatted, int(chassis, "height_u", 2));
    push_u8(&mut formatted, int(chassis, "power_cords", 2));
    push_u8(&mut formatted, 0);
    push_u8(&mut formatted, 0);
    push_u8(&mut formatted, c_sku as i64);
    out.push(structure(3, 0x0300, &formatted, &strings)?);

    // ---- type 7: cache, type 4: processor
    let processor = object(smbios, "processor");
    let cache = object(processor, "cache");
    let cache_sizes = [
        (
            "L1 Cache",
            int(cache, "l1_kb", 0),
            int(cache, "l1_assoc", 7),
        ),
        (
            "L2 Cache",
            int(cache, "l2_kb", 0),
            int(cache, "l2_assoc", 7),
        ),
        (
            "L3 Cache",
            int(cache, "l3_kb", 0),
            int(cache, "l3_assoc", 8),
        ),
    ];
    let mut cache_handles: Vec<u16> = Vec::new();
    for (index, (label, size_kb, assoc)) in cache_sizes.iter().enumerate() {
        let handle = 0x0700 + index as u16;
        if *size_kb <= 0 {
            cache_handles.push(0xFFFF);
            continue;
        }
        let mut strings = Strings::new();
        let designation = strings.add_str(label);
        let cfg = (index as i64 & 0x7) | (1 << 7) | (1 << 9);
        let mut formatted = Vec::new();
        push_u8(&mut formatted, designation as i64);
        push_u16(&mut formatted, cfg);
        push_u16(&mut formatted, cache_size(*size_kb as u64)? as i64);
        push_u16(&mut formatted, cache_size(*size_kb as u64)? as i64);
        push_u16(&mut formatted, 0x0020);
        push_u16(&mut formatted, 0x0020);
        push_u8(&mut formatted, 0);
        push_u8(&mut formatted, 0x03);
        push_u8(&mut formatted, 0x05);
        push_u8(&mut formatted, *assoc);
        out.push(structure(7, handle, &formatted, &strings)?);
        cache_handles.push(handle);
    }

    let proc_chars = bits(
        &string_list(processor.get("characteristics")),
        &[
            ("64bit", 2),
            ("multicore", 3),
            ("hardware_thread", 4),
            ("execute_protection", 5),
            ("enhanced_virtualization", 6),
            ("power_performance", 7),
            ("128bit", 8),
        ],
    )?;
    let family = int(processor, "family", 257);
    for socket_index in 0..sockets {
        let mut strings = Strings::new();
        let socket = if sockets == 1 {
            strings.add(Some(&Value::String(ascii(
                processor.get("socket").filter(|value| !value.is_null()),
            )?)))
        } else {
            strings.add_str(&format!("CPU {socket_index}"))
        };
        let socket = if socket == 0 {
            strings.add_str("CPU 0")
        } else {
            socket
        };
        let proc_man = strings.add(Some(&Value::String(ascii(processor.get("manufacturer"))?)));
        let proc_ver = strings.add(Some(&Value::String(ascii(processor.get("version"))?)));
        let proc_serial = strings.add(Some(&Value::String(ascii(processor.get("serial"))?)));
        let proc_asset = strings.add(Some(&Value::String(ascii(processor.get("asset"))?)));
        let proc_part = strings.add(Some(&Value::String(ascii(processor.get("part"))?)));
        let family_byte = if family <= 0xFE { family } else { 0xFE };
        let family2 = if family > 0xFF { family } else { 0 };
        let mut formatted = Vec::new();
        push_u8(&mut formatted, socket as i64);
        push_u8(&mut formatted, int(processor, "type", 3));
        push_u8(&mut formatted, family_byte);
        push_u8(&mut formatted, proc_man as i64);
        push_u64(&mut formatted, midr as u64);
        push_u8(&mut formatted, proc_ver as i64);
        push_u8(&mut formatted, int(processor, "voltage", 139));
        push_u16(&mut formatted, int(processor, "external_clock_mhz", 100));
        push_u16(&mut formatted, int(processor, "max_speed_mhz", 3000));
        push_u16(&mut formatted, int(processor, "current_speed_mhz", 3000));
        push_u8(&mut formatted, int(processor, "status", 0x41));
        push_u8(&mut formatted, int(processor, "upgrade", 1));
        push_u16(
            &mut formatted,
            *cache_handles.first().unwrap_or(&0xFFFF) as i64,
        );
        push_u16(
            &mut formatted,
            *cache_handles.get(1).unwrap_or(&0xFFFF) as i64,
        );
        push_u16(
            &mut formatted,
            *cache_handles.get(2).unwrap_or(&0xFFFF) as i64,
        );
        push_u8(&mut formatted, proc_serial as i64);
        push_u8(&mut formatted, proc_asset as i64);
        push_u8(&mut formatted, proc_part as i64);
        push_u8(&mut formatted, cores.min(255));
        push_u8(&mut formatted, cores.min(255));
        push_u8(&mut formatted, (cores * threads).min(255));
        push_u16(&mut formatted, proc_chars as i64);
        push_u16(&mut formatted, family2);
        push_u16(&mut formatted, cores);
        push_u16(&mut formatted, cores);
        push_u16(&mut formatted, cores * threads);
        out.push(structure(
            4,
            0x0400 + socket_index as u16,
            &formatted,
            &strings,
        )?);
    }

    // ---- type 16/17: physical + memory devices
    let memory = object(smbios, "memory");
    let dimms = int(memory, "resolved_dimm_count", 0)
        .max(int(memory, "dimm_count", 1))
        .max(1);
    let per_dimm = if int(memory, "resolved_size_mb", 0) > 0 {
        int(memory, "resolved_size_mb", 0)
    } else {
        memory_mb as i64 / dimms
    };
    let total_kb = memory_mb as i64 * 1024;
    let max_kb = int(memory, "maximum_capacity_kb", total_kb).max(0);
    let max_field: i64 = if max_kb > 0xFFFF_FFFF {
        0x8000_0000
    } else {
        max_kb
    };
    let mut formatted = Vec::new();
    push_u8(&mut formatted, 0x03);
    push_u8(&mut formatted, 0x03);
    push_u8(&mut formatted, int(memory, "error_correction", 6));
    push_u32(&mut formatted, max_field);
    push_u16(&mut formatted, 0xFFFE);
    push_u16(&mut formatted, dimms);
    push_u64(&mut formatted, (memory_mb as u64) * 1024 * 1024);
    out.push(structure(16, 0x1000, &formatted, &Strings::new())?);

    let locator_prefix = memory
        .get("locator_prefix")
        .and_then(Value::as_str)
        .unwrap_or("DIMM_A");
    let bank_prefix = memory
        .get("bank_prefix")
        .and_then(Value::as_str)
        .unwrap_or("BANK ");
    for index in 0..dimms {
        let mut strings = Strings::new();
        let locator = strings.add_str(&format!("{locator_prefix}{}", index + 1));
        let bank = strings.add_str(&format!("{bank_prefix}{index}"));
        let dimm_man = strings.add(Some(&Value::String(ascii(memory.get("manufacturer"))?)));
        let serial_value = match memory.get("serial") {
            Some(Value::String(text)) if !text.is_empty() => Value::String(text.clone()),
            _ => Value::String(serial_for(name, "dimm-serial", index as usize)),
        };
        let dimm_serial = strings.add(Some(&serial_value));
        let asset_value = match memory.get("asset") {
            Some(Value::String(text)) if !text.is_empty() => Value::String(text.clone()),
            _ => Value::String(serial_for(name, "dimm-asset", index as usize)),
        };
        let dimm_asset = strings.add(Some(&asset_value));
        let dimm_part = strings.add(Some(&Value::String(ascii(memory.get("part"))?)));
        let (size_field, extended) = if per_dimm > 0x7FFF {
            (0x7FFF, per_dimm)
        } else {
            (per_dimm, 0)
        };
        let mut formatted = Vec::new();
        push_u16(&mut formatted, 0x1000);
        push_u16(&mut formatted, 0xFFFE);
        push_u16(&mut formatted, int(memory, "total_width", 64));
        push_u16(&mut formatted, int(memory, "data_width", 64));
        push_u16(&mut formatted, size_field);
        push_u8(&mut formatted, int(memory, "form_factor", 9));
        push_u8(&mut formatted, 0);
        push_u8(&mut formatted, locator as i64);
        push_u8(&mut formatted, bank as i64);
        push_u8(&mut formatted, int(memory, "memory_type", 26));
        push_u16(&mut formatted, int(memory, "type_detail", 128));
        push_u16(&mut formatted, int(memory, "speed_mts", 3200));
        push_u8(&mut formatted, dimm_man as i64);
        push_u8(&mut formatted, dimm_serial as i64);
        push_u8(&mut formatted, dimm_asset as i64);
        push_u8(&mut formatted, dimm_part as i64);
        push_u8(&mut formatted, int(memory, "rank", 2));
        push_u32(&mut formatted, extended);
        let speed = int(memory, "speed_mts", 3200);
        push_u16(&mut formatted, int(memory, "configured_speed_mts", speed));
        push_u16(&mut formatted, int(memory, "min_voltage_mv", 1200));
        push_u16(&mut formatted, int(memory, "max_voltage_mv", 1200));
        push_u16(&mut formatted, int(memory, "configured_voltage_mv", 1200));
        out.push(structure(
            17,
            0x1100u16.wrapping_add((index as u16) * 0x100),
            &formatted,
            &strings,
        )?);
    }

    // ---- type 19: memory array mapped address
    let mut formatted = Vec::new();
    push_u32(&mut formatted, 0);
    push_u32(&mut formatted, (total_kb - 1).max(0));
    push_u16(&mut formatted, 0x1000);
    push_u8(&mut formatted, 1);
    push_u64(&mut formatted, 0);
    push_u64(&mut formatted, (memory_mb as u64) * 1024 * 1024 - 1);
    out.push(structure(19, 0x1300, &formatted, &Strings::new())?);

    // ---- type 32: system boot information
    out.push(structure(32, 0x2000, &[0u8; 7], &Strings::new())?);

    for (index, extra) in smbios
        .get("extra_structures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let formatted = hex_to_bytes(extra.get("formatted").and_then(Value::as_str).unwrap_or(""))?;
        let mut strings = Strings::new();
        for item in string_list(extra.get("strings")) {
            strings.add_str(&ascii(Some(&Value::String(item)))?);
        }
        let stype = int(extra, "type", 1) as u8;
        let handle = int(extra, "handle", 0x7F00 + index as i64) as u16;
        out.push(structure(stype, handle, &formatted, &strings)?);
    }

    out.push(structure(127, 0x7F00, &[], &Strings::new())?);
    let data: Vec<u8> = out.into_iter().flatten().collect();
    validate_table(&data)?;
    Ok(data)
}

fn hex_to_bytes(text: &str) -> Result<Vec<u8>> {
    let compact: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    if !compact.len().is_multiple_of(2) || !compact.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return message("SMBIOS extra structure formatted field is invalid hex");
    }
    (0..compact.len() / 2)
        .map(|index| {
            u8::from_str_radix(&compact[index * 2..index * 2 + 2], 16)
                .map_err(|_| crate::Error::Message("invalid hex".to_string()))
        })
        .collect()
}

#[derive(Debug, Clone)]
pub struct Structure {
    pub stype: u8,
    pub length: u8,
    pub handle: u16,
    pub formatted: Vec<u8>,
    pub strings: Vec<String>,
}

pub fn parse_table(data: &[u8]) -> Result<Vec<Structure>> {
    let mut structures = Vec::new();
    let mut offset = 0;
    while offset < data.len() {
        if offset + 4 > data.len() {
            return message("truncated SMBIOS structure header");
        }
        let stype = data[offset];
        let length = data[offset + 1];
        if length < 4 {
            return message(format!(
                "invalid SMBIOS structure length {length} at offset {offset}"
            ));
        }
        if offset + length as usize > data.len() {
            return message(format!("truncated SMBIOS structure at offset {offset}"));
        }
        let handle = u16::from_le_bytes([data[offset + 2], data[offset + 3]]);
        let formatted = data[offset + 4..offset + length as usize].to_vec();
        let mut cursor = offset + length as usize;
        let mut strings = Vec::new();
        if cursor >= data.len() {
            return message(format!(
                "SMBIOS structure type {stype} has no string terminator"
            ));
        }
        if data[cursor] == 0 {
            cursor += 2;
        } else {
            while cursor < data.len() && data[cursor] != 0 {
                let end = data[cursor..]
                    .iter()
                    .position(|byte| *byte == 0)
                    .map(|index| cursor + index)
                    .ok_or_else(|| {
                        crate::Error::Message(format!("unterminated SMBIOS string in type {stype}"))
                    })?;
                strings.push(String::from_utf8_lossy(&data[cursor..end]).into_owned());
                cursor = end + 1;
            }
            cursor += 1;
        }
        structures.push(Structure {
            stype,
            length,
            handle,
            formatted,
            strings,
        });
        offset = cursor;
        if stype == 127 {
            if cursor != data.len() {
                return message("SMBIOS data continues after the end-of-table structure");
            }
            break;
        }
    }
    if structures.is_empty() || structures.last().map(|item| item.stype) != Some(127) {
        return message("SMBIOS table has no end-of-table (type 127) structure");
    }
    Ok(structures)
}

pub fn validate_table(data: &[u8]) -> Result<()> {
    if data.len() > 60 * 1024 {
        return message("SMBIOS table is larger than 60 KiB");
    }
    let structures = parse_table(data)?;
    let mut handles = HashSet::new();
    for item in &structures {
        if !handles.insert(item.handle) {
            return message("SMBIOS table contains duplicate handles");
        }
    }
    let required = [0u8, 1, 2, 3, 4, 16, 17, 19, 127];
    let present: HashSet<u8> = structures.iter().map(|item| item.stype).collect();
    let missing: Vec<u8> = required
        .iter()
        .copied()
        .filter(|value| !present.contains(value))
        .collect();
    if !missing.is_empty() {
        return message(format!(
            "SMBIOS table is missing required structure types: {missing:?}"
        ));
    }
    Ok(())
}

pub fn summary(data: &[u8]) -> Result<Value> {
    let structures = parse_table(data)?;
    Ok(serde_json::json!({
        "bytes": data.len(),
        "structures": structures.len(),
        "types": structures.iter().map(|item| item.stype).collect::<Vec<_>>(),
    }))
}
