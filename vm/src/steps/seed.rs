//! Cloud-init seed step: users, hostname, SSH key, apt proxy, network config.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::Result;
use crate::device::guest as device_guest;
use crate::device::render as device_render;
use crate::journal::load_state;
use crate::steps::base::{CheckResult, Step, StepContext, delete_artifacts};
use crate::util::{
    atomic_write_json, atomic_write_text, ensure_dir, message, now_millis, read_json,
    read_text, remove_path, sha256_file,
};

/// Installed on every new guest: the Node.js runtime library and the full
/// kernel metapackage that replaces the minimal image's (`os`).
fn guest_runtime_packages(cfg: &crate::config::Config) -> Vec<String> {
    vec![
        "libatomic1".to_string(),
        cfg.guest_os().kernel().install.to_string(),
    ]
}

const SEED_FILES: [&str; 7] = [
    "seed.iso",
    "user-data",
    "meta-data",
    "network-config",
    "id_ed25519",
    "id_ed25519.pub",
    "credentials.json",
];

pub fn generate_password() -> String {
    crate::util::random_password(20)
}

pub fn hash_password(password: &str) -> Result<String> {
    use sha_crypt::{PasswordHasher, ShaCrypt};
    ShaCrypt::SHA512
        .hash_password(password.as_bytes())
        .map(|hash| hash.to_string())
        .map_err(|error| crate::Error::Message(format!("cannot hash the password: {error}")))
}

fn proxy_no_proxy(cfg: &crate::config::Config) -> String {
    [
        "localhost",
        "127.0.0.1",
        "::1",
        "10.0.2.2",
        cfg.hostname.as_str(),
        &format!("{}.local", cfg.hostname),
    ]
    .join(",")
}

/// Gateway mode: keep the clock with the gateway's NTP server (the Mac's
/// clock). DHCP announces it too; the drop-in makes it the only source.
fn add_time_sync(doc: &mut Map<String, Value>) {
    let ntp = crate::netgw::GATEWAY_IP;
    let mut write_files = doc
        .get("write_files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    write_files.push(json!({
        "path": "/etc/systemd/timesyncd.conf.d/cabin.conf",
        "permissions": "0644",
        "content": format!("[Time]\nNTP={ntp}\nFallbackNTP=\nPollIntervalMaxSec=1024\n"),
    }));
    doc.insert("write_files".to_string(), Value::Array(write_files));
    let mut runcmd = doc
        .get("runcmd")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    runcmd.insert(0, json!(["timedatectl", "set-ntp", "true"]));
    runcmd.insert(1, json!(["systemctl", "restart", "systemd-timesyncd"]));
    doc.insert("runcmd".to_string(), Value::Array(runcmd));
}

fn add_kernel_switch(cfg: &crate::config::Config, doc: &mut Map<String, Value>) {
    let kernel = cfg.guest_os().kernel();
    let mut runcmd = doc
        .get("runcmd")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    runcmd.push(json!(["sh", "-c", kernel.script()]));
    doc.insert("runcmd".to_string(), Value::Array(runcmd));
}

fn write_file(path: &str, content: String) -> Value {
    json!({
        "path": path,
        "permissions": "0644",
        "content": content,
    })
}

/// Replace the image's package sources in cloud-init's early write-files
/// phase. The apt module's `primary` setting applies later; this keeps boot
/// services and the first package update from touching the image's default
/// archive in between.
fn apt_source_files(os: &crate::os::Os, mirror: Option<&str>) -> Vec<Value> {
    let Some(mirror) = mirror.map(str::trim).filter(|mirror| !mirror.is_empty()) else {
        return Vec::new();
    };
    let mirror = mirror.trim_end_matches('/');
    let disabled = "# Package sources are managed by cabin.\n".to_string();
    let suites = format!("{} {}-updates", os.codename, os.codename);
    let security = format!("{}-security", os.codename);
    vec![
        write_file("/etc/apt/sources.list", disabled),
        write_file(
            "/etc/apt/sources.list.d/ubuntu.sources",
            format!(
                "Types: deb\nURIs: {mirror}\nSuites: {suites}\nComponents: main restricted universe multiverse\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n\n\
                 Types: deb\nURIs: {mirror}\nSuites: {security}\nComponents: main restricted universe multiverse\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
            ),
        ),
    ]
}

fn add_apt_sources(cfg: &crate::config::Config, doc: &mut Map<String, Value>) {
    let sources = apt_source_files(cfg.guest_os(), cfg.apt_mirror.as_deref());
    if sources.is_empty() {
        return;
    }
    let mut write_files = doc
        .get("write_files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    write_files.extend(sources);
    doc.insert("write_files".to_string(), Value::Array(write_files));
}

fn add_ipv4_policy(doc: &mut Map<String, Value>) {
    let mut files = doc.get("write_files").and_then(Value::as_array).cloned().unwrap_or_default();
    files.push(json!({
        "path": "/etc/sysctl.d/zz-cabin-ipv4.conf",
        "permissions": "0644",
        "content": "# Managed by cabin: guest network interfaces are IPv4-only.\nnet.ipv6.conf.default.disable_ipv6 = 1\nnet.ipv6.conf.all.disable_ipv6 = 1\nnet.ipv6.conf.lo.disable_ipv6 = 0\n"
    }));
    doc.insert("write_files".into(), Value::Array(files));
}

pub fn build_user_data(
    cfg: &crate::config::Config,
    password_hash: &str,
    ssh_pubkey: &str,
) -> Result<Value> {
    let guest_proxy = cfg.guest_proxy_url();
    let resolved = if cfg.device_enabled() {
        Some(cfg.device_resolved()?)
    } else {
        None
    };
    let mut doc = Map::new();
    doc.insert("hostname".to_string(), json!(cfg.hostname));
    doc.insert("manage_etc_hosts".to_string(), json!(true));
    doc.insert("preserve_hostname".to_string(), json!(false));
    doc.insert(
        "timezone".to_string(),
        json!(if let Some(resolved) = &resolved {
            resolved
                .get("guest")
                .and_then(|value| value.get("timezone"))
                .and_then(Value::as_str)
                .unwrap_or("Etc/UTC")
        } else {
            "Etc/UTC"
        }),
    );
    doc.insert(
        "users".to_string(),
        json!([{
            "name": cfg.username,
            "gecos": cfg.username,
            "groups": ["adm", "sudo"],
            "shell": "/bin/bash",
            "sudo": "ALL=(ALL) NOPASSWD:ALL",
            "lock_passwd": false,
            "passwd": password_hash,
            "ssh_authorized_keys": [ssh_pubkey],
        }]),
    );
    doc.insert("ssh_pwauth".to_string(), json!(true));
    doc.insert("disable_root".to_string(), json!(true));
    doc.insert("chpasswd".to_string(), json!({"expire": false}));
    doc.insert("package_update".to_string(), json!(true));
    doc.insert(
        "growpart".to_string(),
        json!({"mode": "auto", "devices": ["/"]}),
    );
    doc.insert("resize_rootfs".to_string(), json!(true));
    doc.insert(
        "final_message".to_string(),
        json!(if resolved.is_some() {
            "cloud-init finished"
        } else {
            "vmctl cloud-init finished"
        }),
    );
    if let Some(resolved) = &resolved {
        let guest = resolved.get("guest").unwrap_or(&Value::Null);
        doc.insert(
            "locale".to_string(),
            guest
                .get("locale")
                .cloned()
                .unwrap_or_else(|| json!("en_US.UTF-8")),
        );
        doc.insert(
            "keyboard".to_string(),
            json!({
                "layout": guest.get("keyboard_layout").and_then(Value::as_str).unwrap_or("us"),
                "variant": guest.get("keyboard_variant").and_then(Value::as_str).unwrap_or(""),
            }),
        );
        let mut packages = device_guest::cloud_init_packages(resolved);
        for package in guest_runtime_packages(cfg) {
            if !packages.contains(&package) {
                packages.push(package);
            }
        }
        doc.insert("packages".to_string(), json!(packages));
        doc.insert(
            "write_files".to_string(),
            Value::Array(device_guest::cloud_init_write_files(cfg, resolved)?),
        );
        doc.insert(
            "runcmd".to_string(),
            Value::Array(device_guest::cloud_init_runcmd(cfg, resolved)),
        );
        doc.insert(
            "apt".to_string(),
            apt_settings(
                guest_proxy.as_deref(),
                cfg.apt_mirror.as_deref(),
            ),
        );
        if let Some(proxy) = &guest_proxy {
            let no_proxy = proxy_no_proxy(cfg);
            let write_files = doc
                .get("write_files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut write_files = write_files;
            write_files.push(json!({
                "path": "/etc/apt/apt.conf.d/90proxy",
                "permissions": "0644",
                "content": format!("Acquire::http::Proxy \"{proxy}\";\nAcquire::https::Proxy \"{proxy}\";\n"),
            }));
            write_files.push(json!({
                "path": "/etc/environment.d/90proxy.conf",
                "permissions": "0644",
                "content": format!(
                    "http_proxy=\"{proxy}\"\nhttps_proxy=\"{proxy}\"\nHTTP_PROXY=\"{proxy}\"\nHTTPS_PROXY=\"{proxy}\"\nno_proxy=\"{no_proxy}\"\nNO_PROXY=\"{no_proxy}\"\n"
                ),
            }));
            doc.insert("write_files".to_string(), Value::Array(write_files));
        }
        add_apt_sources(cfg, &mut doc);
        if cfg.isolated() {
            add_ipv4_policy(&mut doc);
            add_time_sync(&mut doc);
        }
        add_kernel_switch(cfg, &mut doc);
        return Ok(Value::Object(doc));
    }

    doc.insert(
        "packages".to_string(),
        json!(
            ["qemu-guest-agent".to_string(), "iproute2".to_string()]
                .into_iter()
                .chain(guest_runtime_packages(cfg))
                .collect::<Vec<_>>()
        ),
    );
    doc.insert(
        "runcmd".to_string(),
        json!([
            ["systemctl", "enable", "--now", "qemu-guest-agent"],
            [
                "sh",
                "-c",
                "ip -o -4 addr show > /var/log/vmctl-network.txt 2>&1 || true"
            ],
            [
                "sh",
                "-c",
                "cloud-init status --long > /var/log/vmctl-cloud-init-status.txt 2>&1 || true"
            ]
        ]),
    );
    let mut write_files = Vec::new();
    doc.insert(
        "apt".to_string(),
        apt_settings(
            guest_proxy.as_deref(),
            cfg.apt_mirror.as_deref(),
        ),
    );
    if let Some(proxy) = &guest_proxy {
        let no_proxy = proxy_no_proxy(cfg);
        write_files.push(json!({
            "path": "/etc/apt/apt.conf.d/90vmctl-proxy",
            "permissions": "0644",
            "content": format!("Acquire::http::Proxy \"{proxy}\";\nAcquire::https::Proxy \"{proxy}\";\n"),
        }));
        write_files.push(json!({
            "path": "/etc/environment.d/90-vmctl-proxy.conf",
            "permissions": "0644",
            "content": format!(
                "http_proxy=\"{proxy}\"\nhttps_proxy=\"{proxy}\"\nHTTP_PROXY=\"{proxy}\"\nHTTPS_PROXY=\"{proxy}\"\nno_proxy=\"{no_proxy}\"\nNO_PROXY=\"{no_proxy}\"\n"
            ),
        }));
        write_files.push(json!({
            "path": "/etc/vmctl-proxy",
            "permissions": "0644",
            "content": format!("{proxy}\n"),
        }));
    }
    write_files.push(json!({
        "path": "/etc/vmctl-instance",
        "permissions": "0644",
        "content": format!(
            "name={}\nhostname={}\nuser={}\nnetwork_mode={}\n",
            cfg.name, cfg.hostname, cfg.username, cfg.network_mode
        ),
    }));
    doc.insert("write_files".to_string(), Value::Array(write_files));
    add_apt_sources(cfg, &mut doc);
    if cfg.isolated() {
        add_ipv4_policy(&mut doc);
        add_time_sync(&mut doc);
    }
    add_kernel_switch(cfg, &mut doc);
    Ok(Value::Object(doc))
}

pub fn build_network_config(cfg: &crate::config::Config) -> Result<Value> {
    let mut ethernets = Map::new();
    // Isolation wins over the device profile's bridged network: the VM must
    // never reach the LAN.
    if cfg.isolated() {
        // One NIC, the gateway: internet, SSH from the Mac and NTP. DNS and
        // the NTP server come from its DHCP.
        ethernets.insert(
            "data0".to_string(),
            json!({
                "match": {"macaddress": cfg.mac_data()},
                "set-name": "data0",
                "dhcp4": true,
                "dhcp6": false,
                "accept-ra": false,
                "link-local": ["ipv4"],
            }),
        );
        return Ok(json!({"version": 2, "ethernets": Value::Object(ethernets)}));
    }
    if cfg.device_enabled() {
        return device_render::network_plan(&cfg.device_resolved()?, Some(&cfg.hostname));
    }
    ethernets.insert(
        "mgmt0".to_string(),
        json!({
            "match": {"macaddress": cfg.mac_mgmt()},
            "set-name": "mgmt0",
            "dhcp4": true,
            "dhcp4-overrides": {"route-metric": 100},
            "optional": true,
        }),
    );
    if cfg.needs_second_nic() {
        ethernets.insert(
            "data0".to_string(),
            json!({
                "match": {"macaddress": cfg.mac_data()},
                "set-name": "data0",
                "dhcp4": true,
                "dhcp4-overrides": {"route-metric": 200},
                "optional": true,
            }),
        );
    }
    Ok(json!({"version": 2, "ethernets": Value::Object(ethernets)}))
}

/// cloud-init `apt` settings. Retries and generous timeouts make a transient
/// DNS or network hiccup during first boot retry instead of failing the
/// install (seen on slow networks and under emulation).
fn apt_settings(proxy: Option<&str>, mirror: Option<&str>) -> Value {
    let mut apt = json!({
        "conf": "Acquire::Retries \"10\";\nAcquire::http::Timeout \"60\";\nAcquire::https::Timeout \"60\";\n",
    });
    if let Some(proxy) = proxy {
        apt["proxy"] = json!(proxy);
    }
    if let Some(mirror) = mirror {
        // cloud-init rewrites the deb822 sources; Ubuntu mirrors carry the
        // security pocket too.
        let source = json!([{ "arches": ["default"], "uri": mirror }]);
        apt["primary"] = source.clone();
        apt["security"] = source;
    }
    apt
}

/// Read the written image back and compare its label and payload.
fn verify_seed_iso(iso: &Path, expected: &[(&str, &Path)]) -> Result<()> {
    let (label, files) = crate::iso::read_iso(iso)?;
    if !label.eq_ignore_ascii_case("cidata") {
        return message(format!("seed ISO volume label is {label:?}, expected CIDATA"));
    }
    for (name, source) in expected {
        let Some((_, data)) = files.iter().find(|(found, _)| found == name) else {
            return message(format!("seed ISO is missing {name}"));
        };
        if crate::util::sha256_bytes(data) != sha256_file(source)? {
            return message(format!("seed ISO file hash mismatch: {name}"));
        }
    }
    Ok(())
}

pub struct SeedStep;

impl Step for SeedStep {
    fn id(&self) -> &str {
        "seed"
    }
    fn title(&self) -> &str {
        "Generate cloud-init seed (user, hostname, SSH key, proxy, dual NIC)"
    }

    fn check(&self, ctx: &mut StepContext) -> Result<CheckResult> {
        let cfg = ctx.cfg.clone();
        let state = self.state(&cfg).unwrap_or(Value::Null);
        let meta = state.get("meta").cloned().unwrap_or(Value::Null);
        if state.get("status").and_then(Value::as_str) != Some("applied") {
            return Ok(CheckResult::pending(
                "seed step has not been applied successfully",
            ));
        }
        if meta.get("seed_fingerprint").and_then(Value::as_str)
            != Some(cfg.seed_fingerprint().as_str())
        {
            return Ok(CheckResult::pending(
                "seed configuration changed (username/hostname/proxy/network)",
            ));
        }
        if !cfg.instance_meta_path().exists() {
            return Ok(CheckResult::pending("instance.json is missing"));
        }
        let instance_meta = read_json(&cfg.instance_meta_path()).unwrap_or(Value::Null);
        if instance_meta
            .get("fingerprints")
            .and_then(|value| value.get("seed"))
            .and_then(Value::as_str)
            != Some(cfg.seed_fingerprint().as_str())
        {
            return Ok(CheckResult::pending(
                "instance metadata seed fingerprint differs",
            ));
        }
        for path in [
            cfg.seed_path(),
            cfg.user_data_path(),
            cfg.meta_data_path(),
            cfg.network_config_path(),
            cfg.ssh_key_path(),
            cfg.ssh_pub_path(),
            cfg.credentials_path(),
        ] {
            if !path.exists() {
                return Ok(CheckResult::pending(format!(
                    "missing {}",
                    path.file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("file")
                )));
            }
        }
        for (label, path, expected) in [
            ("seed.iso", cfg.seed_path(), meta.get("seed_sha256")),
            (
                "user-data",
                cfg.user_data_path(),
                meta.get("user_data_sha256"),
            ),
            (
                "network-config",
                cfg.network_config_path(),
                meta.get("network_config_sha256"),
            ),
            (
                "id_ed25519.pub",
                cfg.ssh_pub_path(),
                meta.get("ssh_pub_sha256"),
            ),
        ] {
            if let Some(expected) = expected.and_then(Value::as_str)
                && sha256_file(&path)? != expected
            {
                return Ok(CheckResult::pending(format!("{label} changed on disk")));
            }
        }
        let credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
        let username = credentials
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or("");
        if username != cfg.username && username != cfg.current_username() {
            return Ok(CheckResult::pending("credentials.json username differs"));
        }
        let fingerprint = meta
            .get("seed_fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("");
        Ok(CheckResult::ready(format!(
            "seed ok ({}), user={}, host={}",
            &fingerprint[..fingerprint.len().min(12)],
            cfg.username,
            cfg.hostname
        )))
    }

    fn apply(&self, ctx: &mut StepContext) -> Result<()> {
        let cfg = ctx.cfg.clone();
        let provision = load_state(&cfg.home, &cfg.name, "provision").unwrap_or(Value::Null);
        if provision.get("status").and_then(Value::as_str) == Some("applied") {
            return message(
                "provision is already applied and the seed is immutable; use 'vmctl reconfigure' for guest identity changes",
            );
        }
        if !cfg.disk_path().exists() {
            return message(
                "disk is not ready; run 'vmctl apply' so the disk step completes first",
            );
        }
        cfg.save_instance_meta()?;
        ensure_dir(&cfg.instance_dir())?;
        ensure_dir(&cfg.tmp_dir())?;
        let existing_credentials = read_json(&cfg.credentials_path()).unwrap_or(Value::Null);
        let mut password = cfg.password.clone();
        if password.is_none()
            && let Some(existing) = existing_credentials.get("password").and_then(Value::as_str)
        {
            password = Some(existing.to_string());
        }
        let mut removed = Vec::new();
        for name in SEED_FILES {
            let path = cfg.instance_file(name);
            if path.exists() {
                removed.push(name.to_string());
                remove_path(&path)?;
            }
        }
        if !removed.is_empty() {
            ctx.log(&format!(
                "removed previous seed files: {}",
                removed.join(", ")
            ));
        }
        let password = match password {
            Some(password) => password,
            None => {
                ctx.log("generated a random guest password");
                generate_password()
            }
        };
        let password_hash = hash_password(&password)?;
        let stage = cfg
            .tmp_dir()
            .join(format!("seed-{}-{}", cfg.name, std::process::id()));
        let _ = std::fs::remove_dir_all(&stage);
        ensure_dir(&stage)?;
        let result = (|| -> Result<(Map<String, Value>, Value)> {
            ctx.log("generating ed25519 SSH key for the guest");
            ctx.run(
                &crate::platform::resolve_ssh_argv(vec![
                    "ssh-keygen".to_string(),
                    "-q".to_string(),
                    "-t".to_string(),
                    "ed25519".to_string(),
                    "-N".to_string(),
                    "".to_string(),
                    "-C".to_string(),
                    format!("vmctl:{}", cfg.name),
                    "-f".to_string(),
                    stage.join("id_ed25519").display().to_string(),
                ]),
                std::time::Duration::from_secs(60),
                true,
            )?;
            crate::util::chmod(&stage.join("id_ed25519"), 0o600)?;
            let ssh_pubkey = read_text(&stage.join("id_ed25519.pub")).trim().to_string();
            if !ssh_pubkey.starts_with("ssh-ed25519 ") {
                return message("ssh-keygen produced an unexpected public key");
            }
            let user_data = build_user_data(&cfg, &password_hash, &ssh_pubkey)?;
            let user_data_text = format!(
                "#cloud-config\n{}\n",
                crate::py_json::dumps_pretty_with(&user_data, true)
            );
            let seed_fingerprint = cfg.seed_fingerprint();
            let instance_id = if cfg.device_enabled() {
                format!(
                    "iid-{}",
                    &seed_fingerprint[..16.min(seed_fingerprint.len())]
                )
            } else {
                format!(
                    "vmctl-{}-{}",
                    cfg.name,
                    &seed_fingerprint[..12.min(seed_fingerprint.len())]
                )
            };
            let meta_data = json!({
                "instance-id": instance_id,
                "local-hostname": cfg.hostname,
            });
            let meta_data_text =
                format!("{}\n", crate::py_json::dumps_pretty_with(&meta_data, true));
            let network_config = build_network_config(&cfg)?;
            let network_config_text = format!(
                "{}\n",
                crate::py_json::dumps_pretty_with(&network_config, true)
            );
            atomic_write_text(&stage.join("user-data"), &user_data_text, 0o600)?;
            atomic_write_text(&stage.join("meta-data"), &meta_data_text, 0o600)?;
            atomic_write_text(&stage.join("network-config"), &network_config_text, 0o600)?;
            ctx.log("building the CIDATA seed ISO");
            crate::platform::build_seed_iso(
                &stage,
                &stage.join("seed.iso"),
                &["user-data", "meta-data", "network-config"],
            )?;
            ctx.log("verifying seed ISO label and payload");
            verify_seed_iso(
                &stage.join("seed.iso"),
                &[
                    ("user-data", &stage.join("user-data")),
                    ("meta-data", &stage.join("meta-data")),
                    ("network-config", &stage.join("network-config")),
                ],
            )?;
            let mut credentials = Map::new();
            credentials.insert("name".to_string(), json!(cfg.name));
            credentials.insert("username".to_string(), json!(cfg.username));
            credentials.insert("password".to_string(), json!(password));
            credentials.insert("hostname".to_string(), json!(cfg.hostname));
            credentials.insert("ssh_port".to_string(), json!(cfg.ssh_port));
            credentials.insert(
                "ssh_key".to_string(),
                json!(cfg.ssh_key_path().display().to_string()),
            );
            credentials.insert("proxy".to_string(), json!(cfg.proxy));
            credentials.insert("guest_proxy".to_string(), json!(cfg.guest_proxy_url()));
            credentials.insert("device".to_string(), json!(cfg.device_enabled()));
            credentials.insert(
                "device_fingerprint".to_string(),
                json!(cfg.device_fingerprint(None).unwrap_or_default()),
            );
            credentials.insert("created_at".to_string(), json!(now_millis()));
            atomic_write_json(
                &stage.join("credentials.json"),
                &Value::Object(credentials.clone()),
                0o600,
            )?;
            let moves = [
                ("seed.iso", cfg.seed_path()),
                ("user-data", cfg.user_data_path()),
                ("meta-data", cfg.meta_data_path()),
                ("network-config", cfg.network_config_path()),
                ("id_ed25519", cfg.ssh_key_path()),
                ("id_ed25519.pub", cfg.ssh_pub_path()),
                ("credentials.json", cfg.credentials_path()),
            ];
            for (name, target) in moves {
                std::fs::rename(stage.join(name), &target)?;
                ctx.track_file(&target)?;
            }
            Ok((credentials, meta_data))
        })();
        let _ = std::fs::remove_dir_all(&stage);
        let (_, meta_data) = result?;
        ctx.set_meta("seed_fingerprint", json!(cfg.seed_fingerprint()));
        ctx.set_meta("seed_sha256", json!(sha256_file(&cfg.seed_path())?));
        ctx.set_meta(
            "user_data_sha256",
            json!(sha256_file(&cfg.user_data_path())?),
        );
        ctx.set_meta(
            "network_config_sha256",
            json!(sha256_file(&cfg.network_config_path())?),
        );
        ctx.set_meta("ssh_pub_sha256", json!(sha256_file(&cfg.ssh_pub_path())?));
        ctx.set_meta(
            "instance_id",
            meta_data.get("instance-id").cloned().unwrap_or(Value::Null),
        );
        ctx.set_meta("guest_proxy", json!(cfg.guest_proxy_url()));
        ctx.set_meta("mac_mgmt", json!(cfg.mac_mgmt()));
        ctx.set_meta(
            "mac_data",
            json!(if cfg.needs_second_nic() {
                Some(cfg.mac_data())
            } else {
                None
            }),
        );
        ctx.set_meta(
            "password_source",
            json!(if cfg.password.is_some() {
                "user"
            } else {
                "generated"
            }),
        );
        ctx.set_meta("device", json!(cfg.device_enabled()));
        ctx.set_meta(
            "device_fingerprint",
            json!(cfg.device_fingerprint(None).unwrap_or_default()),
        );
        ctx.log(&format!(
            "seed ready: user={} hostname={} network={} proxy={} apt={}",
            cfg.username,
            cfg.hostname,
            cfg.network_mode,
            cfg.guest_proxy_url().unwrap_or_else(|| "none".to_string()),
            cfg.apt_mirror.as_deref().unwrap_or("image default")
        ));
        Ok(())
    }

    fn rollback(&self, ctx: &mut StepContext, state: &Value, force: bool) -> Result<()> {
        let cfg = ctx.cfg.clone();
        let provision = load_state(&cfg.home, &cfg.name, "provision").unwrap_or(Value::Null);
        if provision.get("status").and_then(Value::as_str) == Some("applied") {
            return message("provision is still applied; roll it back before the seed");
        }
        for name in SEED_FILES {
            let _ = std::fs::remove_file(cfg.instance_file(name));
        }
        delete_artifacts(ctx, state, force)
    }
}

#[cfg(test)]
mod apt_tests {
    use super::*;

    #[test]
    fn password_hashes_are_salted_sha512_crypt_that_verify() {
        use sha_crypt::{PasswordVerifier, ShaCrypt};
        let first = hash_password("correct horse").unwrap();
        assert!(first.starts_with("$6$"), "{first}");
        assert_ne!(first, hash_password("correct horse").unwrap());
        let hash = sha_crypt::PasswordHashRef::new(&first).unwrap();
        ShaCrypt::SHA512.verify_password(b"correct horse", hash).unwrap();
        assert!(ShaCrypt::SHA512.verify_password(b"wrong", hash).is_err());
        // `openssl passwd -6 -salt abcdefgh hunter2`: the format the guest checks.
        let known = "$6$abcdefgh$M/eYsB4rVXAm3ZNc88J.UD9rCKAT6FB1rahiwJCHtEndQNORCub5qhjxn50qbqVVthkM.9HpEwtf0t.iV9uH0/";
        ShaCrypt::SHA512
            .verify_password(b"hunter2", sha_crypt::PasswordHashRef::new(known).unwrap())
            .unwrap();
    }

    #[test]
    fn gateway_cloud_init_is_explicitly_ipv4_only() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for (os, device) in [("ubuntu-24.04", false), ("ubuntu-24.04", true), ("ubuntu-26.04", false)] {
            let cfg = crate::config::build_config(&json!({"name": "ipv4-test", "network_mode": "gateway", "os": os, "device": {"enabled": device}}), root).unwrap();
            let network = build_network_config(&cfg).unwrap();
            let iface = &network["ethernets"]["data0"];
            assert_eq!(iface["dhcp6"], false);
            assert_eq!(iface["accept-ra"], false);
            assert_eq!(iface["link-local"], json!(["ipv4"]));
            let data = build_user_data(&cfg, "hash", "ssh-ed25519 test").unwrap();
            let file = data["write_files"].as_array().unwrap().iter().find(|file| file["path"] == "/etc/sysctl.d/zz-cabin-ipv4.conf").unwrap();
            let content = file["content"].as_str().unwrap();
            assert!(content.contains("net.ipv6.conf.default.disable_ipv6 = 1"));
            assert!(content.contains("net.ipv6.conf.lo.disable_ipv6 = 0"));
        }
    }

    #[test]
    fn mirror_sets_primary_and_security_sources() {
        let apt = apt_settings(None, Some("https://ftp.kaist.ac.kr/ubuntu-ports"));
        assert_eq!(
            apt["primary"][0]["uri"],
            "https://ftp.kaist.ac.kr/ubuntu-ports"
        );
        assert_eq!(apt["security"][0]["arches"][0], "default");
        assert!(apt["conf"].as_str().unwrap().contains("Acquire::Retries"));
        assert!(apt_settings(None, None).get("primary").is_none());
    }

    #[test]
    fn chosen_mirror_replaces_default_sources_before_apt_runs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mirror = "https://mirrors.nju.edu.cn/ubuntu-ports";
        for device in [false, true] {
            let cfg = crate::config::build_config(
                &json!({
                    "name": "mirror-test",
                    "network_mode": "gateway",
                    "os": "ubuntu-26.04",
                    "apt_mirror": mirror,
                    "device": {"enabled": device},
                }),
                root,
            )
            .unwrap();
            let doc = build_user_data(&cfg, "test-hash", "ssh-ed25519 test-key").unwrap();
            let files = doc["write_files"].as_array().unwrap();
            let source = files
                .iter()
                .find(|file| file["path"] == "/etc/apt/sources.list.d/ubuntu.sources")
                .expect("the Ubuntu deb822 source is written early");
            let content = source["content"].as_str().unwrap();
            assert!(content.contains(mirror), "{content}");
            assert!(
                content.contains("Suites: resolute resolute-updates"),
                "{content}"
            );
            assert!(content.contains("Suites: resolute-security"), "{content}");
            assert!(!content.contains("archive.ubuntu.com"), "{content}");

            let legacy = files
                .iter()
                .find(|file| file["path"] == "/etc/apt/sources.list")
                .expect("the image's legacy source list is disabled");
            assert!(
                legacy["content"]
                    .as_str()
                    .unwrap()
                    .starts_with("# Package sources are managed by cabin."),
            );
            assert_eq!(doc["apt"]["primary"][0]["uri"], mirror);
        }
    }

    #[test]
    fn new_guests_request_the_generic_kernel_in_both_install_modes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for device in [false, true] {
            let cfg = crate::config::build_config(
                &json!({"name": "kernel-test", "network_mode": "gateway", "device": {"enabled": device}}),
                root,
            )
            .unwrap();
            let doc = build_user_data(&cfg, "test-hash", "ssh-ed25519 test-key").unwrap();
            let packages = doc["packages"].as_array().unwrap();
            assert_eq!(
                packages
                    .iter()
                    .filter(|p| p.as_str() == Some("linux-image-generic"))
                    .count(),
                1
            );
            let commands = doc["runcmd"].as_array().unwrap();
            assert_eq!(commands.last().unwrap()[2], switch_script());
        }
    }

    pub(super) fn switch_script() -> String {
        crate::os::find("ubuntu-24.04").unwrap().kernel().script()
    }
}

#[cfg(all(test, unix))]
mod kernel_switch_tests {
    use super::apt_tests::switch_script;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn exercise_kernel_switch(generic: &str, fail_mark: bool, fail_purge: &str) -> (bool, String) {
        let dir = std::env::temp_dir().join(format!(
            "cabin-kernel-switch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let programs = [
            (
                "dpkg-query",
                r##"#!/bin/sh
printf 'query %s\n' "$3" >> "$KERNEL_TEST_LOG"
case "$3" in
  linux-image-generic) [ "$KERNEL_TEST_GENERIC" = installed ] || exit 1 ;;
  linux-virtual|linux-image-virtual) ;;
  *) exit 1 ;;
esac
printf installed
"##,
            ),
            (
                "apt-mark",
                r##"#!/bin/sh
printf 'mark %s %s\n' "$1" "$2" >> "$KERNEL_TEST_LOG"
[ "$KERNEL_TEST_FAIL_MARK" != yes ]
"##,
            ),
            (
                "dpkg",
                r##"#!/bin/sh
printf 'purge %s\n' "$2" >> "$KERNEL_TEST_LOG"
[ "$KERNEL_TEST_FAIL_PURGE" != "$2" ]
"##,
            ),
        ];
        for (name, script) in programs {
            let path = dir.join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let log = dir.join("calls");
        let result = Command::new("/bin/sh")
            .args(["-c", &switch_script()])
            .env("PATH", &dir)
            .env("KERNEL_TEST_LOG", &log)
            .env("KERNEL_TEST_GENERIC", generic)
            .env(
                "KERNEL_TEST_FAIL_MARK",
                if fail_mark { "yes" } else { "no" },
            )
            .env("KERNEL_TEST_FAIL_PURGE", fail_purge)
            .output()
            .unwrap();
        let calls = std::fs::read_to_string(&log).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        (result.status.success(), calls)
    }

    #[test]
    fn kernel_switch_keeps_virtual_until_generic_is_installed_and_manual() {
        for (generic, fail_mark) in [("missing", false), ("installed", true)] {
            let (success, calls) = exercise_kernel_switch(generic, fail_mark, "");
            assert!(!success);
            assert!(!calls.contains("purge"), "{calls}");
        }
        let (success, calls) = exercise_kernel_switch("installed", false, "");
        assert!(success, "{calls}");
        assert!(
            calls.find("mark manual linux-image-generic").unwrap()
                < calls.find("purge linux-virtual").unwrap()
        );
        assert!(calls.contains("purge linux-image-virtual"));
        assert!(!calls.contains("purge linux-image-6."));

        let (success, calls) = exercise_kernel_switch("installed", false, "linux-virtual");
        assert!(!success);
        assert!(!calls.contains("purge linux-image-virtual"), "{calls}");
    }
}
