//! Declarative device profile: defaults, merge, validation, resolution.
//!
//! The profile stays a plain nested JSON object so every field can be
//! overridden from a JSON file or with `--set a.b.c=value`.  Deterministic
//! identities (serials, UUID, MAC, NVMe/optical serials) are derived from the
//! instance name exactly like the Python reference implementation so
//! fingerprints remain stable across the migration.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::validation_regex;
use crate::paths::ResourcePaths;
use crate::py_json;
use crate::util::{message, read_json, sha256_text};
use crate::{Error, Result};

pub const GROUPS: [&str; 5] = ["cpu", "smbios", "devices", "network", "guest"];

const DIGEST_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ0123456789";

pub fn known_midrs() -> BTreeMap<&'static str, u32> {
    let mut map = BTreeMap::new();
    map.insert("neoverse-n1", 0x413F_D0C1);
    map.insert("neoverse-n2", 0x410F_D490);
    map.insert("cortex-a72", 0x410F_D083);
    map.insert("cortex-a76", 0x411F_D0D0);
    map.insert("cortex-a78", 0x411F_D410);
    map.insert("cortex-a710", 0x411F_D470);
    map
}

fn midr_models() -> BTreeMap<u32, &'static str> {
    known_midrs()
        .into_iter()
        .map(|(name, value)| (value, name))
        .collect()
}

pub fn load_defaults(project_root: &Path) -> Result<Value> {
    let path = ResourcePaths::new(project_root).device_config();
    match read_json(&path) {
        Some(Value::Object(map)) if !map.is_empty() => Ok(Value::Object(map)),
        _ => message(format!(
            "missing device profile defaults: {}",
            path.display()
        )),
    }
}

pub fn deep_merge(base: &Value, override_value: &Value) -> Value {
    match (base, override_value) {
        (Value::Object(base), Value::Object(override_map)) => {
            let mut result = base.clone();
            for (key, value) in override_map {
                let merged = match result.get(key) {
                    Some(existing) => deep_merge(existing, value),
                    None => value.clone(),
                };
                result.insert(key.clone(), merged);
            }
            Value::Object(result)
        }
        _ => override_value.clone(),
    }
}

pub fn get_path<'a>(data: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cursor = data;
    for part in path.split('.') {
        let Value::Object(map) = cursor else {
            return None;
        };
        cursor = map.get(part)?;
    }
    Some(cursor)
}

pub fn set_path(data: &mut Value, path: &str, value: Value) -> Result<()> {
    let parts: Vec<&str> = path.split('.').filter(|part| !part.is_empty()).collect();
    if parts.is_empty() {
        return message("empty --set path");
    }
    if !data.is_object() {
        *data = Value::Object(Map::new());
    }
    let mut cursor = data;
    for part in &parts[..parts.len() - 1] {
        if !cursor.is_object() {
            *cursor = Value::Object(Map::new());
        }
        let Value::Object(map) = cursor else {
            return message("could not construct --set object");
        };
        let entry = map
            .entry(part.to_string())
            .or_insert_with(|| Value::Object(Map::new()));
        if !entry.is_object() {
            *entry = Value::Object(Map::new());
        }
        cursor = entry;
    }
    let Value::Object(map) = cursor else {
        return message("could not construct --set object");
    };
    map.insert(parts[parts.len() - 1].to_string(), value);
    Ok(())
}

pub fn enabled(raw: &Value) -> bool {
    raw.get("enabled").and_then(Value::as_bool).unwrap_or(false)
}

pub fn parse_midr(value: Option<&Value>, model: Option<&str>) -> Result<u32> {
    let parsed = match value {
        None | Some(Value::Null) => {
            let Some(model) = model else {
                return message("device.cpu.midr is required when cpu.model is not a known preset");
            };
            match known_midrs().get(model) {
                Some(value) => *value,
                None => {
                    return message(
                        "device.cpu.midr is required when cpu.model is not a known preset",
                    );
                }
            }
        }
        Some(Value::String(text)) if text.is_empty() => {
            let Some(model) = model else {
                return message("device.cpu.midr is required when cpu.model is not a known preset");
            };
            match known_midrs().get(model) {
                Some(value) => *value,
                None => {
                    return message(
                        "device.cpu.midr is required when cpu.model is not a known preset",
                    );
                }
            }
        }
        Some(Value::Number(number)) => number
            .as_u64()
            .filter(|value| *value <= 0xFFFF_FFFF)
            .map(|value| value as u32)
            .ok_or_else(|| {
                crate::Error::Message("device.cpu.midr must fit in 32 bits".to_string())
            })?,
        Some(Value::String(text)) => {
            let stripped = text.trim();
            let hex = stripped
                .strip_prefix("0x")
                .or_else(|| stripped.strip_prefix("0X"));
            match hex {
                Some(hex)
                    if !hex.is_empty()
                        && hex.len() <= 8
                        && hex.chars().all(|ch| ch.is_ascii_hexdigit()) =>
                {
                    u32::from_str_radix(hex, 16).map_err(|_| {
                        crate::Error::Message("device.cpu.midr must fit in 32 bits".to_string())
                    })?
                }
                _ => {
                    return message(format!(
                        "device.cpu.midr must look like 0x413fd0c1, got {text:?}"
                    ));
                }
            }
        }
        Some(other) => {
            return message(format!(
                "device.cpu.midr must look like 0x413fd0c1, got {other}"
            ));
        }
    };
    Ok(parsed)
}

fn digest(name: &str, field: &str, length: usize, alphabet: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("vmctl-device:{name}:{field}").as_bytes());
    let bytes = hasher.finalize();
    bytes[..length]
        .iter()
        .map(|byte| alphabet[(*byte as usize) % alphabet.len()] as char)
        .collect()
}

fn uuid_for(name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("vmctl-device-uuid:{name}").as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    let time_low = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let time_mid = u16::from_be_bytes([bytes[4], bytes[5]]);
    let time_hi = u16::from_be_bytes([bytes[6], bytes[7]]);
    let clock_seq = u16::from_be_bytes([bytes[8], bytes[9]]);
    format!(
        "{time_low:08x}-{time_mid:04x}-{time_hi:04x}-{clock_seq:04x}-{}",
        crate::util::hex_lower(&bytes[10..16])
    )
}

fn mac_for_oui(oui: &str, name: &str) -> Result<String> {
    let parts: Vec<&str> = oui.split(':').collect();
    if parts.len() != 3 {
        return message("device.network.mac_oui must look like 00:1b:21");
    }
    let mut hasher = Sha256::new();
    hasher.update(format!("vmctl-device-mac:{name}").as_bytes());
    let digest = hasher.finalize();
    let first = u8::from_str_radix(parts[0], 16).map_err(|_| {
        crate::Error::Message("device.network.mac_oui must look like 00:1b:21".to_string())
    })?;
    let second = u8::from_str_radix(parts[1], 16).map_err(|_| {
        crate::Error::Message("device.network.mac_oui must look like 00:1b:21".to_string())
    })?;
    let third = u8::from_str_radix(parts[2], 16).map_err(|_| {
        crate::Error::Message("device.network.mac_oui must look like 00:1b:21".to_string())
    })?;
    Ok(format!(
        "{first:02x}:{second:02x}:{third:02x}:{:02x}:{:02x}:{:02x}",
        digest[0], digest[1], digest[2]
    ))
}

fn validate_text(
    value: Option<&Value>,
    field: &str,
    max_len: usize,
    allow_empty: bool,
) -> Result<()> {
    let Some(value) = value else {
        if allow_empty {
            return Ok(());
        }
        return message(format!("{field} is required"));
    };
    let Value::String(text) = value else {
        return message(format!("{field} must be a string"));
    };
    if text.len() > max_len {
        return message(format!("{field} is longer than {max_len} characters"));
    }
    if text.is_empty() && !allow_empty {
        return message(format!("{field} must not be empty"));
    }
    if text
        .chars()
        .any(|ch| (ch as u32) < 0x20 || ch as u32 == 0x7F)
    {
        return message(format!("{field} contains control characters"));
    }
    Ok(())
}

fn validate_int(
    value: Option<&Value>,
    field: &str,
    low: i64,
    high: i64,
    default: i64,
) -> Result<i64> {
    let value = match value {
        None | Some(Value::Null) => default,
        Some(Value::Bool(_)) => return message(format!("{field} must be an integer")),
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| crate::Error::Message(format!("{field} must be an integer")))?,
        Some(_) => return message(format!("{field} must be an integer")),
    };
    if value < low || value > high {
        return message(format!("{field} must be between {low} and {high}"));
    }
    Ok(value)
}

fn validate_bool(value: Option<&Value>, field: &str) -> Result<bool> {
    match value {
        Some(Value::Bool(value)) => Ok(*value),
        _ => message(format!("{field} must be true or false")),
    }
}

fn validate_string_list(value: Option<&Value>, field: &str) -> Result<Vec<String>> {
    match value {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut result = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(text) => result.push(text.clone()),
                    _ => return message(format!("{field} must be a list of strings")),
                }
            }
            Ok(result)
        }
        _ => message(format!("{field} must be a list of strings")),
    }
}

fn object<'a>(value: &'a Value, key: &str) -> &'a Map<String, Value> {
    static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
    value
        .get(key)
        .and_then(Value::as_object)
        .unwrap_or_else(|| EMPTY.get_or_init(Map::new))
}

fn require_object(value: &Value, key: &str, field: &str) -> Result<()> {
    match value.get(key) {
        Some(Value::Object(_)) => Ok(()),
        _ => message(format!("{field} must be an object")),
    }
}

pub fn validate(raw: &Value, name: &str, cpus: u32, memory_mb: u32) -> Result<()> {
    let Some(root) = raw.as_object() else {
        return message("device profile is empty");
    };
    if root.is_empty() {
        return message("device profile is empty");
    }
    if root.get("version").and_then(Value::as_i64).unwrap_or(1) != 1 {
        return message("unsupported device profile version");
    }
    match root.get("enabled") {
        Some(Value::Bool(_)) | None => {}
        _ => return message("device.enabled must be a boolean"),
    }

    let cpu = object(raw, "cpu");
    parse_midr(cpu.get("midr"), cpu.get("model").and_then(Value::as_str))?;
    let sockets = validate_int(cpu.get("sockets"), "device.cpu.sockets", 1, cpus as i64, 1)?;
    let threads = validate_int(
        cpu.get("threads_per_core"),
        "device.cpu.threads_per_core",
        1,
        8,
        1,
    )?;
    if let Some(cores) = cpu.get("cores_per_socket").filter(|value| !value.is_null()) {
        let cores = validate_int(Some(cores), "device.cpu.cores_per_socket", 1, 255, 1)?;
        if sockets * cores * threads != cpus as i64 {
            return message(format!(
                "device.cpu topology (sockets * cores_per_socket * threads_per_core) must equal cpus ({cpus})"
            ));
        }
    }
    let features = match cpu.get("qemu_features") {
        Some(Value::Object(map)) => map.clone(),
        None => Map::new(),
        _ => return message("device.cpu.qemu_features must be an object"),
    };
    let allowed_features = [
        "pmu", "pauth", "sme", "sme128", "sme256", "sme512", "sme1024", "sme2048", "sme_fa64",
        "aarch64",
    ];
    for (key, value) in &features {
        if !allowed_features.contains(&key.as_str()) {
            return message(format!(
                "device.cpu.qemu_features.{key} is not a QEMU host-CPU property; allowed: {}",
                allowed_features.join(", ")
            ));
        }
        validate_bool(Some(value), &format!("device.cpu.qemu_features.{key}"))?;
    }

    let smbios_value = raw.get("smbios").unwrap_or(&Value::Null);
    for section in [
        "bios",
        "system",
        "baseboard",
        "chassis",
        "processor",
        "memory",
        "oem",
    ] {
        require_object(smbios_value, section, &format!("device.smbios.{section}"))?;
    }
    let smbios = smbios_value
        .as_object()
        .ok_or_else(|| Error::Message("device.smbios must be an object".into()))?;
    let bios = object(smbios_value, "bios");
    for key in ["vendor", "version", "release_date"] {
        validate_text(
            bios.get(key),
            &format!("device.smbios.bios.{key}"),
            64,
            false,
        )?;
    }
    validate_int(
        bios.get("rom_size"),
        "device.smbios.bios.rom_size",
        0,
        255,
        255,
    )?;
    validate_int(bios.get("major"), "device.smbios.bios.major", 0, 255, 2)?;
    validate_int(bios.get("minor"), "device.smbios.bios.minor", 0, 255, 0)?;
    let bios_characteristics = [
        ("pci", 7),
        ("upgradeable", 11),
        ("cd_boot", 15),
        ("selectable_boot", 16),
        ("edd", 19),
    ];
    for item in validate_string_list(
        bios.get("characteristics"),
        "device.smbios.bios.characteristics",
    )? {
        if !bios_characteristics.iter().any(|(known, _)| *known == item) {
            return message(format!("unknown BIOS characteristic {item:?}"));
        }
    }
    let extension1 = [
        "acpi",
        "usb_legacy",
        "agp",
        "i2o",
        "ls120",
        "zip",
        "ieee1394",
        "smart_battery",
    ];
    for item in validate_string_list(bios.get("extension1"), "device.smbios.bios.extension1")? {
        if !extension1.contains(&item.as_str()) {
            return message(format!("unknown BIOS extension1 bit {item:?}"));
        }
    }
    let extension2 = [
        "bios_boot_spec",
        "network_boot",
        "targeted_content",
        "uefi",
        "virtual_machine",
        "manufacturing_supported",
        "manufacturing_enabled",
        "crash_dump",
    ];
    for item in validate_string_list(bios.get("extension2"), "device.smbios.bios.extension2")? {
        if !extension2.contains(&item.as_str()) {
            return message(format!("unknown BIOS extension2 bit {item:?}"));
        }
        if item == "virtual_machine" {
            return message("device.smbios.bios.extension2 must not contain 'virtual_machine'");
        }
    }

    let system = object(smbios_value, "system");
    for key in ["manufacturer", "product", "version", "sku", "family"] {
        validate_text(
            system.get(key),
            &format!("device.smbios.system.{key}"),
            64,
            false,
        )?;
    }
    if let Some(uuid) = system.get("uuid").and_then(Value::as_str)
        && uuid::is_nil_or_invalid(uuid)
    {
        return message(format!("device.smbios.system.uuid is invalid: {uuid:?}"));
    }
    validate_int(
        system.get("wakeup_type"),
        "device.smbios.system.wakeup_type",
        0,
        255,
        6,
    )?;

    let baseboard = object(smbios_value, "baseboard");
    for key in ["manufacturer", "product", "version", "location"] {
        validate_text(
            baseboard.get(key),
            &format!("device.smbios.baseboard.{key}"),
            64,
            false,
        )?;
    }
    validate_int(
        baseboard.get("board_type"),
        "device.smbios.baseboard.board_type",
        1,
        255,
        10,
    )?;

    let chassis = object(smbios_value, "chassis");
    validate_text(
        chassis.get("manufacturer"),
        "device.smbios.chassis.manufacturer",
        64,
        false,
    )?;
    validate_text(
        chassis.get("version"),
        "device.smbios.chassis.version",
        64,
        false,
    )?;
    validate_int(chassis.get("type"), "device.smbios.chassis.type", 1, 36, 17)?;
    validate_int(
        chassis.get("height_u"),
        "device.smbios.chassis.height_u",
        0,
        255,
        2,
    )?;
    validate_int(
        chassis.get("power_cords"),
        "device.smbios.chassis.power_cords",
        0,
        255,
        2,
    )?;

    let processor = object(smbios_value, "processor");
    for key in ["socket", "manufacturer", "version", "part"] {
        validate_text(
            processor.get(key),
            &format!("device.smbios.processor.{key}"),
            64,
            false,
        )?;
    }
    validate_int(
        processor.get("family"),
        "device.smbios.processor.family",
        0,
        65535,
        257,
    )?;
    let proc_characteristics = [
        "64bit",
        "multicore",
        "hardware_thread",
        "execute_protection",
        "enhanced_virtualization",
        "power_performance",
        "128bit",
    ];
    for item in validate_string_list(
        processor.get("characteristics"),
        "device.smbios.processor.characteristics",
    )? {
        if !proc_characteristics.contains(&item.as_str()) {
            return message(format!("unknown processor characteristic {item:?}"));
        }
    }
    let cache = match processor.get("cache") {
        Some(Value::Object(map)) => map.clone(),
        None => Map::new(),
        _ => return message("device.smbios.processor.cache must be an object"),
    };
    for key in ["l1_kb", "l2_kb", "l3_kb"] {
        validate_int(
            cache.get(key),
            &format!("device.smbios.processor.cache.{key}"),
            0,
            1024 * 1024,
            0,
        )?;
    }

    let memory = object(smbios_value, "memory");
    let dimms = validate_int(
        memory.get("dimm_count"),
        "device.smbios.memory.dimm_count",
        1,
        16,
        1,
    )?;
    if memory_mb as i64 % dimms != 0 {
        return message(format!(
            "memory ({memory_mb} MiB) must be divisible by device.smbios.memory.dimm_count ({dimms})"
        ));
    }
    validate_int(
        memory.get("form_factor"),
        "device.smbios.memory.form_factor",
        1,
        255,
        9,
    )?;
    validate_int(
        memory.get("memory_type"),
        "device.smbios.memory.memory_type",
        1,
        255,
        26,
    )?;
    validate_int(
        memory.get("type_detail"),
        "device.smbios.memory.type_detail",
        0,
        65535,
        128,
    )?;
    validate_int(
        memory.get("speed_mts"),
        "device.smbios.memory.speed_mts",
        0,
        65535,
        3200,
    )?;
    for key in ["manufacturer", "part", "locator_prefix", "bank_prefix"] {
        validate_text(
            memory.get(key),
            &format!("device.smbios.memory.{key}"),
            64,
            false,
        )?;
    }

    let oem = object(smbios_value, "oem");
    for key in ["acpi_oem_id", "acpi_oem_table_id"] {
        validate_text(oem.get(key), &format!("device.smbios.oem.{key}"), 8, false)?;
    }
    match smbios.get("extra_structures") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) => {
            for (index, item) in items.iter().enumerate() {
                let Some(item) = item.as_object() else {
                    return message(format!(
                        "device.smbios.extra_structures[{index}] must be an object"
                    ));
                };
                validate_int(
                    item.get("type"),
                    &format!("device.smbios.extra_structures[{index}].type"),
                    1,
                    255,
                    1,
                )?;
                validate_int(
                    item.get("handle"),
                    &format!("device.smbios.extra_structures[{index}].handle"),
                    0,
                    65535,
                    0x7F00,
                )?;
                let formatted = item.get("formatted").and_then(Value::as_str).unwrap_or("");
                if !formatted
                    .chars()
                    .all(|ch| ch.is_ascii_hexdigit() || ch == ' ')
                    || formatted.len() % 2 != 0
                {
                    return message(format!(
                        "device.smbios.extra_structures[{index}].formatted must be hex"
                    ));
                }
                validate_string_list(
                    item.get("strings"),
                    &format!("device.smbios.extra_structures[{index}].strings"),
                )?;
            }
        }
        _ => return message("device.smbios.extra_structures must be a list"),
    }

    let devices = object(raw, "devices");
    let storage = devices
        .get("storage")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let storage_controller = storage
        .get("controller")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !["nvme", "ahci"].contains(&storage_controller) {
        return message("device.devices.storage.controller must be one of [\"ahci\", \"nvme\"]");
    }
    validate_text(
        storage.get("model"),
        "device.devices.storage.model",
        40,
        false,
    )?;
    if let Some(version) = storage
        .get("firmware_version")
        .filter(|value| !value.is_null())
    {
        validate_text(
            Some(version),
            "device.devices.storage.firmware_version",
            16,
            false,
        )?;
    }
    let optical = devices
        .get("optical")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if optical
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        validate_text(
            optical.get("model"),
            "device.devices.optical.model",
            40,
            false,
        )?;
        let controller = optical
            .get("controller")
            .and_then(Value::as_str)
            .unwrap_or("lsi53c895a");
        if !["lsi53c895a", "ide"].contains(&controller) {
            return message("device.devices.optical.controller must be lsi53c895a or ide");
        }
        if optical.get("vendor").is_some_and(|value| !value.is_null()) {
            validate_text(
                optical.get("vendor"),
                "device.devices.optical.vendor",
                8,
                false,
            )?;
        }
        if optical.get("product").is_some_and(|value| !value.is_null()) {
            validate_text(
                optical.get("product"),
                "device.devices.optical.product",
                16,
                false,
            )?;
        }
        if optical.get("ver").is_some_and(|value| !value.is_null()) {
            validate_text(optical.get("ver"), "device.devices.optical.ver", 4, false)?;
        }
        let bus = optical
            .get("bus")
            .and_then(Value::as_str)
            .unwrap_or("scsi.0");
        if !validation_regex(r"^[a-z0-9_.-]+$")?.is_match(bus) {
            return message("device.devices.optical.bus must look like scsi.0");
        }
    }
    let usb = devices
        .get("usb")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if !["nec-usb-xhci", "qemu-xhci"]
        .contains(&usb.get("controller").and_then(Value::as_str).unwrap_or(""))
    {
        return message(
            "device.devices.usb.controller must be one of [\"nec-usb-xhci\", \"qemu-xhci\"]",
        );
    }
    let sound = devices
        .get("sound")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let sound_controller = sound
        .get("controller")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !["ich9-intel-hda", "intel-hda", "AC97"].contains(&sound_controller) {
        return message(
            "device.devices.sound.controller must be one of [\"AC97\", \"ich9-intel-hda\", \"intel-hda\"]",
        );
    }

    let network = object(raw, "network");
    let mode = network
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("user");
    if ![
        "user",
        "gateway",
        "socket-vmnet",
        "vmnet-shared",
        "vmnet-bridged",
    ]
    .contains(&mode)
    {
        return message(
            "device.network.mode must be one of [\"gateway\", \"socket-vmnet\", \"user\", \"vmnet-bridged\", \"vmnet-shared\"]",
        );
    }
    let nic = network
        .get("nic_model")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !["e1000e", "igb", "e1000", "rtl8139"].contains(&nic) {
        return message(
            "device.network.nic_model must be one of [\"e1000\", \"e1000e\", \"igb\", \"rtl8139\"]",
        );
    }
    let oui = network
        .get("mac_oui")
        .and_then(Value::as_str)
        .unwrap_or("00:1b:21");
    if !is_hex6(oui, 3) {
        return message("device.network.mac_oui must look like 00:1b:21");
    }
    let first_octet = u8::from_str_radix(&oui[..2], 16).unwrap_or(0);
    if first_octet & 1 != 0 {
        return message("device.network.mac_oui must be a unicast OUI");
    }
    if let Some(mac) = network.get("mac").filter(|value| !value.is_null()) {
        let Some(mac) = mac.as_str() else {
            return message("device.network.mac must look like 00:1b:21:aa:bb:cc");
        };
        if !is_hex6(mac, 6) {
            return message("device.network.mac must look like 00:1b:21:aa:bb:cc");
        }
        let first = u8::from_str_radix(&mac[..2], 16).unwrap_or(0);
        if first & 1 != 0 {
            return message("device.network.mac must be unicast");
        }
    }
    let interface = network
        .get("interface")
        .and_then(Value::as_str)
        .unwrap_or("enp0s2");
    if !validation_regex(r"^[A-Za-z0-9_.:-]{1,15}$")?.is_match(interface) {
        return message("device.network.interface is not a valid Linux interface name");
    }
    let address = network.get("address").and_then(Value::as_str).unwrap_or("");
    let gateway = network.get("gateway").and_then(Value::as_str).unwrap_or("");
    if !address.is_empty() {
        let Some((ip, prefix)) = parse_cidr(address) else {
            return message(format!("device.network.address is invalid: {address:?}"));
        };
        if !gateway.is_empty() {
            let Ok(gw) = gateway.parse::<std::net::IpAddr>() else {
                return message(format!("device.network.gateway is invalid: {gateway:?}"));
            };
            if !same_network(ip, prefix, gw) {
                return message(
                    "device.network.gateway must be inside device.network.address network",
                );
            }
        }
    }
    if let Some(dns) = network.get("dns").filter(|value| !value.is_null()) {
        let Some(dns) = dns.as_array() else {
            return message("device.network.dns must be a list");
        };
        for server in dns {
            let text = server.as_str().unwrap_or("");
            if text.parse::<std::net::IpAddr>().is_err() {
                return message(format!("device.network.dns entry is invalid: {server}"));
            }
        }
    }
    if let Some(ssh_host) = network.get("ssh_host").filter(|value| !value.is_null()) {
        let text = ssh_host.as_str().unwrap_or("");
        if text.parse::<std::net::IpAddr>().is_err() {
            validate_text(Some(ssh_host), "device.network.ssh_host", 253, false)?;
        }
    }
    if network
        .get("require_bridged")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && mode == "user"
    {
        return message(
            "device.network.require_bridged is true but device.network.mode is 'user'; use --network socket-vmnet with a bridged socket_vmnet daemon (or set device.network.require_bridged=false for a lab VM)",
        );
    }

    let guest = object(raw, "guest");
    let timezone = guest
        .get("timezone")
        .and_then(Value::as_str)
        .unwrap_or("Etc/UTC");
    validate_text(
        Some(&Value::String(timezone.to_string())),
        "device.guest.timezone",
        64,
        false,
    )?;
    if !valid_timezone(timezone) {
        return message(format!("device.guest.timezone is invalid: {timezone:?}"));
    }
    let locale = guest
        .get("locale")
        .and_then(Value::as_str)
        .unwrap_or("en_US.UTF-8");
    if !validation_regex(r"^[A-Za-z0-9_.@-]+$")?.is_match(locale) {
        return message("device.guest.locale is invalid");
    }
    if let Some(layout) = guest.get("keyboard_layout") {
        validate_text(Some(layout), "device.guest.keyboard_layout", 16, false)?;
    }
    let ntp = guest
        .get("ntp")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    validate_string_list(ntp.get("servers"), "device.guest.ntp.servers")?;
    validate_string_list(
        ntp.get("fallback_servers"),
        "device.guest.ntp.fallback_servers",
    )?;
    let package_re = validation_regex(r"^[a-z0-9][a-z0-9+.-]{0,63}$")?;
    for field in ["packages", "extra_packages", "purge_packages"] {
        for item in validate_string_list(guest.get(field), &format!("device.guest.{field}"))? {
            if !package_re.is_match(&item) {
                return message(format!(
                    "device.guest.{field} contains an invalid package name: {item:?}"
                ));
            }
        }
    }
    let user_re = validation_regex(r"^[a-z_][a-z0-9_-]{0,31}$")?;
    for item in validate_string_list(guest.get("remove_users"), "device.guest.remove_users")? {
        if !user_re.is_match(&item) {
            return message(format!(
                "device.guest.remove_users contains an invalid username: {item:?}"
            ));
        }
    }
    for key in [
        "regenerate_machine_id",
        "regenerate_ssh_host_keys",
        "clean_logs",
        "install_sound_modules",
        "create_efi_boot_entry",
    ] {
        if guest.contains_key(key) {
            validate_bool(guest.get(key), &format!("device.guest.{key}"))?;
        }
    }
    match guest.get("sysctl") {
        None | Some(Value::Null) => {}
        Some(Value::Object(sysctl)) => {
            let key_re = validation_regex(r"^[a-z0-9_.-]+$")?;
            for (key, value) in sysctl {
                if !key_re.is_match(key) {
                    return message(format!("invalid sysctl key {key:?}"));
                }
                validate_int(
                    Some(value),
                    &format!("device.guest.sysctl.{key}"),
                    0,
                    2_i64.pow(31) - 1,
                    0,
                )?;
            }
        }
        _ => return message("device.guest.sysctl must be an object"),
    }
    for path in validate_string_list(guest.get("remove_paths"), "device.guest.remove_paths")? {
        if !path.starts_with('/') {
            return message(format!(
                "device.guest.remove_paths entry must be absolute: {path:?}"
            ));
        }
    }
    match guest.get("extra_files") {
        None | Some(Value::Null) | Some(Value::Object(_)) => {}
        _ => return message("device.guest.extra_files must be an object"),
    }

    let verify = object(raw, "verify");
    let fail_on = verify
        .get("fail_on")
        .and_then(Value::as_str)
        .unwrap_or("error");
    if !["error", "warning"].contains(&fail_on) {
        return message("device.verify.fail_on must be 'error' or 'warning'");
    }
    validate_string_list(
        verify.get("forbidden_strings"),
        "device.verify.forbidden_strings",
    )?;

    let extra = object(raw, "extra");
    validate_string_list(extra.get("qemu_args"), "device.extra.qemu_args")?;
    validate_string_list(extra.get("guest_commands"), "device.extra.guest_commands")?;
    match extra.get("guest_files") {
        None | Some(Value::Null) | Some(Value::Object(_)) => {}
        _ => return message("device.extra.guest_files must be an object"),
    }
    let _ = smbios;
    let _ = name;
    Ok(())
}

mod uuid {
    use crate::Result;

    pub fn is_nil_or_invalid(text: &str) -> bool {
        let parts: Vec<&str> = text.split('-').collect();
        if parts.len() != 5
            || parts.iter().map(|part| part.len()).collect::<Vec<_>>() != vec![8, 4, 4, 4, 12]
        {
            return true;
        }
        if !parts
            .iter()
            .all(|part| part.chars().all(|ch| ch.is_ascii_hexdigit()))
        {
            return true;
        }
        let compact: String = parts.concat();
        let numeric = u128::from_str_radix(&compact, 16).unwrap_or(0);
        numeric == 0 || numeric == u128::MAX
    }

    pub fn _never() -> Result<()> {
        Ok(())
    }
}

fn is_hex6(text: &str, octets: usize) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    parts.len() == octets
        && parts
            .iter()
            .all(|part| part.len() == 2 && part.chars().all(|ch| ch.is_ascii_hexdigit()))
}

fn parse_cidr(text: &str) -> Option<(std::net::IpAddr, u8)> {
    let (address, prefix) = text.split_once('/')?;
    let ip: std::net::IpAddr = address.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return None;
    }
    Some((ip, prefix))
}

fn same_network(ip: std::net::IpAddr, prefix: u8, other: std::net::IpAddr) -> bool {
    match (ip, other) {
        (std::net::IpAddr::V4(ip), std::net::IpAddr::V4(other)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(ip) & mask) == (u32::from(other) & mask)
        }
        (std::net::IpAddr::V6(ip), std::net::IpAddr::V6(other)) => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(ip) & mask) == (u128::from(other) & mask)
        }
        _ => false,
    }
}

fn valid_timezone(text: &str) -> bool {
    if text.is_empty() || text.starts_with('/') || text.contains("..") {
        return false;
    }
    if text == "UTC" || text == "Etc/UTC" || text == "GMT" {
        return true;
    }
    let path = Path::new("/usr/share/zoneinfo").join(text);
    path.is_file()
}

/// Return a copy with all deterministic hardware identities filled in.
pub fn resolve(raw: &Value, name: &str, cpus: u32, memory_mb: u32) -> Result<Value> {
    let mut data = raw.clone();
    if !data.is_object() {
        data = Value::Object(Map::new());
    }
    ensure_object(&mut data, "cpu");
    ensure_object(&mut data, "smbios");
    ensure_object(&mut data, "devices");
    ensure_object(&mut data, "network");

    let midr = parse_midr(
        get_path(&data, "cpu.midr"),
        get_path(&data, "cpu.model").and_then(Value::as_str),
    )?;
    set_path(
        &mut data,
        "cpu.midr",
        Value::String(format!("0x{midr:08x}")),
    )?;
    set_path(&mut data, "cpu.midr_int", json!(midr))?;
    let sockets = get_path(&data, "cpu.sockets")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(1);
    let threads = get_path(&data, "cpu.threads_per_core")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(1);
    let cores = match get_path(&data, "cpu.cores_per_socket").and_then(Value::as_u64) {
        Some(value) if value > 0 => value,
        _ => {
            let computed = (cpus as u64) / (sockets * threads).max(1);
            let computed = computed.max(1);
            set_path(&mut data, "cpu.cores_per_socket", json!(computed))?;
            computed
        }
    };
    set_path(
        &mut data,
        "cpu.topology",
        json!({
            "cpus": cpus,
            "sockets": sockets,
            "cores": cores,
            "threads": threads,
        }),
    )?;
    let model = get_path(&data, "cpu.model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !known_midrs().contains_key(model.as_str()) {
        let model_name = midr_models()
            .get(&midr)
            .copied()
            .unwrap_or(if model.is_empty() {
                "custom"
            } else {
                model.as_str()
            });
        set_path(
            &mut data,
            "cpu.model",
            Value::String(model_name.to_string()),
        )?;
    }

    if get_path(&data, "smbios.system.serial")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.system.serial",
            Value::String(format!(
                "AMPR{}",
                digest(name, "system-serial", 10, DIGEST_ALPHABET)
            )),
        )?;
    }
    if get_path(&data, "smbios.system.uuid")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.system.uuid",
            Value::String(uuid_for(name)),
        )?;
    }
    if get_path(&data, "smbios.baseboard.serial")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.baseboard.serial",
            Value::String(format!(
                "BRD{}",
                digest(name, "board-serial", 10, DIGEST_ALPHABET)
            )),
        )?;
    }
    if get_path(&data, "smbios.baseboard.asset")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.baseboard.asset",
            Value::String(format!(
                "ASSET{}",
                digest(name, "board-asset", 7, DIGEST_ALPHABET)
            )),
        )?;
    }
    if get_path(&data, "smbios.chassis.serial")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.chassis.serial",
            Value::String(format!(
                "CHS{}",
                digest(name, "chassis-serial", 9, DIGEST_ALPHABET)
            )),
        )?;
    }
    if get_path(&data, "smbios.chassis.asset")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.chassis.asset",
            Value::String(format!(
                "ASSET{}",
                digest(name, "chassis-asset", 7, DIGEST_ALPHABET)
            )),
        )?;
    }
    if get_path(&data, "smbios.processor.serial")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.processor.serial",
            Value::String(format!(
                "CPU{}",
                digest(name, "cpu-serial", 9, DIGEST_ALPHABET)
            )),
        )?;
    }
    if get_path(&data, "smbios.processor.asset")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.processor.asset",
            Value::String(format!(
                "CPUASSET{}",
                digest(name, "cpu-asset", 5, DIGEST_ALPHABET)
            )),
        )?;
    }
    set_path(&mut data, "smbios.processor.id", json!(midr))?;

    let dimms = get_path(&data, "smbios.memory.dimm_count")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(1);
    let per_dimm = (memory_mb as u64) / dimms;
    set_path(&mut data, "smbios.memory.resolved_dimm_count", json!(dimms))?;
    set_path(&mut data, "smbios.memory.resolved_size_mb", json!(per_dimm))?;
    set_path(&mut data, "smbios.memory.total_mb", json!(memory_mb))?;
    if get_path(&data, "smbios.memory.serial").is_none() {
        set_path(&mut data, "smbios.memory.serial", Value::Null)?;
    }
    if get_path(&data, "smbios.memory.asset").is_none() {
        set_path(&mut data, "smbios.memory.asset", Value::Null)?;
    }
    if get_path(&data, "smbios.memory.maximum_capacity_kb")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "smbios.memory.maximum_capacity_kb",
            json!(memory_mb as u64 * 1024),
        )?;
    }

    if get_path(&data, "devices.storage.serial")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "devices.storage.serial",
            Value::String(format!(
                "PHMB{}",
                digest(
                    name,
                    "nvme-serial",
                    12,
                    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ"
                )
            )),
        )?;
    }
    if get_path(&data, "devices.optical.serial")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        set_path(
            &mut data,
            "devices.optical.serial",
            Value::String(digest(name, "optical-serial", 12, DIGEST_ALPHABET)),
        )?;
    }
    if get_path(&data, "network.mac")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        let oui = get_path(&data, "network.mac_oui")
            .and_then(Value::as_str)
            .unwrap_or("00:1b:21")
            .to_string();
        set_path(
            &mut data,
            "network.mac",
            Value::String(mac_for_oui(&oui, name)?),
        )?;
    }
    if get_path(&data, "network.ssh_host")
        .map(Value::is_null)
        .unwrap_or(true)
    {
        // Keep an explicitly configured host, otherwise leave null.
        if get_path(&data, "network.ssh_host").is_none() {
            set_path(&mut data, "network.ssh_host", Value::Null)?;
        }
    }
    Ok(data)
}

fn ensure_object(data: &mut Value, key: &str) {
    if data.get(key).map(Value::is_object).unwrap_or(false) {
        return;
    }
    let _ = set_path(data, key, Value::Object(Map::new()));
}

pub fn group_payload(resolved: &Value, group: &str, cpus: u32, memory_mb: u32) -> Result<Value> {
    let version = resolved.get("version").cloned().unwrap_or(json!(1));
    let mut payload = Map::new();
    payload.insert("version".to_string(), version);
    payload.insert("group".to_string(), Value::String(group.to_string()));
    match group {
        "cpu" => {
            payload.insert(
                "cpu".to_string(),
                resolved.get("cpu").cloned().unwrap_or(Value::Null),
            );
            payload.insert("cpus".to_string(), json!(cpus));
        }
        "smbios" => {
            payload.insert(
                "smbios".to_string(),
                resolved.get("smbios").cloned().unwrap_or(Value::Null),
            );
            payload.insert("cpus".to_string(), json!(cpus));
            payload.insert("memory_mb".to_string(), json!(memory_mb));
        }
        "devices" => {
            payload.insert(
                "devices".to_string(),
                resolved.get("devices").cloned().unwrap_or(Value::Null),
            );
        }
        "network" => {
            payload.insert(
                "network".to_string(),
                resolved.get("network").cloned().unwrap_or(Value::Null),
            );
        }
        "guest" => {
            payload.insert(
                "guest".to_string(),
                resolved.get("guest").cloned().unwrap_or(Value::Null),
            );
        }
        other => return message(format!("unknown device group {other:?}")),
    }
    Ok(Value::Object(payload))
}

pub fn group_fingerprint(
    resolved: &Value,
    group: &str,
    cpus: u32,
    memory_mb: u32,
) -> Result<String> {
    Ok(sha256_text(&py_json::dumps_compact(&group_payload(
        resolved, group, cpus, memory_mb,
    )?)))
}

pub fn fingerprint(resolved: &Value, cpus: u32, memory_mb: u32) -> Result<String> {
    let mut groups = Map::new();
    for group in GROUPS {
        groups.insert(
            group.to_string(),
            Value::String(group_fingerprint(resolved, group, cpus, memory_mb)?),
        );
    }
    let payload = json!({
        "version": resolved.get("version").cloned().unwrap_or(json!(1)),
        "enabled": resolved.get("enabled").cloned().unwrap_or(json!(false)),
        "cpus": cpus,
        "memory_mb": memory_mb,
        "groups": Value::Object(groups),
    });
    Ok(sha256_text(&py_json::dumps_compact(&payload)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_override_replaces_non_objects_and_rejects_an_empty_path() {
        let mut value = json!({"network": 42});
        set_path(&mut value, "network.mode", json!("user")).unwrap();
        assert_eq!(value["network"]["mode"], "user");
        set_path(&mut value, "network.address.ip", json!("127.0.0.1")).unwrap();
        assert_eq!(value["network"]["address"]["ip"], "127.0.0.1");
        assert!(set_path(&mut value, ".", json!(true)).is_err());
        let mut scalar = json!(true);
        set_path(&mut scalar, "enabled", json!(false)).unwrap();
        assert_eq!(scalar["enabled"], false);
    }
}
