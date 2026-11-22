//! `vmctl` command line interface.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::browser;
use crate::config::{build_config, default_project_root, load_instance_config, resolve_home};
use crate::console;
use crate::device::profile as device_profile;
use crate::device::render as device_render;
use crate::proxy;
use crate::runner::Runner;
use crate::util::{capture, message, read_json, tail_file};
use crate::vm::{Runtime, preflight};
use crate::{Config, Result};

const DEFAULT_NAME: &str = "dev";

pub struct Args {
    pub command: String,
    pub values: BTreeMap<String, Vec<String>>,
    pub flags: BTreeSet<String>,
    pub positionals: Vec<String>,
    pub ssh_args: Vec<String>,
}

impl Args {
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values
            .get(name)
            .and_then(|items| items.first())
            .map(String::as_str)
    }
    pub fn values(&self, name: &str) -> &[String] {
        self.values.get(name).map(Vec::as_slice).unwrap_or(&[])
    }
    pub fn has(&self, name: &str) -> bool {
        self.flags.contains(name)
    }
}

fn value_flags() -> BTreeSet<&'static str> {
    [
        "home",
        "name",
        "user",
        "hostname",
        "password",
        "ssh-port",
        "cpus",
        "memory",
        "disk-size",
        "network",
        "bridge",
        "socket-vmnet-socket",
        "socket-vmnet-client",
        "socket-vmnet-mode",
        "proxy",
        "device-profile",
        "set",
        "device-timezone",
        "device-locale",
        "device-vendor",
        "device-product",
        "device-cpu",
        "device-mac-oui",
        "device-nic",
        "device-storage-model",
        "device-address",
        "device-gateway",
        "device-ssh-host",
        "device-ssh-port",
        "qemu-bin",
        "desktop",
        "desktop-extra-package",
        "desktop-extra-command",
        "display",
        "desktop-resolution",
        "desktop-scale",
        "gpu",
        "audio",
        "browser-profile",
        "browser-ua",
        "browser-platform",
        "browser-timezone",
        "browser-locale",
        "browser-hw",
        "browser-mem",
        "webgl-vendor",
        "webgl-renderer",
        "canvas-noise",
        "audio-noise",
        "image-url",
        "image-sha256",
        "image-sha512",
        "os",
        "step",
        "timezone",
        "locale",
        "group",
    ]
    .into_iter()
    .collect()
}

fn bool_flags() -> BTreeSet<&'static str> {
    [
        "device",
        "no-device",
        "device-allow-user-network",
        "chrome",
        "usb-tablet",
        "browser",
        "no-browser",
        "start",
        "json",
        "check",
        "last",
        "force",
        "serial",
        "list",
        "follow",
        "yes",
        "all",
        "no-boot",
        "keep-running",
        "raw",
        "web",
        "stop-web",
        "help",
    ]
    .into_iter()
    .collect()
}

fn canonical_flag(name: &str) -> &str {
    match name {
        "\x73\x74\x65\x61\x6c\x74\x68" => "device",
        "no-\x73\x74\x65\x61\x6c\x74\x68" => "no-device",
        "\x73\x74\x65\x61\x6c\x74\x68-allow-user-network" => "device-allow-user-network",
        "\x73\x74\x65\x61\x6c\x74\x68-profile" => "device-profile",
        "\x73\x74\x65\x61\x6c\x74\x68-timezone" => "device-timezone",
        "\x73\x74\x65\x61\x6c\x74\x68-locale" => "device-locale",
        "\x73\x74\x65\x61\x6c\x74\x68-vendor" => "device-vendor",
        "\x73\x74\x65\x61\x6c\x74\x68-product" => "device-product",
        "\x73\x74\x65\x61\x6c\x74\x68-cpu" => "device-cpu",
        "\x73\x74\x65\x61\x6c\x74\x68-mac-oui" => "device-mac-oui",
        "\x73\x74\x65\x61\x6c\x74\x68-nic" => "device-nic",
        "\x73\x74\x65\x61\x6c\x74\x68-storage-model" => "device-storage-model",
        "\x73\x74\x65\x61\x6c\x74\x68-address" => "device-address",
        "\x73\x74\x65\x61\x6c\x74\x68-gateway" => "device-gateway",
        "\x73\x74\x65\x61\x6c\x74\x68-ssh-host" => "device-ssh-host",
        "\x73\x74\x65\x61\x6c\x74\x68-ssh-port" => "device-ssh-port",
        _ => name,
    }
}

pub fn parse_args(argv: &[String]) -> Result<Args> {
    let value_flags = value_flags();
    let bool_flags = bool_flags();
    let mut args = Args {
        command: String::new(),
        values: BTreeMap::new(),
        flags: BTreeSet::new(),
        positionals: Vec::new(),
        ssh_args: Vec::new(),
    };
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if arg == "--version" {
            println!("vmctl 0.1.0");
            std::process::exit(0);
        }
        if arg == "-h" || arg == "--help" {
            args.flags.insert("help".to_string());
            index += 1;
            continue;
        }
        if arg == "--" {
            if args.command == "ssh" {
                args.ssh_args.extend(argv[index + 1..].iter().cloned());
            } else {
                args.positionals.extend(argv[index + 1..].iter().cloned());
            }
            break;
        }
        if let Some(rest) = arg.strip_prefix("--") {
            let (raw_name, inline) = match rest.split_once('=') {
                Some((name, value)) => (name.to_string(), Some(value.to_string())),
                None => (rest.to_string(), None),
            };
            let name = canonical_flag(&raw_name).to_string();
            if bool_flags.contains(name.as_str()) {
                if inline.is_some() {
                    return message(format!("--{name} does not take a value"));
                }
                args.flags.insert(name);
            } else if value_flags.contains(name.as_str()) {
                let value = match inline {
                    Some(value) => value,
                    None => {
                        index += 1;
                        let Some(value) = argv.get(index) else {
                            return message(format!("--{name} requires a value"));
                        };
                        value.clone()
                    }
                };
                args.values.entry(name).or_default().push(value);
            } else {
                return message(format!("unknown option --{name}"));
            }
        } else if args.command.is_empty() {
            args.command = arg.clone();
        } else if args.command == "ssh" {
            args.ssh_args.extend(argv[index..].iter().cloned());
            break;
        } else {
            args.positionals.push(arg.clone());
        }
        index += 1;
    }
    if args.flags.contains("help") && args.command.is_empty() {
        args.command = "help".to_string();
    }
    if args.command.is_empty() {
        return message("a command is required (plan, apply, status, start, stop, ...)");
    }
    Ok(args)
}

fn device_overrides(args: &Args) -> Result<(Option<Value>, Map<String, Value>)> {
    let mut top = Map::new();
    let shortcuts = [
        ("device-timezone", "guest.timezone"),
        ("device-locale", "guest.locale"),
        ("device-vendor", "smbios.system.manufacturer"),
        ("device-product", "smbios.system.product"),
        ("device-cpu", "cpu.model"),
        ("device-mac-oui", "network.mac_oui"),
        ("device-nic", "network.nic_model"),
        ("device-storage-model", "devices.storage.model"),
        ("device-address", "network.address"),
        ("device-gateway", "network.gateway"),
        ("device-ssh-host", "network.ssh_host"),
        ("device-ssh-port", "network.ssh_port"),
    ];
    let explicit = args.has("device")
        || args.has("no-device")
        || args.value("device-profile").is_some()
        || !args.values("set").is_empty()
        || shortcuts.iter().any(|(flag, _)| args.value(flag).is_some())
        || args.has("device-allow-user-network");
    if !explicit {
        return Ok((None, top));
    }
    let mut data = Value::Object(Map::new());
    if let Some(profile_path) = args.value("device-profile") {
        let parsed = read_json(&PathBuf::from(profile_path)).ok_or_else(|| {
            crate::Error::Message(format!("cannot read device profile JSON: {profile_path}"))
        })?;
        if !parsed.is_object() {
            return message(format!("cannot read device profile JSON: {profile_path}"));
        }
        data = device_profile::deep_merge(&Value::Object(Map::new()), &parsed);
    }
    for (flag, path) in shortcuts {
        if let Some(value) = args.value(flag) {
            let parsed = if path == "network.ssh_port" {
                json!(value.parse::<u16>().unwrap_or(0))
            } else {
                Value::String(value.to_string())
            };
            device_profile::set_path(&mut data, path, parsed)?;
        }
    }
    if args.has("device-allow-user-network") {
        device_profile::set_path(&mut data, "verify.allow_user_network", json!(true))?;
        device_profile::set_path(&mut data, "network.require_bridged", json!(false))?;
    }
    let mut device_touch = args.value("device-profile").is_some()
        || args.has("device-allow-user-network")
        || shortcuts.iter().any(|(flag, _)| args.value(flag).is_some());
    for entry in args.values("set") {
        let Some((path, raw)) = entry.split_once('=') else {
            return message(format!("--set expects PATH=VALUE, got {entry:?}"));
        };
        let path = path.trim();
        if path.is_empty() {
            return message("--set path is empty");
        }
        let value = crate::py_json::parse_loose_value(raw);
        if let Some(rest) = path.strip_prefix("device.") {
            device_profile::set_path(&mut data, rest, value)?;
            device_touch = true;
        } else if path.starts_with("browser.") {
            continue;
        } else if path.starts_with("extra.")
            || path.starts_with("guest.")
            || path.starts_with("cpu.")
            || path.starts_with("smbios.")
            || path.starts_with("devices.")
            || path.starts_with("network.")
            || path.starts_with("verify.")
        {
            device_profile::set_path(&mut data, path, value)?;
            device_touch = true;
        } else {
            top.insert(path.to_string(), value);
        }
    }
    if (args.has("device") || device_touch)
        && let Some(map) = data.as_object_mut()
    {
        map.entry("enabled".to_string()).or_insert(json!(true));
    }
    if args.has("no-device") {
        device_profile::set_path(&mut data, "enabled", json!(false))?;
    }
    Ok((Some(data), top))
}

fn browser_overrides(args: &Args) -> Result<Option<Value>> {
    let shortcuts = [
        ("browser-ua", "navigator.user_agent"),
        ("browser-platform", "navigator.platform"),
        ("browser-timezone", "timezone"),
        ("browser-locale", "locale"),
        ("browser-hw", "navigator.hardware_concurrency"),
        ("browser-mem", "navigator.device_memory"),
        ("webgl-vendor", "webgl.vendor"),
        ("webgl-renderer", "webgl.renderer"),
        ("canvas-noise", "canvas.noise"),
        ("audio-noise", "audio.noise"),
    ];
    let browser_sets: Vec<&String> = args
        .values("set")
        .iter()
        .filter(|entry| entry.starts_with("browser."))
        .collect();
    let explicit = args.has("browser")
        || args.has("no-browser")
        || args.value("browser-profile").is_some()
        || !browser_sets.is_empty()
        || shortcuts.iter().any(|(flag, _)| args.value(flag).is_some());
    if !explicit {
        return Ok(None);
    }
    let mut data = Value::Object(Map::new());
    if let Some(profile_path) = args.value("browser-profile") {
        let parsed = read_json(&PathBuf::from(profile_path)).ok_or_else(|| {
            crate::Error::Message(format!("cannot read browser profile JSON: {profile_path}"))
        })?;
        if !parsed.is_object() {
            return message(format!("cannot read browser profile JSON: {profile_path}"));
        }
        data = browser::deep_merge(&Value::Object(Map::new()), &parsed);
    }
    for (flag, path) in shortcuts {
        if let Some(value) = args.value(flag) {
            let parsed = match flag {
                "browser-hw" => json!(value.parse::<i64>().unwrap_or(0)),
                "browser-mem" | "canvas-noise" | "audio-noise" => {
                    json!(value.parse::<f64>().unwrap_or(0.0))
                }
                _ => Value::String(value.to_string()),
            };
            browser::set_path(&mut data, path, parsed)?;
        }
    }
    for entry in &browser_sets {
        let Some((path, raw)) = entry.split_once('=') else {
            continue;
        };
        browser::set_path(
            &mut data,
            path.trim_start_matches("browser."),
            crate::py_json::parse_loose_value(raw),
        )?;
    }
    if (args.has("browser")
        || !browser_sets.is_empty()
        || args.value("browser-profile").is_some()
        || shortcuts.iter().any(|(flag, _)| args.value(flag).is_some()))
        && let Some(map) = data.as_object_mut()
    {
        map.entry("enabled".to_string()).or_insert(json!(true));
    }
    if args.has("no-browser") {
        browser::set_path(&mut data, "enabled", json!(false))?;
    }
    Ok(Some(data))
}

pub fn config_overrides(args: &Args) -> Result<Value> {
    let mut overrides = Map::new();
    let scalars = [
        ("home", "home"),
        ("name", "name"),
        ("user", "username"),
        ("hostname", "hostname"),
        ("password", "password"),
        ("ssh-port", "ssh_port"),
        ("cpus", "cpus"),
        ("memory", "memory_mb"),
        ("disk-size", "disk_size"),
        ("network", "network_mode"),
        ("bridge", "bridge_if"),
        ("socket-vmnet-socket", "socket_vmnet_socket"),
        ("socket-vmnet-client", "socket_vmnet_client"),
        ("socket-vmnet-mode", "socket_vmnet_mode"),
        ("proxy", "proxy"),
        ("display", "display_mode"),
        ("desktop-resolution", "desktop_resolution"),
        ("desktop-scale", "desktop_scale"),
        ("gpu", "gpu_mode"),
        ("audio", "audio_backend"),
        ("image-url", "image_url"),
        ("image-sha256", "image_sha256"),
        ("image-sha512", "image_sha512"),
        ("os", "os"),
    ];
    for (flag, key) in scalars {
        if let Some(value) = args.value(flag) {
            let parsed = match key {
                "ssh_port" | "cpus" | "memory_mb" => {
                    json!(value.parse::<u64>().unwrap_or(0))
                }
                "desktop_scale" => json!(value.parse::<f64>().unwrap_or(0.0)),
                _ => json!(value),
            };
            overrides.insert(key.to_string(), parsed);
        }
    }
    if let Some(value) = args.value("desktop") {
        overrides.insert(
            "desktop_env".to_string(),
            if value == "none" {
                Value::Null
            } else {
                json!(value)
            },
        );
    }
    if !args.values("desktop-extra-package").is_empty() {
        overrides.insert(
            "desktop_extra_packages".to_string(),
            json!(args.values("desktop-extra-package")),
        );
    }
    if !args.values("desktop-extra-command").is_empty() {
        overrides.insert(
            "desktop_extra_commands".to_string(),
            json!(args.values("desktop-extra-command")),
        );
    }
    for (flag, key) in [("chrome", "desktop_chrome"), ("usb-tablet", "usb_tablet")] {
        if args.has(flag) {
            overrides.insert(key.to_string(), json!(true));
        }
    }
    if let Some(value) = browser_overrides(args)? {
        overrides.insert("browser".to_string(), value);
    }
    let (device, top) = device_overrides(args)?;
    for (key, value) in top {
        overrides.insert(key, value);
    }
    if let Some(device) = device
        && device
            .as_object()
            .map(|map| !map.is_empty())
            .unwrap_or(false)
    {
        overrides.insert("device".to_string(), device);
    }
    Ok(Value::Object(overrides))
}

fn cfg_for_new(args: &Args) -> Result<Config> {
    build_config(&config_overrides(args)?, &default_project_root())
}

fn cfg_for_existing(args: &Args) -> Result<Config> {
    let home = resolve_home(args.value("home"));
    let name = args.value("name").unwrap_or(DEFAULT_NAME);
    load_instance_config(&default_project_root(), &home, name)
}

fn print_credentials(cfg: &Config) -> Result<()> {
    let credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
    if !credentials.is_object() {
        return Ok(());
    }
    let (host, port) = if cfg.device_enabled() {
        device_render::ssh_endpoint(cfg, None)?
    } else {
        ("127.0.0.1".to_string(), cfg.ssh_connect_port())
    };
    let fallback_username = cfg.current_username();
    println!();
    println!(
        "guest login: {} / {}",
        credentials
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or(&fallback_username),
        credentials
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    println!(
        "ssh:         {}",
        credentials
            .get("ssh_key")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    println!(
        "ssh command: ssh -i {} -p {port} {}@{host}",
        cfg.ssh_key_path().display(),
        cfg.current_username()
    );
    println!(
        "credentials: {} (mode 0600)",
        cfg.credentials_path().display()
    );
    Ok(())
}

pub fn cmd_plan(args: &Args) -> Result<i32> {
    let cfg = cfg_for_new(args)?;
    let runner = Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary());
    let rows = runner.plan()?;
    println!("instance: {}   home: {}", cfg.name, cfg.home.display());
    println!(
        "network:  {}   ssh: 127.0.0.1:{}",
        cfg.network_mode, cfg.ssh_port
    );
    println!("image:    {}", cfg.image_path().display());
    println!();
    println!("{:<11} {:<9} {:<9} DETAIL", "STEP", "SCOPE", "STATUS");
    for row in rows {
        let status = if row["ok"].as_bool().unwrap_or(false) {
            "ready"
        } else {
            "pending"
        };
        println!(
            "{:<11} {:<9} {:<9} {}",
            row["id"].as_str().unwrap_or(""),
            row["scope"].as_str().unwrap_or(""),
            status,
            row["detail"].as_str().unwrap_or("")
        );
    }
    println!();
    println!("apply will run pending steps with automatic rollback on failure");
    Ok(0)
}

pub fn cmd_apply(args: &Args) -> Result<i32> {
    let mut cfg = cfg_for_new(args)?;
    cfg.adopt_stored_image();
    cfg.pin_image()?;
    Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary()).apply()?;
    print_credentials(&cfg)?;
    if cfg.device_enabled() {
        let report = read_json(&cfg.device_report_path()).unwrap_or(Value::Null);
        if report.is_object() {
            println!();
            println!(
                "device: {} errors={} warnings={}",
                report.get("status").and_then(Value::as_str).unwrap_or("?"),
                report.get("errors").and_then(Value::as_u64).unwrap_or(0),
                report.get("warnings").and_then(Value::as_u64).unwrap_or(0)
            );
            println!("report:  {}", cfg.device_report_path().display());
            println!("verify:  vmctl device verify --name {}", cfg.name);
        }
    }
    if args.has("start") {
        Runner::new(cfg)?.with_agentd(crate::agentd::default_binary()).start_vm()?;
    }
    Ok(0)
}

pub fn cmd_status(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    let data = Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary()).status()?;
    if args.has("json") {
        println!("{}", crate::py_json::dumps_pretty_with(&data, true));
    } else {
        let meta = data.get("instance_meta").cloned().unwrap_or(Value::Null);
        println!("instance: {}   home: {}", cfg.name, cfg.home.display());
        println!(
            "config:   user={} host={} network={} ssh_port={}",
            cfg.current_username(),
            cfg.current_hostname(),
            cfg.effective_network_mode(),
            cfg.ssh_port
        );
        println!(
            "created:  {}",
            meta.get("created_at")
                .and_then(crate::util::timestamp_millis)
                .map(crate::util::utcstamp_from_millis)
                .unwrap_or_else(|| "?".into())
        );
        println!();
        println!(
            "{:<11} {:<9} {:<12} {:<6} DETAIL",
            "STEP", "SCOPE", "STATE", "CHECK"
        );
        if let Some(steps) = data.get("steps").and_then(Value::as_array) {
            for row in steps {
                let check = if row["ok"].as_bool().unwrap_or(false) {
                    "ok"
                } else {
                    "PENDING"
                };
                println!(
                    "{:<11} {:<9} {:<12} {:<6} {}",
                    row["id"].as_str().unwrap_or(""),
                    row["scope"].as_str().unwrap_or(""),
                    row["state"].as_str().unwrap_or(""),
                    check,
                    row["detail"].as_str().unwrap_or("")
                );
            }
        }
        let runtime = data.get("runtime").cloned().unwrap_or(Value::Null);
        let state = if runtime
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "running"
        } else if runtime
            .get("alive")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "dead"
        } else {
            "stopped"
        };
        println!();
        println!(
            "vm:        {state}  pid={}  ssh=127.0.0.1:{}",
            runtime
                .get("pid")
                .and_then(Value::as_u64)
                .map(|value| value.to_string())
                .unwrap_or_else(|| "None".to_string()),
            cfg.ssh_port
        );
        println!(
            "disk:      {}",
            data["paths"]["disk"].as_str().unwrap_or("")
        );
        println!(
            "serial:    {}",
            data["paths"]["serial_log"].as_str().unwrap_or("")
        );
        println!(
            "journal:   {}",
            data["paths"]["journal"].as_str().unwrap_or("")
        );
        if data
            .get("credentials_present")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            println!(
                "login:     credentials.json present (run: vmctl credentials --name {})",
                cfg.name
            );
        }
        let device = data.get("device").cloned().unwrap_or(Value::Null);
        if device
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let report = device.get("report").cloned().unwrap_or(Value::Null);
            println!();
            println!(
                "device:   phase={} sealed={} report={} errors={} warnings={}",
                device
                    .get("runtime_phase")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                device
                    .get("sealed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                report.get("status").and_then(Value::as_str).unwrap_or("?"),
                report.get("errors").and_then(Value::as_u64).unwrap_or(0),
                report.get("warnings").and_then(Value::as_u64).unwrap_or(0)
            );
            if let Some(groups) = device.get("groups").and_then(Value::as_object) {
                for (group, info) in groups {
                    println!(
                        "  {:<8} state={}",
                        group,
                        info.get("state").and_then(Value::as_str).unwrap_or("?")
                    );
                }
            }
        }
    }
    if args.has("check") {
        let problems = data
            .get("steps")
            .and_then(Value::as_array)
            .map(|steps| {
                steps
                    .iter()
                    .any(|row| !row["ok"].as_bool().unwrap_or(false))
            })
            .unwrap_or(false);
        let device_bad = data["device"]["enabled"].as_bool().unwrap_or(false)
            && !data["device"]["report"]["acceptable"]
                .as_bool()
                .unwrap_or(false);
        if problems || !data["runtime"]["running"].as_bool().unwrap_or(false) || device_bad {
            return Ok(1);
        }
    }
    Ok(0)
}

pub fn cmd_rollback(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    Runner::new(cfg)?.with_agentd(crate::agentd::default_binary()).rollback(args.value("step"), args.has("last"), args.has("force"))?;
    Ok(0)
}

pub fn cmd_start(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    Runner::new(cfg)?.with_agentd(crate::agentd::default_binary()).start_vm()?;
    Ok(0)
}

pub fn cmd_stop(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    Runner::new(cfg)?.with_agentd(crate::agentd::default_binary()).stop_vm()?;
    Ok(0)
}

pub fn cmd_console(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    let runtime = Runtime::new(&cfg);
    if !runtime.running() {
        if args.has("start") {
            Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary()).start_vm()?;
        } else {
            return message(format!(
                "instance {:?} is not running; use 'vmctl start --name {}' or 'vmctl console --name {} --start'",
                cfg.name, cfg.name, cfg.name
            ));
        }
    }
    console::attach(&cfg.console_socket())?;
    Ok(0)
}

pub fn cmd_ssh(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    let runtime = Runtime::new(&cfg);
    if !runtime.running() {
        return message(format!(
            "instance {:?} is not running; run 'vmctl start --name {}'",
            cfg.name, cfg.name
        ));
    }
    let mut argv = runtime.ssh_argv(args.ssh_args.is_empty())?;
    argv.extend(args.ssh_args.iter().cloned());
    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()?;
    Ok(status.code().unwrap_or(0))
}

pub fn cmd_logs(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    let runner = Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary());
    let files = runner.logs(args.value("step"), args.has("serial"));
    if files.is_empty() {
        println!("no logs found");
        return Ok(1);
    }
    if args.has("list") {
        for path in files {
            println!("{}", path.display());
        }
        return Ok(0);
    }
    let target = &files[0];
    if args.has("follow") {
        let status = std::process::Command::new("tail")
            .args(["-n", "200", "-f"])
            .arg(target)
            .status()?;
        return Ok(status.code().unwrap_or(0));
    }
    println!("==> {} <==", target.display());
    println!("{}", tail_file(target, 200));
    Ok(0)
}

pub fn cmd_credentials(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    let credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
    if !credentials.is_object() {
        return message(format!(
            "no credentials for {:?} (has it been provisioned?)",
            cfg.name
        ));
    }
    let (host, port) = if cfg.device_enabled() {
        device_render::ssh_endpoint(&cfg, None)?
    } else {
        ("127.0.0.1".to_string(), cfg.ssh_connect_port())
    };
    println!(
        "name:      {}",
        credentials
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    println!(
        "hostname:  {}",
        credentials
            .get("hostname")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    println!(
        "username:  {}",
        credentials
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    println!(
        "password:  {}",
        credentials
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    println!("ssh host:  {host}");
    println!("ssh port:  {port}");
    println!(
        "ssh key:   {}",
        credentials
            .get("ssh_key")
            .and_then(Value::as_str)
            .unwrap_or("")
    );
    if let Some(proxy) = credentials.get("guest_proxy").and_then(Value::as_str) {
        println!("guest proxy: {proxy}");
    }
    println!();
    println!(
        "ssh -i {} -p {port} {}@{host}",
        cfg.ssh_key_path().display(),
        cfg.current_username()
    );
    Ok(0)
}

pub fn cmd_reconfigure(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    if !args.has("yes") {
        println!(
            "reconfigure will change the guest identity (host records are restored if it fails):"
        );
        println!(
            "  current: user={} host={} timezone={} locale={}",
            cfg.current_username(),
            cfg.current_hostname(),
            cfg.current_timezone(),
            cfg.current_locale()
        );
        let current_user = cfg.current_username();
        let current_host = cfg.current_hostname();
        let current_tz = cfg.current_timezone();
        let current_locale = cfg.current_locale();
        println!(
            "  new:     user={} host={} timezone={} locale={}",
            args.value("user").unwrap_or(&current_user),
            args.value("hostname").unwrap_or(&current_host),
            args.value("timezone").unwrap_or(&current_tz),
            args.value("locale").unwrap_or(&current_locale)
        );
        return message("re-run with --yes to apply");
    }
    let result = Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary()).reconfigure(
        args.value("user"),
        args.value("hostname"),
        args.value("timezone"),
        args.value("locale"),
    )?;
    println!("{}", crate::py_json::dumps_pretty_with(&result, true));
    let cfg = cfg_for_existing(args)?;
    let (host, port) = if cfg.device_enabled() {
        device_render::ssh_endpoint(&cfg, None)?
    } else {
        ("127.0.0.1".to_string(), cfg.ssh_connect_port())
    };
    println!();
    println!(
        "new ssh: ssh -i {} -p {port} {}@{host}",
        cfg.ssh_key_path().display(),
        cfg.current_username()
    );
    Ok(0)
}

pub fn cmd_destroy(args: &Args) -> Result<i32> {
    if !args.has("yes") {
        return message("destroy is irreversible; re-run with --yes");
    }
    let cfg = cfg_for_existing(args)?;
    Runner::new(cfg)?.with_agentd(crate::agentd::default_binary()).destroy(args.has("all"))?;
    Ok(0)
}

pub fn cmd_show_config(args: &Args) -> Result<i32> {
    let cfg = cfg_for_new(args)?;
    let payload = json!({
        "config": cfg.storage(),
        "fingerprints": {
            "overall": cfg.fingerprint(),
            "disk": cfg.disk_fingerprint(),
            "seed": cfg.seed_fingerprint(),
        },
        "paths": {
            "home": cfg.home.display().to_string(),
            "instance_dir": cfg.instance_dir().display().to_string(),
            "image": cfg.image_path().display().to_string(),
            "disk": cfg.disk_path().display().to_string(),
            "seed": cfg.seed_path().display().to_string(),
            "credentials": cfg.credentials_path().display().to_string(),
        },
        "guest_proxy_url": cfg.guest_proxy_url(),
        "device_enabled": cfg.device_enabled(),
        "device_profile": if cfg.device_enabled() { cfg.device_resolved()? } else { Value::Null },
        "device_fingerprint": cfg.device_fingerprint(None)?,
        "mac_mgmt": cfg.mac_mgmt(),
        "mac_data": if cfg.needs_second_nic() { Some(cfg.mac_data()) } else { None },
    });
    println!("{}", crate::py_json::dumps_pretty_with(&payload, true));
    Ok(0)
}

pub fn cmd_proxy(args: &Args) -> Result<i32> {
    let home = resolve_home(args.value("home"));
    let action = args
        .positionals
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    match action {
        "status" => {
            let result = proxy::status(&home)?;
            if args.has("json") {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("engine:  {}", result.engine);
                println!("version: {}", result.version);
                println!(
                    "state:   {}",
                    if result.running { "running" } else { "stopped" }
                );
                println!("listen:  {}", result.listen);
                println!("log:     {}", proxy::log_path_for(&home).display());
            }
        }
        "check" => {
            let result = proxy::check(&home)?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        "start" => {
            let result = proxy::start(&home)?;
            println!(
                "shoes running: pid={} listen={}",
                result.pid.unwrap_or(0),
                result.listen
            );
        }
        "stop" => {
            let result = proxy::stop(&home, args.has("force"))?;
            println!("shoes stopped: {}", !result.running);
        }
        other => {
            return message(format!(
                "unknown proxy action {other:?} (use status, check, start or stop)"
            ));
        }
    }
    Ok(0)
}

pub fn cmd_bridge_setup(args: &Args) -> Result<i32> {
    let bridge = args.value("bridge").unwrap_or("en0");
    let shared_socket = args
        .value("socket-vmnet-socket")
        .unwrap_or("/opt/homebrew/var/run/socket_vmnet");
    let client = args
        .value("socket-vmnet-client")
        .unwrap_or("/opt/homebrew/opt/socket_vmnet/bin/socket_vmnet_client");
    let bridged_socket = format!("/opt/homebrew/var/run/socket_vmnet.bridged.{bridge}");
    println!("macOS rootless bridge setup (one-time sudo)");
    println!();
    println!("1) install the helper (no sudo):");
    println!("   brew install socket_vmnet");
    println!();
    println!("2a) shared/NAT mode (socket_vmnet daemon as a root service):");
    println!("   sudo brew services start socket_vmnet");
    println!("   # socket: {shared_socket}  gateway: 192.168.105.1");
    println!();
    println!("2b) bridged mode (guest gets a DHCP address on the LAN):");
    println!("   sudo /opt/homebrew/opt/socket_vmnet/bin/socket_vmnet \\");
    println!("     --vmnet-mode=bridged --vmnet-interface={bridge} {bridged_socket}");
    println!("   # run it in the foreground or install the launchd plist from");
    println!("   # /opt/homebrew/opt/socket_vmnet/share/doc/socket_vmnet/launchd/");
    println!();
    println!("3) create the VM:");
    println!("   vmctl apply --name demo --user alice --hostname alice-vm \\");
    println!("     --network socket-vmnet --socket-vmnet-mode shared");
    println!("   # or --socket-vmnet-mode bridged --socket-vmnet-socket {bridged_socket}");
    println!();
    println!("4) HTTP proxy: bind it to 0.0.0.0 and pass --proxy http://<host-ip>:<port>.");
    println!("   localhost proxies are rewritten to the host address reachable from the guest.");
    println!();
    println!(
        "current: client={}  shared_socket={}  bridged_socket={}",
        if Path::new(client).exists() {
            "ok"
        } else {
            "missing"
        },
        if Path::new(shared_socket).exists() {
            "ok"
        } else {
            "missing"
        },
        if Path::new(&bridged_socket).exists() {
            "ok"
        } else {
            "missing"
        }
    );
    Ok(0)
}

pub fn cmd_doctor(args: &Args) -> Result<i32> {
    let cfg = cfg_for_new(args)?;
    let issues = preflight(&cfg);
    if !issues.is_empty() {
        println!("preflight issues:");
        for issue in &issues {
            println!("  - {issue}");
        }
        return Ok(1);
    }
    println!("preflight: ok");
    println!("home: {}", cfg.home.display());
    let version = capture(
        &["qemu-system-aarch64".to_string(), "--version".to_string()],
        std::time::Duration::from_secs(5),
        None,
    );
    let first = version.output.lines().next().unwrap_or("").trim();
    println!(
        "qemu: {}",
        if first.is_empty() {
            "not installed"
        } else {
            first
        }
    );
    println!("vmctl lock: free");
    Ok(0)
}

pub fn cmd_device(args: &Args) -> Result<i32> {
    let cfg = cfg_for_existing(args)?;
    let action = args.positionals.first().map(String::as_str).unwrap_or("");
    if action == "status" {
        let data = Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary()).device_status()?;
        if args.has("json") {
            println!("{}", crate::py_json::dumps_pretty_with(&data, true));
        } else if !data
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            println!("device: disabled");
        } else {
            println!(
                "device: enabled  phase={}  sealed={}",
                data.get("runtime_phase")
                    .and_then(Value::as_str)
                    .unwrap_or("?"),
                data.get("sealed").and_then(Value::as_bool).unwrap_or(false)
            );
            println!(
                "profile: {}",
                data.get("profile_fingerprint")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            );
            println!(
                "report:  {} errors={} warnings={} ({})",
                data["report"]["status"].as_str().unwrap_or("?"),
                data["report"]["errors"].as_u64().unwrap_or(0),
                data["report"]["warnings"].as_u64().unwrap_or(0),
                data["report"]["path"].as_str().unwrap_or("")
            );
            if let Some(groups) = data.get("groups").and_then(Value::as_object) {
                for (group, info) in groups {
                    let fp = info
                        .get("fingerprint")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    println!(
                        "  {:<8} state={:<10} fp={}",
                        group,
                        info.get("state").and_then(Value::as_str).unwrap_or("?"),
                        &fp[..fp.len().min(12)]
                    );
                }
            }
        }
        return Ok(0);
    }
    if action == "verify" {
        let report = Runner::new(cfg.clone())?.with_agentd(crate::agentd::default_binary()).verify_device(
            args.value("group"),
            !args.has("no-boot"),
            args.has("keep-running"),
        )?;
        if args.has("json") {
            println!("{}", crate::py_json::dumps_pretty_with(&report, true));
        } else {
            print_report(&report, args.value("group"));
        }
        return Ok(if report["acceptable"].as_bool() == Some(true) { 0 } else { 1 });
    }
    if action == "report" {
        let Some(report) = read_json(&cfg.device_report_path()) else {
            return message(format!(
                "no device report for {:?}; run 'vmctl device verify --name {}'",
                cfg.name, cfg.name
            ));
        };
        if args.has("raw") {
            let raw = read_json(&cfg.device_raw_path()).unwrap_or(Value::Null);
            println!("{}", crate::py_json::dumps_pretty_with(&raw, true));
            return Ok(0);
        }
        if args.has("json") {
            println!("{}", crate::py_json::dumps_pretty_with(&report, true));
        } else {
            print_report(&report, None);
        }
        return Ok(
            if report
                .get("acceptable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                0
            } else {
                1
            },
        );
    }
    if action == "show" {
        if !cfg.device_enabled() {
            println!(
                "{}",
                crate::py_json::dumps_pretty_with(&json!({"enabled": false}), true)
            );
            return Ok(0);
        }
        let resolved = cfg.device_resolved()?;
        let (host, port) = device_render::ssh_endpoint(&cfg, Some(&resolved))?;
        let mut groups = Map::new();
        for group in device_profile::GROUPS {
            groups.insert(
                group.to_string(),
                json!(cfg.device_fingerprint(Some(group))?),
            );
        }
        let payload = json!({
            "enabled": true,
            "profile": resolved,
            "fingerprint": cfg.device_fingerprint(None)?,
            "group_fingerprints": Value::Object(groups),
            "paths": {
                "smbios": cfg.smbios_path().display().to_string(),
                "runtime_marker": cfg.device_runtime_marker().display().to_string(),
                "report": cfg.device_report_path().display().to_string(),
                "raw": cfg.device_raw_path().display().to_string(),
            },
            "install_args": device_render::device_args(&cfg, &cfg.device_resolved()?, "install")?,
            "runtime_args": device_render::device_args(&cfg, &cfg.device_resolved()?, "runtime")?,
            "machine_args": device_render::machine_args(&cfg.device_resolved()?),
            "cpu_args": device_render::cpu_args(&cfg.device_resolved()?),
            "network_args": device_render::network_args(&cfg, &cfg.device_resolved()?)?,
            "ssh_endpoint": [host, port],
        });
        println!("{}", crate::py_json::dumps_pretty_with(&payload, true));
        return Ok(0);
    }
    message(format!("unknown device action {action:?}"))
}

pub fn print_report(report: &Value, group: Option<&str>) {
    println!(
        "device report: {}  status={}  acceptable={}  lab_mode={}",
        report.get("instance").and_then(Value::as_str).unwrap_or(""),
        report.get("status").and_then(Value::as_str).unwrap_or(""),
        report
            .get("acceptable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        report
            .get("lab_mode")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    );
    println!(
        "errors={} warnings={} checks={}  generated={}",
        report.get("errors").and_then(Value::as_u64).unwrap_or(0),
        report.get("warnings").and_then(Value::as_u64).unwrap_or(0),
        report.get("checks").and_then(Value::as_u64).unwrap_or(0),
        report
            .get("generated_at")
            .and_then(crate::util::timestamp_millis)
            .map(crate::util::utcstamp_from_millis)
            .unwrap_or_default()
    );
    if let Some(groups) = report.get("groups").and_then(Value::as_object) {
        for (name, info) in groups {
            if let Some(group) = group
                && name != group
            {
                continue;
            }
            println!();
            println!(
                "[{name}] {}  errors={} warnings={}",
                info.get("status").and_then(Value::as_str).unwrap_or(""),
                info.get("errors").and_then(Value::as_u64).unwrap_or(0),
                info.get("warnings").and_then(Value::as_u64).unwrap_or(0)
            );
            if let Some(findings) = info.get("findings").and_then(Value::as_array) {
                for finding in findings {
                    let severity = finding
                        .get("severity")
                        .and_then(Value::as_str)
                        .unwrap_or("info");
                    let marker = match severity {
                        "error" => "ERROR",
                        "warning" => "WARN ",
                        _ => "INFO ",
                    };
                    println!(
                        "  {marker} {:<28} {}",
                        finding.get("id").and_then(Value::as_str).unwrap_or(""),
                        finding.get("title").and_then(Value::as_str).unwrap_or("")
                    );
                    if severity != "info" {
                        println!(
                            "        {}",
                            finding.get("detail").and_then(Value::as_str).unwrap_or("")
                        );
                        if let Some(remediation) =
                            finding.get("remediation").and_then(Value::as_str)
                            && !remediation.is_empty()
                        {
                            println!("        fix: {remediation}");
                        }
                    }
                }
            }
        }
    }
    if let Some(residuals) = report.get("residuals").and_then(Value::as_array)
        && !residuals.is_empty()
    {
        println!();
        println!("documented residuals:");
        for item in residuals {
            println!(
                "  WARN {:<28} {}",
                item.get("id").and_then(Value::as_str).unwrap_or(""),
                item.get("title").and_then(Value::as_str).unwrap_or("")
            );
        }
    }
}

pub fn run(argv: &[String]) -> Result<i32> {
    let args = parse_args(argv)?;
    if args.has("help") || args.command == "help" {
        print_help();
        return Ok(0);
    }
    match args.command.as_str() {
        "plan" => cmd_plan(&args),
        "apply" => cmd_apply(&args),
        "status" => cmd_status(&args),
        "rollback" => cmd_rollback(&args),
        "start" => cmd_start(&args),
        "stop" => cmd_stop(&args),
        "console" => cmd_console(&args),
        "ssh" => cmd_ssh(&args),
        "logs" => cmd_logs(&args),
        "credentials" => cmd_credentials(&args),
        "reconfigure" => cmd_reconfigure(&args),
        "destroy" => cmd_destroy(&args),
        "show-config" => cmd_show_config(&args),
        "doctor" => cmd_doctor(&args),
        "proxy" => cmd_proxy(&args),
        "bridge-setup" => cmd_bridge_setup(&args),
        "device" => cmd_device(&args),
        other => message(format!(
            "unknown command {other:?}; run 'vmctl --help' (commands: plan, apply, status, rollback, start, stop, console, ssh, logs, credentials, reconfigure, destroy, show-config, doctor, proxy, bridge-setup, device)"
        )),
    }
}

pub fn print_help() {
    println!(
        "vmctl — atomic ARM64 QEMU VM manager for macOS (Ubuntu 24.04/26.04 LTS)"
    );
    println!();
    println!("usage: vmctl [--home DIR] <command> [options]");
    println!();
    println!("commands:");
    println!("  plan              show the step pipeline and readiness");
    println!("  apply             apply pending steps atomically (--start)");
    println!("  status            show step/VM status (--json, --check)");
    println!("  rollback          roll back steps (--last, --step ID, --force)");
    println!("  start / stop      control the VM (QMP + SSH)");
    println!("  console           attach to the guest serial console (--start)");
    println!("  ssh [command]     open SSH or run a guest command");
    println!("  logs              list/tail step or serial logs (--list, --follow)");
    println!("  credentials       print the generated guest login");
    println!("  reconfigure       rename user / change host, timezone, locale (--yes)");
    println!("  destroy           stop and remove an instance (--yes, --all)");
    println!("  show-config       print the effective config and fingerprints");
    println!("  doctor            run read-only environment checks");
    println!("  proxy <action>    embedded shoes proxy: status | check | start | stop");
    println!("  bridge-setup      print the one-time socket_vmnet setup");
    println!("  device <action>  status | verify | report | show");
    println!();
    println!("common options: --name NAME --user USER --hostname HOST --ssh-port PORT");
    println!("  --cpus N --memory MiB --disk-size 20G --network user|socket-vmnet|...");
    println!(
        "  --desktop sway --chrome --gpu none|virtio|ramfb --audio ... --os ubuntu-24.04|ubuntu-26.04"
    );
    println!("  --device / --no-device --set PATH=VALUE --browser ...");
    println!("  --home DIR (or VMCTL_HOME)");
}

pub fn main(argv: &[String]) -> i32 {
    match run(argv) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_args;

    #[test]
    fn previous_device_flags_remain_supported() {
        let args = parse_args(&[
            "apply".into(),
            "--\x73\x74\x65\x61\x6c\x74\x68".into(),
            "--\x73\x74\x65\x61\x6c\x74\x68-timezone=Asia/Tokyo".into(),
        ])
        .unwrap();
        assert!(args.has("device"));
        assert_eq!(args.value("device-timezone"), Some("Asia/Tokyo"));
    }
}
