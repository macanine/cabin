//! QEMU argument rendering for the five device aspects.
//!
//! `install` is the bootable compatibility set (virtio root/seed, device CPU,
//! SMBIOS, NIC and non-boot devices).  `runtime` is the sealed hardware: NVMe
//! root, SATA/SCSI optical, e1000e NIC, NEC xHCI and Intel HDA.  No virtio, no
//! guest agent, no virtio-rng.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::Result;
use crate::config::Config;
use crate::util::message;

fn esc(value: Option<&Value>) -> String {
    let text = match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    };
    text.replace(',', ",,")
}

fn s(value: &str) -> String {
    value.to_string()
}

fn int(value: &Value, key: &str, default: i64) -> i64 {
    value
        .get(key)
        .and_then(|value| value.as_i64().or_else(|| value.as_u64().map(|v| v as i64)))
        .unwrap_or(default)
}

fn text<'a>(value: &'a Value, key: &str, default: &'a str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or(default)
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

pub fn resolved_profile(cfg: &Config) -> Result<Value> {
    if !cfg.device_enabled() {
        return message("device is not enabled for this instance");
    }
    cfg.device_resolved()
}

pub fn machine_args(resolved: &Value) -> Vec<String> {
    let smbios = object(resolved, "smbios");
    let oem = object(smbios, "oem");
    let mut machine = "virt,accel=hvf,highmem=on,usb=off".to_string();
    if let Some(id) = oem.get("acpi_oem_id").and_then(Value::as_str)
        && !id.is_empty()
    {
        machine.push_str(",x-oem-id=");
        machine.push_str(&esc(Some(&json!(id))));
    }
    if let Some(id) = oem.get("acpi_oem_table_id").and_then(Value::as_str)
        && !id.is_empty()
    {
        machine.push_str(",x-oem-table-id=");
        machine.push_str(&esc(Some(&json!(id))));
    }
    vec!["-machine".to_string(), machine]
}

pub fn cpu_args(resolved: &Value) -> Vec<String> {
    let cpu = object(resolved, "cpu");
    let topology = object(cpu, "topology");
    let midr = cpu
        .get("midr")
        .and_then(Value::as_str)
        .unwrap_or("0x413fd0c1");
    let mut model = format!("host,midr={midr}");
    if let Some(features) = cpu.get("qemu_features").and_then(Value::as_object) {
        let mut keys: Vec<&String> = features.keys().collect();
        keys.sort();
        for key in keys {
            let enabled = features.get(key).and_then(Value::as_bool).unwrap_or(false);
            model.push_str(&format!(",{key}={}", if enabled { "on" } else { "off" }));
        }
    }
    let smp = format!(
        "cpus={},sockets={},cores={},threads={}",
        int(topology, "cpus", 1),
        int(topology, "sockets", 1),
        int(topology, "cores", 1),
        int(topology, "threads", 1)
    );
    vec!["-cpu".to_string(), model, "-smp".to_string(), smp]
}

pub fn smbios_args(cfg: &Config) -> Vec<String> {
    vec![
        "-smbios".to_string(),
        format!("file={}", cfg.smbios_path().display()),
    ]
}

fn usb_args(resolved: &Value) -> Vec<String> {
    let usb = object(object(resolved, "devices"), "usb");
    if !usb.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
        return Vec::new();
    }
    let controller = text(usb, "controller", "nec-usb-xhci");
    let mut args = vec!["-device".to_string(), format!("{controller},id=usb")];
    for (index, device) in string_list(usb.get("devices")).iter().enumerate() {
        args.push("-device".to_string());
        args.push(format!("{device},bus=usb.0,port={}", index + 1));
    }
    args
}

fn sound_args(resolved: &Value, audio_backend: &str) -> Vec<String> {
    let sound = object(object(resolved, "devices"), "sound");
    if !sound
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        return Vec::new();
    }
    let audiodev = if audio_backend != "none" {
        audio_backend.to_string()
    } else {
        text(sound, "audiodev", "none").to_string()
    };
    let controller = text(sound, "controller", "ich9-intel-hda");
    let mut args = vec!["-audiodev".to_string(), format!("{audiodev},id=audio0")];
    if controller == "AC97" {
        args.push("-device".to_string());
        args.push("AC97,audiodev=audio0".to_string());
        return args;
    }
    args.push("-device".to_string());
    args.push(format!("{controller},id=snd"));
    let codec = text(sound, "codec", "hda-duplex");
    let codec_cad = int(sound, "codec_cad", 0);
    args.push("-device".to_string());
    args.push(format!("{codec},bus=snd.0,cad={codec_cad},audiodev=audio0"));
    args
}

fn storage_install_args(cfg: &Config) -> Vec<String> {
    vec![
        "-drive".to_string(),
        format!(
            "if=none,id=disk,format=qcow2,file={},cache=writeback",
            cfg.disk_path().display()
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=disk,bootindex=0".to_string(),
    ]
}

fn seed_install_args(cfg: &Config) -> Vec<String> {
    vec![
        "-drive".to_string(),
        format!(
            "if=none,id=seed,format=raw,file={},readonly=on",
            cfg.seed_path().display()
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=seed,bootindex=2".to_string(),
    ]
}

fn storage_runtime_args(cfg: &Config, resolved: &Value) -> Vec<String> {
    let storage = object(object(resolved, "devices"), "storage");
    let controller = text(storage, "controller", "nvme");
    if controller == "nvme" {
        let use_intel = storage
            .get("use_intel_id")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let suffix = if use_intel { ",use-intel-id=on" } else { "" };
        return vec![
            "-drive".to_string(),
            format!(
                "if=none,id=disk,format=qcow2,file={},cache=writeback,discard=unmap",
                cfg.disk_path().display()
            ),
            "-device".to_string(),
            format!(
                "nvme,id=nvme0,serial={},model={},firmware-version={}{}",
                esc(storage.get("serial")),
                esc(storage.get("model")),
                esc(storage.get("firmware_version")),
                suffix
            ),
            "-device".to_string(),
            format!(
                "nvme-ns,drive=disk,bus=nvme0,bootindex={}",
                int(storage, "bootindex", 0)
            ),
        ];
    }
    vec![
        "-device".to_string(),
        "ich9-ahci,id=sata".to_string(),
        "-drive".to_string(),
        format!(
            "if=none,id=disk,format=qcow2,file={},cache=writeback,discard=unmap",
            cfg.disk_path().display()
        ),
        "-device".to_string(),
        format!(
            "ide-hd,drive=disk,bus=sata.0,model={},serial={},bootindex={}",
            esc(storage.get("model")),
            esc(storage.get("serial")),
            int(storage, "bootindex", 0)
        ),
    ]
}

fn optical_args(resolved: &Value) -> Result<Vec<String>> {
    let optical = object(object(resolved, "devices"), "optical");
    if !optical
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        return Ok(Vec::new());
    }
    let controller = text(optical, "controller", "lsi53c895a");
    let bus = text(optical, "bus", "scsi.0");
    let model = text(optical, "model", "HL-DT-ST DVDRAM GU90N");
    let vendor = match optical.get("vendor").and_then(Value::as_str) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => model.split(' ').next().unwrap_or("").to_string(),
    };
    let product = match optical.get("product").and_then(Value::as_str) {
        Some(value) if !value.is_empty() => value.to_string(),
        _ => {
            let mut parts = model.splitn(2, ' ');
            parts.next();
            parts.next().unwrap_or("DVDRAM").to_string()
        }
    };
    let serial = optical.get("serial").and_then(Value::as_str).unwrap_or("");
    if controller == "lsi53c895a" {
        if !bus.starts_with("scsi.") {
            return message("device optical bus must look like scsi.0 for lsi53c895a");
        }
        return Ok(vec![
            "-device".to_string(),
            "lsi53c895a,id=scsi".to_string(),
            "-device".to_string(),
            format!(
                "scsi-cd,bus={bus},scsi-id={},lun={},vendor={},product={},serial={},ver={}",
                int(optical, "scsi_id", 0),
                int(optical, "lun", 0),
                esc(Some(&json!(vendor))),
                esc(Some(&json!(product))),
                esc(Some(&json!(serial))),
                esc(Some(&json!(text(optical, "ver", "1.00")))),
            ),
        ]);
    }
    if controller == "ide" {
        if !bus.starts_with("sata.") {
            return message("device optical bus must look like sata.1 for the IDE model");
        }
        let storage_controller = text(
            object(object(resolved, "devices"), "storage"),
            "controller",
            "nvme",
        );
        let mut args = Vec::new();
        if storage_controller != "ahci" {
            args.push("-device".to_string());
            args.push("ich9-ahci,id=sata".to_string());
        }
        args.push("-device".to_string());
        args.push(format!(
            "ide-cd,bus={bus},model={},serial={}",
            esc(optical.get("model")),
            esc(Some(&json!(serial)))
        ));
        return Ok(args);
    }
    message(format!(
        "unsupported device optical controller {controller:?}"
    ))
}

pub fn device_args(cfg: &Config, resolved: &Value, phase: &str) -> Result<Vec<String>> {
    let devices = object(resolved, "devices");
    let mut args = match phase {
        "install" => {
            let mut args = storage_install_args(cfg);
            args.extend(seed_install_args(cfg));
            args.extend(usb_args(resolved));
            args.extend(sound_args(resolved, &cfg.audio_backend));
            args
        }
        "runtime" => {
            let mut args = storage_runtime_args(cfg, resolved);
            args.extend(optical_args(resolved)?);
            args.extend(usb_args(resolved));
            args.extend(sound_args(resolved, &cfg.audio_backend));
            args
        }
        other => return message(format!("unknown QEMU phase {other:?}")),
    };
    args.extend(string_list(devices.get("extra_qemu_args")));
    Ok(args)
}

pub fn normalize_cidr(text: &str) -> Option<String> {
    let (address, prefix) = text.split_once('/')?;
    let ip: std::net::IpAddr = address.parse().ok()?;
    let prefix: u8 = prefix.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    if prefix > max {
        return None;
    }
    Some(format!("{ip}/{prefix}"))
}

pub fn network_plan(resolved: &Value, hostname: Option<&str>) -> Result<Value> {
    let network = object(resolved, "network");
    if text(network, "mode", "user") == "gateway" {
        let data_mac = crate::netgw::GUEST_MAC
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":");
        return Ok(json!({"version": 2, "ethernets": {
            "data0": {"match": {"macaddress": data_mac}, "set-name": "data0", "dhcp4": true,
                "dhcp6": false, "accept-ra": false, "link-local": ["ipv4"]}
        }}));
    }
    let interface = text(network, "interface", "enp0s2");
    let has_address = network
        .get("address")
        .and_then(Value::as_str)
        .map(|text| !text.is_empty())
        .unwrap_or(false);
    let mut ethernet = Map::new();
    ethernet.insert(
        "match".to_string(),
        json!({"macaddress": network.get("mac").cloned().unwrap_or(Value::Null)}),
    );
    ethernet.insert("set-name".to_string(), json!(interface));
    ethernet.insert("dhcp4".to_string(), json!(!has_address));
    ethernet.insert("optional".to_string(), json!(false));
    if has_address {
        let address = network.get("address").and_then(Value::as_str).unwrap_or("");
        let normalized = normalize_cidr(address).ok_or_else(|| {
            crate::Error::Message(format!("invalid device.network.address {address:?}"))
        })?;
        ethernet.insert("addresses".to_string(), json!([normalized]));
        ethernet.insert("dhcp4".to_string(), json!(false));
        if let Some(gateway) = network
            .get("gateway")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            ethernet.insert(
                "routes".to_string(),
                json!([{"to": "default", "via": gateway}]),
            );
        }
        if let Some(dns) = network.get("dns").and_then(Value::as_array)
            && !dns.is_empty()
        {
            ethernet.insert("nameservers".to_string(), json!({"addresses": dns}));
        }
    } else {
        let mut overrides = Map::new();
        if network
            .get("dhcp_hostname")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && let Some(hostname) = hostname
        {
            overrides.insert("hostname".to_string(), json!(hostname));
        }
        if let Some(extra) = network.get("dhcp_overrides").and_then(Value::as_object) {
            for (key, value) in extra {
                overrides.insert(key.clone(), value.clone());
            }
        }
        if !overrides.is_empty() {
            ethernet.insert("dhcp4-overrides".to_string(), Value::Object(overrides));
        }
    }
    if network
        .get("ipv6_privacy")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        ethernet.insert("ipv6-privacy".to_string(), json!(true));
        ethernet.insert(
            "ipv6-address-generation".to_string(),
            json!("stable-privacy"),
        );
        ethernet.insert("dhcp6".to_string(), json!(false));
        ethernet.insert("accept-ra".to_string(), json!(true));
    }
    Ok(json!({
        "version": 2,
        "ethernets": {interface: Value::Object(ethernet)},
    }))
}

pub fn netplan_document(resolved: &Value, hostname: Option<&str>) -> Result<Value> {
    Ok(json!({"network": network_plan(resolved, hostname)?}))
}

pub fn network_args(cfg: &Config, resolved: &Value) -> Result<Vec<String>> {
    let network = object(resolved, "network");
    let mode = text(network, "mode", "user");
    let nic = text(network, "nic_model", "e1000e");
    let mac = network.get("mac").cloned().unwrap_or(Value::Null);
    let mut args = Vec::new();
    match mode {
        "gateway" => {
            let qemu = crate::vm::resolve_qemu(cfg)?.qemu_bin;
            return Ok(vec![
                "-netdev".into(),
                format!(
                    "stream,id=data,server=off,addr.type=unix,addr.path={},{}",
                    cfg.net_socket().display(),
                    crate::vm::stream_reconnect_option(&qemu)
                ),
                "-device".into(),
                format!("{nic},netdev=data,mac={}", cfg.mac_data()),
            ]);
        }
        "user" => {
            args.push("-netdev".to_string());
            args.push(crate::proxy::user_netdev(cfg, "net0")?);
        }
        "socket-vmnet" => {
            args.push("-netdev".to_string());
            args.push("socket,id=net0,fd=3".to_string());
        }
        "vmnet-shared" => {
            args.push("-netdev".to_string());
            args.push("vmnet-shared,id=net0".to_string());
        }
        "vmnet-bridged" => {
            args.push("-netdev".to_string());
            args.push(format!(
                "vmnet-bridged,id=net0,ifname={}",
                esc(Some(&json!(cfg.bridge_if)))
            ));
        }
        other => return message(format!("unsupported device network mode {other:?}")),
    }
    args.push("-device".to_string());
    args.push(format!("{nic},netdev=net0,mac={}", esc(Some(&mac))));
    args.extend(string_list(network.get("extra_qemu_args")));
    Ok(args)
}

pub fn ssh_endpoint(cfg: &Config, resolved: Option<&Value>) -> Result<(String, u16)> {
    let resolved = match resolved {
        Some(value) => value.clone(),
        None => resolved_profile(cfg)?,
    };
    let network = object(&resolved, "network");
    if matches!(text(network, "mode", "user"), "user" | "gateway") {
        return Ok(("127.0.0.1".to_string(), cfg.ssh_connect_port()));
    }
    let mut host = network
        .get("ssh_host")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned);
    if host.is_none()
        && let Some(address) = network
            .get("address")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        && let Some((ip, _)) = address.split_once('/')
    {
        host = Some(ip.to_string());
    }
    let Some(host) = host else {
        return message(
            "device network mode is not 'user' but no reachable SSH host is configured; set device.network.address (static) or device.network.ssh_host",
        );
    };
    let port = network
        .get("ssh_port")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(22) as u16;
    Ok((host, port))
}

pub fn graphics_args(cfg: &Config, display_mode: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if cfg.gpu_mode == "virtio" {
        args.push("-device".to_string());
        args.push("virtio-gpu-pci,edid=on".to_string());
    } else if cfg.gpu_mode == "ramfb" {
        args.push("-device".to_string());
        args.push("ramfb".to_string());
    }
    if cfg.usb_tablet {
        args.push("-device".to_string());
        args.push("usb-tablet".to_string());
        args.push("-device".to_string());
        args.push("usb-kbd".to_string());
    }
    let display = display_mode.unwrap_or(&cfg.display_mode);
    args.push("-display".to_string());
    args.push(if display == "cocoa" { "cocoa" } else { "none" }.to_string());
    args
}

#[allow(clippy::too_many_arguments)]
pub fn full_args(
    cfg: &Config,
    phase: &str,
    qemu_bin: &str,
    firmware_dir: Option<&Path>,
    firmware_code: &Path,
    serial_log: &Path,
    daemonize: bool,
    display_mode: Option<&str>,
) -> Result<Vec<String>> {
    let resolved = resolved_profile(cfg)?;
    let mut args = vec![s(qemu_bin)];
    if let Some(directory) = firmware_dir {
        args.push("-L".to_string());
        args.push(directory.display().to_string());
    }
    args.push("-name".to_string());
    args.push(format!("vmctl-{}", cfg.name));
    args.extend(machine_args(&resolved));
    args.extend(cpu_args(&resolved));
    args.push("-m".to_string());
    args.push(cfg.memory_mb.to_string());
    args.push("-drive".to_string());
    args.push(format!(
        "if=pflash,format=raw,unit=0,readonly=on,file={}",
        firmware_code.display()
    ));
    args.push("-drive".to_string());
    args.push(format!(
        "if=pflash,format=raw,unit=1,file={}",
        cfg.efi_vars_path().display()
    ));
    args.extend(smbios_args(cfg));
    args.extend(device_args(cfg, &resolved, phase)?);
    args.extend(network_args(cfg, &resolved)?);
    args.extend(graphics_args(cfg, display_mode));
    args.extend([
        "-rtc".to_string(),
        "base=utc,clock=host,driftfix=none".to_string(),
        "-monitor".to_string(),
        "none".to_string(),
        "-chardev".to_string(),
        format!(
            "socket,id=console0,path={},server=on,wait=off,logfile={},logappend=on",
            cfg.console_socket().display(),
            serial_log.display()
        ),
        "-serial".to_string(),
        "chardev:console0".to_string(),
        "-qmp".to_string(),
        format!("unix:{},server=on,wait=off", cfg.qmp_socket().display()),
        "-pidfile".to_string(),
        cfg.pidfile().display().to_string(),
    ]);
    if let Some(extra) = resolved
        .get("extra")
        .and_then(|value| value.get("qemu_args"))
        .and_then(Value::as_array)
    {
        for item in extra {
            if let Some(text) = item.as_str() {
                args.push(text.to_string());
            }
        }
    }
    if daemonize {
        args.push("-daemonize".to_string());
    }
    Ok(args)
}
