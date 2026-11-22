//! Raw in-guest device evidence in the JSON format used by the host verifier.
//!
//! `probe()` collects from the current machine with the current credentials; it
//! does not elevate privileges. The caller must run it as root when the full
//! root-visible report is wanted. `probe_with()` accepts a fully supplied
//! environment so tests and other callers can avoid accessing the real system.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use chrono::Utc;
use serde_json::{Map, Value, json};

const TEXT_LIMIT: usize = 200_000;
const BINARY_LIMIT: usize = 131_072;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
const SHORT_TIMEOUT: Duration = Duration::from_secs(8);

const FORBIDDEN: &[&str] = &[
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

// Keep these in script order: commands are run sequentially, not in parallel.
const COMMANDS: &[(&str, &[&str])] = &[
    ("uname", &["uname", "-a"]),
    ("os_release", &["cat", "/etc/os-release"]),
    ("hostname", &["hostname"]),
    ("id", &["id"]),
    ("date", &["date", "-Is"]),
    ("uptime", &["uptime"]),
    ("lscpu", &["lscpu"]),
    ("lscpu_json", &["lscpu", "-J"]),
    ("cpuinfo", &["cat", "/proc/cpuinfo"]),
    ("cmdline", &["cat", "/proc/cmdline"]),
    ("detect_virt", &["systemd-detect-virt"]),
    ("virt_what", &["virt-what"]),
    (
        "dmidecode",
        &["dmidecode", "-q", "-t", "0,1,2,3,4,7,16,17,19,32"],
    ),
    ("lspci", &["lspci", "-nn"]),
    ("lspci_vv", &["lspci", "-nnvv"]),
    (
        "lsblk",
        &[
            "lsblk",
            "-J",
            "-o",
            "NAME,MODEL,SERIAL,SIZE,TRAN,ROTA,TYPE,FSTYPE,MOUNTPOINTS",
        ],
    ),
    ("lsusb", &["lsusb"]),
    ("lsusb_tree", &["lsusb", "-t"]),
    ("aplay", &["aplay", "-l"]),
    ("asound_cards", &["cat", "/proc/asound/cards"]),
    ("ip_addr", &["ip", "-j", "addr"]),
    ("ip_route", &["ip", "-j", "route"]),
    ("ip_neigh", &["ip", "-j", "neigh"]),
    ("ip_link", &["ip", "-j", "link"]),
    ("timedatectl", &["timedatectl", "show"]),
    ("localectl", &["localectl", "status"]),
    ("hostnamectl", &["hostnamectl", "status"]),
    (
        "systemctl_units",
        &[
            "systemctl",
            "list-units",
            "--type=service",
            "--all",
            "--no-pager",
            "--plain",
            "--no-legend",
        ],
    ),
    (
        "systemctl_failed",
        &["systemctl", "--failed", "--no-pager", "--plain"],
    ),
    ("ps", &["ps", "-eo", "pid,comm,args"]),
    ("dmesg", &["dmesg"]),
    (
        "journal_kernel",
        &["journalctl", "-k", "-b", "--no-pager", "-o", "cat"],
    ),
    (
        "journal_cloud",
        &[
            "journalctl",
            "-b",
            "--no-pager",
            "-o",
            "cat",
            "--grep",
            "cloud-init|qemu-ga|qemu-guest|virtio",
        ],
    ),
    (
        "dpkg",
        &[
            "dpkg-query",
            "-W",
            "-f=${Package}\\t${Status}\\t${Version}\\n",
        ],
    ),
    ("lsmod", &["lsmod"]),
    ("efibootmgr", &["efibootmgr", "-v"]),
    ("resolvectl", &["resolvectl", "status"]),
    ("networkctl", &["networkctl", "status", "--no-pager"]),
    ("sshd_T", &["sshd", "-T"]),
    ("locale", &["locale"]),
    (
        "findmnt_root",
        &["findmnt", "-no", "SOURCE,FSTYPE,OPTIONS", "/"],
    ),
    (
        "findmnt_boot",
        &["findmnt", "-no", "SOURCE,FSTYPE,OPTIONS", "/boot"],
    ),
    (
        "findmnt_efi",
        &["findmnt", "-no", "SOURCE,FSTYPE,OPTIONS", "/boot/efi"],
    ),
    ("blkid", &["blkid"]),
    ("nproc", &["nproc"]),
    ("getconf_pagesize", &["getconf", "PAGESIZE"]),
    // The Python script uses a shell solely for `2>/dev/null || true`. Apply
    // those two operations below, while invoking sysctl directly as argv.
    (
        "sysctl_random",
        &["sysctl", "kernel.random.boot_id", "kernel.random.uuid"],
    ),
];

const FILES: &[&str] = &[
    "/etc/hostname",
    "/etc/hosts",
    "/etc/timezone",
    "/etc/adjtime",
    "/etc/localtime",
    "/etc/default/locale",
    "/etc/locale.gen",
    "/etc/default/keyboard",
    "/etc/machine-id",
    "/var/lib/dbus/machine-id",
    "/etc/resolv.conf",
    "/etc/netplan/50-vmctl.yaml",
    "/etc/netplan/50-cloud-init.yaml",
    "/etc/ssh/sshd_config",
    "/etc/ssh/sshd_config.d/99-vmctl.conf",
    "/etc/sysctl.d/99-vmctl-hardening.conf",
    "/proc/net/arp",
    "/sys/firmware/devicetree/base/compatible",
    "/sys/firmware/efi",
    "/etc/apt/apt.conf.d/90vmctl-proxy",
    "/etc/environment.d/90-vmctl-proxy.conf",
    "/etc/vmctl-instance",
    "/etc/vmctl-proxy",
    "/var/log/cloud-init.log",
    "/var/log/cloud-init-output.log",
    "/var/log/syslog",
    "/var/log/auth.log",
    "/var/log/kern.log",
    "/var/log/apt/history.log",
    "/var/log/dpkg.log",
    "/usr/share/misc/pci.ids",
    "/usr/share/hwdata/pci.ids",
];
const DMI_NAMES: &[&str] = &[
    "sys_vendor",
    "product_name",
    "product_version",
    "product_serial",
    "product_uuid",
    "board_vendor",
    "board_name",
    "board_version",
    "board_serial",
    "board_asset_tag",
    "chassis_vendor",
    "chassis_type",
    "chassis_serial",
    "chassis_asset_tag",
    "bios_vendor",
    "bios_version",
    "bios_date",
    "bios_release",
];
const PATHS: &[&str] = &[
    "/etc/cloud",
    "/var/lib/cloud",
    "/etc/vmctl-instance",
    "/etc/vmctl-proxy",
    "/var/log/cloud-init.log",
    "/var/log/cloud-init-output.log",
    "/etc/netplan/50-cloud-init.yaml",
    "/etc/ssh/sshd_config.d/50-cloud-init.conf",
    "/etc/sudoers.d/90-cloud-init-users",
    "/etc/sudoers.d/90-hidden-users",
    "/var/lib/qemu-guest-agent",
    "/dev/kvm",
    "/dev/virtio-ports",
    "/sys/firmware/qemu_fw_cfg",
    "/sys/bus/acpi/devices/QEMU0002:00",
    "/sys/firmware/devicetree/base",
    "/sys/firmware/efi",
    "/usr/share/misc/pci.ids",
    "/usr/share/hwdata/pci.ids",
];
const LOGS: &[&str] = &[
    "/var/log/syslog",
    "/var/log/auth.log",
    "/var/log/kern.log",
    "/var/log/dpkg.log",
    "/var/log/apt/history.log",
];

/// Result of one command, matching the `commands` entries in the probe report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandResult {
    pub rc: i32,
    pub out: String,
    pub err: String,
}

impl CommandResult {
    fn json(&self) -> Value {
        json!({"rc": self.rc, "out": self.out, "err": self.err})
    }
}

/// One account from the system's password database (including NSS sources).
#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
    pub gecos: String,
}

/// All inputs used by the collector. Implementations of `run` and
/// `read_text` must apply their character limits, and `read_bytes` its byte
/// limit. `glob` returns full, guest-absolute paths and skips dotfiles, as
/// Python's glob does. Errors from individual files and commands are encoded
/// in their returned values rather than aborting the report.
pub trait ProbeEnvironment {
    fn run(&self, argv: &[&str], timeout: Duration, limit: usize) -> CommandResult;
    fn read_text(&self, path: &str, limit: usize) -> Value;
    fn read_bytes(&self, path: &str, limit: usize) -> Vec<u8>;
    fn glob(&self, directory: &str, prefix: &str, suffix: &str) -> Vec<String>;
    fn exists(&self, path: &str) -> bool;
    fn users(&self) -> Result<Vec<User>, String>;
    fn collected_at(&self) -> String;
}

/// Collect from the running guest using the caller's current credentials.
pub fn probe() -> Value {
    probe_with(&RealEnvironment)
}

/// Collect the same report using injected filesystem, command and clock inputs.
pub fn probe_with(env: &impl ProbeEnvironment) -> Value {
    let mut commands = Map::new();
    for &(name, argv) in COMMANDS {
        let mut result = env.run(argv, COMMAND_TIMEOUT, TEXT_LIMIT);
        if name == "sysctl_random" && !(result.rc == 124 && result.err == "timeout") {
            // The old shell command ignores sysctl's exit status and stderr,
            // including an exec failure. A timeout is the only exception.
            result.rc = 0;
            result.err.clear();
        }
        commands.insert(name.into(), result.json());
    }

    let mut files = Map::new();
    for &path in FILES {
        files.insert(path.into(), env.read_text(path, TEXT_LIMIT));
    }
    let mut dmi = Map::new();
    for &name in DMI_NAMES {
        let path = format!("/sys/class/dmi/id/{name}");
        let info = env.read_text(&path, TEXT_LIMIT);
        if info.get("exists") == Some(&Value::Bool(true)) {
            dmi.insert(
                name.into(),
                json!(info["text"].as_str().unwrap_or("").trim()),
            );
        }
        files.insert(path, info);
    }

    let mut acpi = Map::new();
    let mut tables = env.glob("/sys/firmware/acpi/tables", "", "");
    tables.sort();
    for path in tables {
        let data = env.read_bytes(&path, BINARY_LIMIT);
        if data.is_empty() {
            continue;
        }
        let field = |start: usize, end: usize| {
            let slice = data.get(start..end.min(data.len())).unwrap_or(&[]);
            ascii_replace(slice).trim_matches(['\0', ' ']).to_owned()
        };
        let strings: Vec<_> = strings_in(&data, 4, 256)
            .into_iter()
            .filter(|s| s.chars().any(char::is_alphabetic))
            .take(256)
            .collect();
        let name = path.rsplit('/').next().unwrap_or("");
        acpi.insert(
            name.into(),
            json!({
                "size": data.len(), "oem_id": field(10, 16),
                "oem_table_id": field(16, 24), "creator_id": field(28, 32),
                "strings": strings,
            }),
        );
    }
    let mut dmi_strings = strings_in(
        &env.read_bytes("/sys/firmware/dmi/tables/DMI", BINARY_LIMIT),
        4,
        4000,
    );
    for info in acpi.values() {
        if let Some(strings) = info["strings"].as_array() {
            dmi_strings.extend(strings.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    let acpi_hits = scan_forbidden(dmi_strings.iter().map(String::as_str));

    let command_out = |key: &str| commands[key]["out"].as_str().unwrap_or("");
    let parse_ip = |key: &str| {
        serde_json::from_str::<Value>(if command_out(key).is_empty() {
            "[]"
        } else {
            command_out(key)
        })
        .unwrap_or_else(|_| json!([]))
    };
    let interfaces = parse_ip("ip_link");
    let addresses = parse_ip("ip_addr");
    let routes = parse_ip("ip_route");
    let neighbors = parse_ip("ip_neigh");
    let mut ethtool = Map::new();
    if let Some(interfaces) = interfaces.as_array() {
        for iface in interfaces {
            if let Some(name) = iface
                .get("ifname")
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty() && *n != "lo")
            {
                ethtool.insert(
                    name.into(),
                    env.run(&["ethtool", "-i", name], SHORT_TIMEOUT, TEXT_LIMIT)
                        .json(),
                );
            }
        }
    }
    let net = json!({"interfaces": interfaces, "addresses": addresses, "routes": routes, "neighbors": neighbors, "ethtool": ethtool});

    let mut leases = Map::new();
    for (dir, suffix, limit) in [
        ("/var/lib/systemd/network", ".lease", 50_000),
        ("/run/systemd/netif/leases", "", 50_000),
        ("/var/lib/dhcp", ".leases", 100_000),
    ] {
        for path in env.glob(dir, "", suffix) {
            leases.insert(path.clone(), env.read_text(&path, limit));
        }
    }
    let mut ssh_keys = Map::new();
    let mut keys = env.glob("/etc/ssh", "ssh_host_", "_key");
    keys.sort();
    for path in keys {
        let mut info = env.read_text(&path, 0);
        let fingerprint = env
            .run(&["ssh-keygen", "-lf", &path], SHORT_TIMEOUT, TEXT_LIMIT)
            .json();
        if let Some(object) = info.as_object_mut() {
            object.insert("fingerprint".into(), fingerprint);
        }
        ssh_keys.insert(path, info);
    }
    let mut users = Map::new();
    match env.users() {
        Ok(entries) => {
            for entry in entries {
                if entry.uid == 0 || entry.uid >= 1000 {
                    users.insert(entry.name, json!({"uid": entry.uid, "gid": entry.gid, "home": entry.home, "shell": entry.shell, "gecos": entry.gecos}));
                }
            }
        }
        Err(error) => {
            users.insert("_error".into(), json!(error));
        }
    }
    let mut paths = Map::new();
    for &path in PATHS {
        paths.insert(path.into(), json!(env.exists(path)));
    }
    let mut sudoers = Map::new();
    for path in env.glob("/etc/sudoers.d", "", "") {
        sudoers.insert(path.clone(), env.read_text(&path, 20_000));
    }
    let mut logs = Map::new();
    let mut log_hits = Vec::new();
    for &path in LOGS {
        let info = &files[path];
        if info.get("exists") == Some(&Value::Bool(true)) {
            let hits = scan_forbidden(splitlines(info["text"].as_str().unwrap_or("")));
            for hit in &hits {
                log_hits.push(json!({"path": path, "needle": hit["needle"], "text": hit["text"]}));
            }
            logs.insert(path.into(), json!(hits));
        }
    }
    let pci: Vec<_> = splitlines(command_out("lspci"))
        .filter(|line| !line.trim().is_empty())
        .collect();
    json!({
        "collected_at": env.collected_at(), "commands": commands, "files": files,
        "dmi": dmi, "acpi": acpi, "acpi_hits": acpi_hits,
        "dmi_strings_sample": dmi_strings.into_iter().take(800).collect::<Vec<_>>(),
        "pci": pci, "net": net, "leases": leases, "ssh_keys": ssh_keys,
        "users": users, "paths": paths, "sudoers": sudoers, "logs": logs,
        "log_hits": log_hits,
    })
}

fn ascii_replace(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| if b.is_ascii() { b as char } else { '\u{fffd}' })
        .collect()
}

fn strings_in(data: &[u8], minimum: usize, cap: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    for bytes in data.split(|b| !(b' '..=b'~').contains(b)) {
        if bytes.len() >= minimum {
            let text = String::from_utf8_lossy(bytes).into_owned();
            if seen.insert(text.clone()) {
                values.push(text);
                if values.len() >= cap {
                    break;
                }
            }
        }
    }
    values
}

fn scan_forbidden<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<Value> {
    let values: Vec<_> = values.into_iter().map(|s| (s, s.to_lowercase())).collect();
    FORBIDDEN
        .iter()
        .filter_map(|needle| {
            let lower = needle.to_lowercase();
            values.iter().find(|(_, text)| text.contains(&lower)).map(|(original, _)| {
            json!({"needle": needle, "text": original.chars().take(400).collect::<String>()})
        })
        })
        .collect()
}

// str.splitlines(), including Unicode separators, omits a trailing empty line.
fn splitlines(text: &str) -> impl Iterator<Item = &str> {
    text.split_terminator([
        '\n', '\r', '\u{000b}', '\u{000c}', '\u{001c}', '\u{001d}', '\u{001e}', '\u{0085}',
        '\u{2028}', '\u{2029}',
    ])
}

/// The real guest environment. No implicit sudo or writes are performed.
pub struct RealEnvironment;

impl ProbeEnvironment for RealEnvironment {
    fn run(&self, argv: &[&str], timeout: Duration, limit: usize) -> CommandResult {
        run_command(argv, timeout, limit)
    }
    fn read_text(&self, path: &str, limit: usize) -> Value {
        read_text(Path::new(path), path, limit)
    }
    fn read_bytes(&self, path: &str, limit: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        if let Ok(file) = File::open(path) {
            let _ = file.take(limit as u64).read_to_end(&mut bytes);
        }
        bytes
    }
    fn glob(&self, directory: &str, prefix: &str, suffix: &str) -> Vec<String> {
        glob_directory(Path::new(directory), directory, prefix, suffix)
    }
    fn exists(&self, path: &str) -> bool {
        Path::new(path).exists()
    }
    fn users(&self) -> Result<Vec<User>, String> {
        system_users()
    }
    fn collected_at(&self) -> String {
        Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }
}

fn python_error(error: &std::io::Error, path: &str) -> String {
    if let Some(code) = error.raw_os_error() {
        let message = error.to_string();
        let message = message.split(" (os error ").next().unwrap_or(&message);
        // Python repr normally uses single quotes, switching to double quotes
        // when that avoids escaping an embedded apostrophe.
        let path = if path.contains('\'') && !path.contains('"') {
            format!("\"{}\"", path.replace('\\', "\\\\"))
        } else {
            format!("'{}'", path.replace('\\', "\\\\").replace('\'', "\\'"))
        };
        format!("[Errno {code}] {message}: {path}")
    } else {
        error.to_string()
    }
}

fn normalize_text(bytes: &[u8], limit: usize) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while result.chars().count() < limit {
        let Some(ch) = chars.next() else { break };
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            result.push('\n');
        } else {
            result.push(ch);
        }
    }
    result
}

fn read_text(path: &Path, display: &str, limit: usize) -> Value {
    let read = || -> std::io::Result<Value> {
        let metadata = fs::metadata(path)?;
        let file = File::open(path)?;
        // Four bytes per UTF-8 scalar (and CRLF uses at most two) suffice to
        // produce `limit` normalized characters, plus boundary lookahead.
        let mut bytes = Vec::new();
        file.take((limit.saturating_mul(4).saturating_add(4)) as u64)
            .read_to_end(&mut bytes)?;
        Ok(
            json!({"exists": true, "text": normalize_text(&bytes, limit),
            "size": metadata.size(), "mode": format!("0o{:o}", metadata.mode() & 0o7777),
            "mtime": metadata.mtime() as f64 + metadata.mtime_nsec() as f64 / 1_000_000_000.0}),
        )
    };
    read().unwrap_or_else(|error| json!({"exists": false, "error": python_error(&error, display)}))
}

fn glob_directory(path: &Path, display: &str, prefix: &str, suffix: &str) -> Vec<String> {
    let Ok(entries) = fs::read_dir(path) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with('.') && name.starts_with(prefix) && name.ends_with(suffix) {
                Some(format!("{}/{name}", display.trim_end_matches('/')))
            } else {
                None
            }
        })
        .collect()
}

fn run_command(argv: &[&str], timeout: Duration, limit: usize) -> CommandResult {
    let result = |rc, out: String, err: String| CommandResult { rc, out, err };
    let Some((program, args)) = argv.split_first() else {
        return result(125, String::new(), "empty command".into());
    };
    let child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return result(127, String::new(), "not found".into());
        }
        Err(error) => return result(125, String::new(), python_error(&error, program)),
    };
    // Drain both pipes concurrently, retaining a bounded prefix. This preserves
    // the script's post-decode character limit without blocking full pipes.
    let capture = |mut pipe: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut kept = Vec::new();
            let max = limit.saturating_mul(4).saturating_add(4);
            let mut buf = [0; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let take = n.min(max.saturating_sub(kept.len()));
                kept.extend_from_slice(&buf[..take]);
            }
            normalize_text(&kept, limit)
        })
    };
    let out = capture(Box::new(child.stdout.take().expect("piped stdout")));
    let err = capture(Box::new(child.stderr.take().expect("piped stderr")));
    let started = Instant::now();
    let status: std::process::ExitStatus = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                // Like subprocess.run, drain pipes after killing the child.
                let _ = out.join();
                let _ = err.join();
                return result(124, String::new(), "timeout".into());
            }
            Ok(None) => std::thread::sleep(
                Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
            ),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = out.join();
                let _ = err.join();
                return result(125, String::new(), error.to_string());
            }
        }
    };
    result(
        status.code().unwrap_or(0),
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    )
}

// libc's passwd enumeration uses process-global state; serialize this module's
// own enumerations. The guest uses NSS just like Python's pwd.getpwall().
fn system_users() -> Result<Vec<User>, String> {
    use std::ffi::CStr;
    use std::sync::Mutex;
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().map_err(|error| error.to_string())?;
    let mut users = Vec::new();
    unsafe {
        libc::setpwent();
        loop {
            let entry = libc::getpwent();
            if entry.is_null() {
                break;
            }
            let entry = &*entry;
            let text = |ptr: *const libc::c_char| {
                if ptr.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(ptr).to_string_lossy().into_owned()
                }
            };
            users.push(User {
                name: text(entry.pw_name),
                uid: entry.pw_uid,
                gid: entry.pw_gid,
                home: text(entry.pw_dir),
                shell: text(entry.pw_shell),
                gecos: text(entry.pw_gecos),
            });
        }
        libc::endpwent();
    }
    Ok(users)
}
