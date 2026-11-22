//! Effective configuration for one VM instance.
//!
//! The field set, defaults, validation rules and fingerprint payloads are kept
//! deterministic so existing instance identities remain stable across upgrades.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use crate::device::profile as device_profile;
use crate::download::Checksum;
use crate::paths::{InstancePaths, ResourcePaths, StatePaths};
use crate::py_json;
use crate::util::{host_ip, message, now_millis, read_json, sha256_text, timestamp_millis};
use crate::{Error, Result};

pub const CHROME_URL: &str = crate::arch::GuestArch::current().chrome_deb_url();

pub const NETWORK_MODES: [&str; 5] = [
    "user",
    "gateway",
    "vmnet-shared",
    "vmnet-bridged",
    "socket-vmnet",
];

pub(crate) fn validation_regex(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|error| {
        Error::Message(format!("invalid configuration validation pattern: {error}"))
    })
}

pub use crate::paths::{default_home, default_project_root, resolve_home, set_project_root};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub name: String,
    pub username: String,
    pub hostname: String,
    #[serde(skip)]
    pub password: Option<String>,
    pub ssh_port: u16,
    pub cpus: u32,
    pub memory_mb: u32,
    pub disk_size: String,
    pub network_mode: String,
    pub bridge_if: String,
    pub vmnet_gateway: String,
    pub socket_vmnet_socket: String,
    pub socket_vmnet_client: String,
    pub socket_vmnet_mode: String,
    pub proxy: Option<String>,
    /// The guest operating system (`os::RELEASES`).
    pub os: String,
    /// Archive mirror for the guest (`https://host/ubuntu-ports`); `None`
    /// keeps the image default.
    pub apt_mirror: Option<String>,
    pub desktop_env: Option<String>,
    pub desktop_chrome: bool,
    pub desktop_chrome_url: String,
    pub desktop_extra_packages: Vec<String>,
    pub desktop_extra_commands: Vec<String>,
    pub display_mode: String,
    pub desktop_resolution: String,
    pub desktop_scale: f64,
    pub gpu_mode: String,
    #[serde(default)]
    pub desktop_spice: bool,
    pub usb_tablet: bool,
    pub audio_backend: String,
    pub browser: Value,
    pub device: Value,
    /// The pinned image build (`os::Os::resolve`) and its digest. Ubuntu
    /// publishes SHA-256; SHA-512 is accepted for a pinned image that
    /// states one. Exactly one is set.
    pub image_url: String,
    pub image_sha256: String,
    pub image_sha512: String,
    pub provision_timeout: u64,
    #[serde(skip)]
    pub project_root: PathBuf,
    #[serde(skip)]
    pub home: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let project_root = default_project_root();
        let home = default_home();
        Self {
            name: "dev".to_string(),
            username: "dev".to_string(),
            hostname: "dev-vm".to_string(),
            password: None,
            ssh_port: 2222,
            cpus: 4,
            memory_mb: 4096,
            disk_size: "20G".to_string(),
            network_mode: "user".to_string(),
            bridge_if: "en0".to_string(),
            vmnet_gateway: "192.168.105.1".to_string(),
            socket_vmnet_socket: "/opt/homebrew/var/run/socket_vmnet".to_string(),
            socket_vmnet_client: "/opt/homebrew/opt/socket_vmnet/bin/socket_vmnet_client"
                .to_string(),
            socket_vmnet_mode: "shared".to_string(),
            proxy: None,
            os: crate::os::DEFAULT.to_string(),
            apt_mirror: None,
            desktop_env: None,
            desktop_chrome: false,
            desktop_chrome_url: CHROME_URL.to_string(),
            desktop_extra_packages: Vec::new(),
            desktop_extra_commands: Vec::new(),
            display_mode: "none".to_string(),
            desktop_resolution: "1600x900".to_string(),
            desktop_scale: 1.0,
            gpu_mode: "none".to_string(),
            desktop_spice: false,
            usb_tablet: false,
            audio_backend: "none".to_string(),
            browser: Value::Object(Map::new()),
            device: Value::Object(Map::new()),
            image_url: String::new(),
            image_sha256: String::new(),
            image_sha512: String::new(),
            provision_timeout: 900,
            project_root,
            home,
        }
    }
}

pub fn load_defaults(project_root: &Path) -> Result<Value> {
    let path = ResourcePaths::new(project_root).default_config();
    match read_json(&path) {
        Some(Value::Object(map)) if !map.is_empty() => Ok(Value::Object(map)),
        _ => message(format!(
            "missing or invalid config defaults: {}",
            path.display()
        )),
    }
}

fn string_at(value: &Value, key: &str, default: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| default.to_string())
}

fn optional_string_at(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(text)) => Some(text.clone()),
        _ => None,
    }
}

fn bool_at(value: &Value, key: &str, default: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn integer_at(value: &Value, key: &str, default: u64) -> u64 {
    value
        .get(key)
        .and_then(Value::as_u64)
        .or_else(|| {
            value
                .get(key)
                .and_then(Value::as_i64)
                .and_then(|v| u64::try_from(v).ok())
        })
        .unwrap_or(default)
}

fn float_at(value: &Value, key: &str, default: f64) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(default)
}

fn string_list_at(value: &Value, key: &str) -> Vec<String> {
    match value.get(key) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn object_at(value: &Value, key: &str) -> Value {
    value
        .get(key)
        .cloned()
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()))
}

fn legacy_device_key() -> &'static str {
    "\x73\x74\x65\x61\x6c\x74\x68"
}

fn device_override_at(value: &Value) -> Option<Value> {
    value
        .get("device")
        .or_else(|| value.get(legacy_device_key()))
        .cloned()
}

impl Config {
    pub fn from_value(
        value: &Value,
        project_root: &Path,
        home: &Path,
        name_override: Option<&str>,
    ) -> Result<Self> {
        let defaults = Config::default();
        let name = name_override
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| string_at(value, "name", &defaults.name));
        let proxy = optional_string_at(value, "proxy").filter(|text| !text.is_empty());
        let apt_mirror = optional_string_at(value, "apt_mirror")
            .map(|text| text.trim().trim_end_matches('/').to_string())
            .filter(|text| !text.is_empty());
        let desktop_env = optional_string_at(value, "desktop_env").filter(|text| !text.is_empty());
        Ok(Self {
            name,
            username: string_at(value, "username", &defaults.username),
            hostname: string_at(value, "hostname", &defaults.hostname),
            password: optional_string_at(value, "password"),
            ssh_port: integer_at(value, "ssh_port", defaults.ssh_port as u64) as u16,
            cpus: integer_at(value, "cpus", defaults.cpus as u64) as u32,
            memory_mb: integer_at(value, "memory_mb", defaults.memory_mb as u64) as u32,
            disk_size: string_at(value, "disk_size", &defaults.disk_size),
            network_mode: string_at(value, "network_mode", &defaults.network_mode),
            bridge_if: string_at(value, "bridge_if", &defaults.bridge_if),
            vmnet_gateway: string_at(value, "vmnet_gateway", &defaults.vmnet_gateway),
            socket_vmnet_socket: string_at(
                value,
                "socket_vmnet_socket",
                &defaults.socket_vmnet_socket,
            ),
            socket_vmnet_client: string_at(
                value,
                "socket_vmnet_client",
                &defaults.socket_vmnet_client,
            ),
            socket_vmnet_mode: string_at(value, "socket_vmnet_mode", &defaults.socket_vmnet_mode),
            proxy,
            apt_mirror,
            os: string_at(value, "os", &defaults.os),
            desktop_env,
            desktop_chrome: bool_at(value, "desktop_chrome", defaults.desktop_chrome),
            desktop_chrome_url: string_at(
                value,
                "desktop_chrome_url",
                &defaults.desktop_chrome_url,
            ),
            desktop_extra_packages: string_list_at(value, "desktop_extra_packages"),
            desktop_extra_commands: string_list_at(value, "desktop_extra_commands"),
            display_mode: string_at(value, "display_mode", &defaults.display_mode),
            desktop_resolution: string_at(
                value,
                "desktop_resolution",
                &defaults.desktop_resolution,
            ),
            desktop_scale: float_at(value, "desktop_scale", defaults.desktop_scale),
            // Historical QXL configs retain their current headless desktop.
            gpu_mode: match string_at(value, "gpu_mode", &defaults.gpu_mode).as_str() {
                "qxl" => "none".to_string(),
                mode => mode.to_string(),
            },
            desktop_spice: bool_at(value, "desktop_spice", defaults.desktop_spice),
            usb_tablet: bool_at(value, "usb_tablet", defaults.usb_tablet),
            audio_backend: string_at(value, "audio_backend", &defaults.audio_backend),
            browser: object_at(value, "browser"),
            device: device_override_at(value)
                .filter(Value::is_object)
                .unwrap_or_else(|| Value::Object(Map::new())),
            image_url: string_at(value, "image_url", &defaults.image_url),
            image_sha256: string_at(value, "image_sha256", &defaults.image_sha256),
            image_sha512: string_at(value, "image_sha512", &defaults.image_sha512),
            provision_timeout: integer_at(value, "provision_timeout", defaults.provision_timeout),
            project_root: project_root.to_path_buf(),
            home: home.to_path_buf(),
        })
    }

    // ---- derived paths -------------------------------------------------
    fn instance_paths(&self) -> InstancePaths {
        StatePaths::new(&self.home).instance(&self.name)
    }

    pub fn instance_dir(&self) -> PathBuf {
        self.instance_paths().root().to_path_buf()
    }
    pub fn instance_file(&self, name: &str) -> PathBuf {
        self.instance_paths().file(name)
    }
    pub fn images_dir(&self) -> PathBuf {
        StatePaths::new(&self.home).images()
    }
    pub fn image_file_name(&self) -> String {
        Url::parse(&self.image_url)
            .ok()
            .and_then(|url| {
                url.path_segments()
                    .and_then(|mut parts| parts.next_back().map(ToOwned::to_owned))
            })
            .unwrap_or_default()
    }
    pub fn image_path(&self) -> PathBuf {
        StatePaths::new(&self.home).image(&self.image_file_name())
    }
    pub fn bundled_image_path(&self) -> PathBuf {
        ResourcePaths::new(&self.project_root).image(&self.image_file_name())
    }
    pub fn disk_path(&self) -> PathBuf {
        self.instance_paths().disk()
    }
    pub fn efi_vars_path(&self) -> PathBuf {
        self.instance_paths().efi_vars()
    }
    pub fn seed_path(&self) -> PathBuf {
        self.instance_paths().seed()
    }
    pub fn user_data_path(&self) -> PathBuf {
        self.instance_paths().user_data()
    }
    pub fn meta_data_path(&self) -> PathBuf {
        self.instance_paths().meta_data()
    }
    pub fn network_config_path(&self) -> PathBuf {
        self.instance_paths().network_config()
    }
    pub fn ssh_key_path(&self) -> PathBuf {
        self.instance_paths().ssh_key()
    }
    pub fn ssh_pub_path(&self) -> PathBuf {
        self.instance_paths().ssh_public_key()
    }
    pub fn credentials_path(&self) -> PathBuf {
        self.instance_paths().credentials()
    }
    pub fn instance_meta_path(&self) -> PathBuf {
        self.instance_paths().meta()
    }
    pub fn provision_marker_path(&self) -> PathBuf {
        self.instance_paths().provisioned()
    }
    pub fn runtime_path(&self) -> PathBuf {
        self.instance_paths().runtime()
    }
    pub fn pidfile(&self) -> PathBuf {
        self.instance_paths().pidfile()
    }
    pub fn qmp_socket(&self) -> PathBuf {
        self.instance_paths().qmp_socket()
    }
    pub fn qga_socket(&self) -> PathBuf {
        self.instance_paths().qga_socket()
    }
    pub fn console_socket(&self) -> PathBuf {
        self.instance_paths().console_socket()
    }
    pub(crate) fn spice_dir(&self) -> PathBuf {
        self.instance_paths().spice_dir()
    }
    pub(crate) fn spice_socket(&self) -> PathBuf {
        self.spice_dir().join("s.sock")
    }
    pub fn provision_serial_log(&self) -> PathBuf {
        self.instance_paths().provision_serial()
    }
    pub fn serial_log_path(&self) -> PathBuf {
        self.instance_file("serial.log")
    }
    pub fn logs_dir(&self) -> PathBuf {
        StatePaths::new(&self.home).logs()
    }
    pub fn instance_logs_dir(&self) -> PathBuf {
        StatePaths::new(&self.home).instance_logs(&self.name)
    }
    pub fn tmp_dir(&self) -> PathBuf {
        StatePaths::new(&self.home).tmp()
    }
    pub fn bin_dir(&self) -> PathBuf {
        StatePaths::new(&self.home).bin()
    }
    pub fn identity_path(&self) -> PathBuf {
        self.instance_paths().identity()
    }
    pub fn desktop_marker_path(&self) -> PathBuf {
        self.instance_paths().desktop()
    }
    pub fn browser_marker_path(&self) -> PathBuf {
        self.instance_paths().browser()
    }
    pub fn device_dir(&self) -> PathBuf {
        let paths = self.instance_paths();
        let current = paths.device();
        let previous = paths.legacy_device();
        if current.is_dir() || !previous.is_dir() {
            current
        } else {
            previous
        }
    }
    pub fn smbios_path(&self) -> PathBuf {
        self.device_dir().join("smbios.bin")
    }
    pub fn device_runtime_marker(&self) -> PathBuf {
        self.device_dir().join("runtime.json")
    }
    pub fn device_report_path(&self) -> PathBuf {
        self.device_dir().join("report.json")
    }
    pub fn device_raw_path(&self) -> PathBuf {
        self.device_dir().join("report-raw.json")
    }
    pub fn device_seal_path(&self) -> PathBuf {
        self.device_dir().join("seal.json")
    }

    pub fn current_identity(&self) -> Value {
        read_json(&self.identity_path())
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Map::new()))
    }

    pub fn current_username(&self) -> String {
        self.current_identity()
            .get("username")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.username.clone())
    }

    pub fn current_hostname(&self) -> String {
        self.current_identity()
            .get("hostname")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| self.hostname.clone())
    }

    pub fn current_timezone(&self) -> String {
        if let Some(text) = self
            .current_identity()
            .get("timezone")
            .and_then(Value::as_str)
        {
            return text.to_string();
        }
        if self.device_enabled() {
            return self
                .device_resolved()
                .ok()
                .and_then(|resolved| {
                    device_profile::get_path(&resolved, "guest.timezone")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| "Etc/UTC".to_string());
        }
        "Etc/UTC".to_string()
    }

    pub fn current_locale(&self) -> String {
        if let Some(text) = self
            .current_identity()
            .get("locale")
            .and_then(Value::as_str)
        {
            return text.to_string();
        }
        if self.device_enabled() {
            return self
                .device_resolved()
                .ok()
                .and_then(|resolved| {
                    device_profile::get_path(&resolved, "guest.locale")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .unwrap_or_else(|| "en_US.UTF-8".to_string());
        }
        "en_US.UTF-8".to_string()
    }

    pub fn display_geometry(&self) -> Result<crate::display::DisplayGeometry> {
        crate::display::DisplayGeometry::parse(&self.desktop_resolution, self.desktop_scale)
    }

    // ---- MACs ----------------------------------------------------------
    pub fn mac_mgmt(&self) -> String {
        if self.device_enabled()
            && let Ok(resolved) = self.device_resolved()
            && let Some(mac) =
                device_profile::get_path(&resolved, "network.mac").and_then(Value::as_str)
        {
            return mac.to_string();
        }
        mac_for(&self.name, 1)
    }

    pub fn mac_data(&self) -> String {
        if self.isolated() {
            return crate::netgw::GUEST_MAC
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<Vec<_>>()
                .join(":");
        }
        self.mac_data_profile()
    }

    fn mac_data_profile(&self) -> String {
        if self.device_enabled()
            && let Ok(resolved) = self.device_resolved()
            && let Some(mac) =
                device_profile::get_path(&resolved, "network.mac").and_then(Value::as_str)
        {
            return mac.to_string();
        }
        mac_for(&self.name, 2)
    }

    pub fn effective_network_mode(&self) -> String {
        if self.device_enabled()
            && let Ok(resolved) = self.device_resolved()
            && let Some(mode) =
                device_profile::get_path(&resolved, "network.mode").and_then(Value::as_str)
        {
            return mode.to_string();
        }
        self.network_mode.clone()
    }

    pub fn needs_data_nic(&self) -> bool {
        matches!(
            self.effective_network_mode().as_str(),
            "vmnet-shared" | "vmnet-bridged" | "socket-vmnet" | "gateway"
        )
    }

    /// The VM's internet goes only through the Mac's gateway (`netgw`).
    pub fn isolated(&self) -> bool {
        self.effective_network_mode() == "gateway"
    }

    /// Where the gateway records the Mac-side SSH port it actually got, when
    /// the configured one was taken (gateway mode only; runtime state, not
    /// configuration, so fingerprints do not change).
    pub fn ssh_relay_port_path(&self) -> PathBuf {
        self.instance_file("ssh-relay-port")
    }

    /// The port to connect to for SSH: the gateway's actual relay port when
    /// it had to move, else the configured one.
    pub fn ssh_connect_port(&self) -> u16 {
        if self.isolated()
            && let Ok(text) = std::fs::read_to_string(self.ssh_relay_port_path())
            && let Ok(port) = text.trim().parse::<u16>()
            && port != 0
        {
            return port;
        }
        self.ssh_port
    }

    /// Record (or with `None`/the configured port, clear) the relay port.
    pub fn set_ssh_relay_port(&self, port: Option<u16>) -> Result<()> {
        let path = self.ssh_relay_port_path();
        match port.filter(|port| *port != self.ssh_port) {
            Some(port) => {
                crate::util::atomic_write_bytes(&path, port.to_string().as_bytes(), 0o600)
            }
            None => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
                _ => Ok(()),
            },
        }
    }

    /// Unix socket where QEMU reaches the gateway (`-netdev stream`).
    pub fn net_socket(&self) -> PathBuf {
        self.instance_file("net.sock")
    }

    pub fn needs_second_nic(&self) -> bool {
        self.needs_data_nic()
    }

    pub fn direct_vmnet(&self) -> bool {
        matches!(
            self.effective_network_mode().as_str(),
            "vmnet-shared" | "vmnet-bridged"
        )
    }

    pub fn uses_vmnet(&self) -> bool {
        self.needs_data_nic()
    }

    // ---- proxy ---------------------------------------------------------
    pub fn host_ip_for_guest(&self) -> String {
        match self.network_mode.as_str() {
            "user" => "10.0.2.2".to_string(),
            "vmnet-shared" => self.vmnet_gateway.clone(),
            "socket-vmnet" if self.socket_vmnet_mode == "shared" => self.vmnet_gateway.clone(),
            _ => host_ip(&self.bridge_if).unwrap_or_else(|| "127.0.0.1".to_string()),
        }
    }

    pub fn guest_proxy_url(&self) -> Option<String> {
        let proxy = self.proxy.as_deref()?;
        let parsed = Url::parse(proxy).ok()?;
        let hostname = parsed.host_str()?;
        if !matches!(hostname, "127.0.0.1" | "localhost" | "::1" | "[::1]") {
            return Some(proxy.to_string());
        }
        let host = self.host_ip_for_guest();
        let mut authority = String::new();
        if !parsed.username().is_empty() {
            authority.push_str(parsed.username());
            if let Some(password) = parsed.password() {
                authority.push(':');
                authority.push_str(password);
            }
            authority.push('@');
        }
        authority.push_str(&host);
        if let Some(port) = parsed.port() {
            authority.push(':');
            authority.push_str(&port.to_string());
        }
        Some(replace_authority(proxy, &parsed, &authority))
    }

    // ---- browser -------------------------------------------------------
    pub fn browser_enabled(&self) -> bool {
        self.browser
            .get("enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    pub fn browser_fingerprint(&self) -> Result<String> {
        if self.browser_enabled() {
            crate::browser::fingerprint(self)
        } else {
            Ok(String::new())
        }
    }

    // ---- device -------------------------------------------------------
    pub fn device_enabled(&self) -> bool {
        device_profile::enabled(&self.device)
    }

    pub fn device_resolved(&self) -> Result<Value> {
        device_profile::resolve(&self.device, &self.name, self.cpus, self.memory_mb)
    }

    pub fn device_fingerprint(&self, group: Option<&str>) -> Result<String> {
        if !self.device_enabled() {
            return Ok(String::new());
        }
        let resolved = self.device_resolved()?;
        match group {
            Some(group) => {
                device_profile::group_fingerprint(&resolved, group, self.cpus, self.memory_mb)
            }
            None => device_profile::fingerprint(&resolved, self.cpus, self.memory_mb),
        }
    }

    // ---- fingerprints --------------------------------------------------
    pub fn disk_fingerprint(&self) -> String {
        let mut payload = json!({
            "name": self.name,
            "disk_size": self.disk_size,
            "image_url": self.image_url,
            "image_sha256": self.image_sha256,
        });
        // Only present for SHA-512 images, so existing machines keep their
        // fingerprint.
        if !self.image_sha512.is_empty() {
            payload["image_sha512"] = json!(self.image_sha512);
        }
        sha256_text(&py_json::dumps_compact(&payload))
    }

    pub fn image_pinned(&self) -> bool {
        !self.image_url.is_empty()
    }

    /// Record the newest build of the chosen OS (network). A config that
    /// already names its image keeps it, so an installed machine's image
    /// never changes.
    pub fn pin_image(&mut self) -> Result<()> {
        if self.image_pinned() {
            return Ok(());
        }
        let image = crate::os::get(&self.os)?.resolve()?;
        self.image_url = image.url;
        match image.checksum {
            Checksum::Sha256(hex) => {
                self.image_sha256 = hex;
                self.image_sha512.clear();
            }
            Checksum::Sha512(hex) => {
                self.image_sha512 = hex;
                self.image_sha256.clear();
            }
        }
        self.validate()
    }

    /// Reuse the image an existing instance of this name was installed
    /// with (same OS), so re-applying never moves it to a newer build.
    pub fn adopt_stored_image(&mut self) {
        if self.image_pinned() {
            return;
        }
        let Some(stored) =
            load_instance_meta(&self.home, &self.name).and_then(|meta| meta.get("config").cloned())
        else {
            return;
        };
        let text = |key: &str| {
            stored
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let stored_os = stored
            .get("os")
            .and_then(Value::as_str)
            .unwrap_or(crate::os::DEFAULT);
        if stored_os == self.os && !text("image_url").is_empty() {
            self.image_url = text("image_url");
            self.image_sha256 = text("image_sha256");
            self.image_sha512 = text("image_sha512");
        }
    }

    /// The pinned image's digest.
    pub fn image_checksum(&self) -> Checksum {
        if self.image_sha512.is_empty() {
            Checksum::Sha256(self.image_sha256.clone())
        } else {
            Checksum::Sha512(self.image_sha512.clone())
        }
    }

    /// The guest operating system (validated by `validate`).
    pub fn guest_os(&self) -> &'static crate::os::Os {
        crate::os::find(&self.os).unwrap_or(&crate::os::RELEASES[0])
    }

    fn seed_fingerprint_payload(&self) -> Value {
        let mut payload = Map::new();
        payload.insert("username".to_string(), json!(self.username));
        payload.insert("hostname".to_string(), json!(self.hostname));
        payload.insert(
            "password".to_string(),
            json!(
                self.password
                    .clone()
                    .unwrap_or_else(|| "<generated>".to_string())
            ),
        );
        payload.insert(
            "proxy".to_string(),
            json!(self.proxy.clone().unwrap_or_default()),
        );
        payload.insert(
            "guest_proxy".to_string(),
            json!(self.guest_proxy_url().unwrap_or_default()),
        );
        // Only present when set, so existing instances keep their fingerprint.
        if self.os != crate::os::DEFAULT {
            payload.insert("os".to_string(), json!(self.os));
        }
        if let Some(mirror) = &self.apt_mirror {
            payload.insert("apt_mirror".to_string(), json!(mirror));
        }
        payload.insert("network_mode".to_string(), json!(self.network_mode));
        payload.insert("bridge_if".to_string(), json!(self.bridge_if));
        payload.insert("ssh_port".to_string(), json!(self.ssh_port));
        if self.network_mode == "socket-vmnet" {
            payload.insert(
                "socket_vmnet_mode".to_string(),
                json!(self.socket_vmnet_mode),
            );
        }
        if self.device_enabled() {
            // Keep the historical names: they are part of the fingerprint
            // contract for instances provisioned before the device rename.
            payload.insert(
                "stealth_guest".to_string(),
                json!(self.device_fingerprint(Some("guest")).unwrap_or_default()),
            );
            payload.insert(
                "stealth_network".to_string(),
                json!(self.device_fingerprint(Some("network")).unwrap_or_default()),
            );
            payload.insert(
                "stealth_devices".to_string(),
                json!(self.device_fingerprint(Some("devices")).unwrap_or_default()),
            );
        }
        Value::Object(payload)
    }

    pub fn seed_fingerprint(&self) -> String {
        sha256_text(&py_json::dumps_compact(&self.seed_fingerprint_payload()))
    }

    fn fingerprint_payload(&self) -> Value {
        let mut payload = Map::new();
        payload.insert("disk".to_string(), json!(self.disk_fingerprint()));
        payload.insert("seed".to_string(), json!(self.seed_fingerprint()));
        payload.insert("cpus".to_string(), json!(self.cpus));
        payload.insert("memory_mb".to_string(), json!(self.memory_mb));
        if self.device_enabled() {
            // See seed_fingerprint: the key predates the device rename.
            payload.insert(
                "stealth".to_string(),
                json!(self.device_fingerprint(None).unwrap_or_default()),
            );
        }
        Value::Object(payload)
    }

    pub fn fingerprint(&self) -> String {
        sha256_text(&py_json::dumps_compact(&self.fingerprint_payload()))
    }

    /// Stable identity for every setting that can change the observable
    /// machine environment. It is deliberately separate from the install
    /// fingerprint, which also covers disk and seed artifacts.
    pub fn environment_revision(&self) -> Result<String> {
        let payload = json!({
            "machine": self.fingerprint(),
            "username": self.current_username(),
            "hostname": self.current_hostname(),
            "timezone": self.current_timezone(),
            "locale": self.current_locale(),
            "display_resolution": self.desktop_resolution,
            "display_scale": self.desktop_scale,
            "desktop": self.desktop_env,
            "browser": self.browser_fingerprint()?,
            "device": self.device_fingerprint(None)?,
        });
        let mut payload = payload;
        if self.desktop_spice {
            payload["desktop_spice"] = json!(true);
        }
        Ok(sha256_text(&py_json::dumps_compact(&payload)))
    }

    // ---- validation ----------------------------------------------------
    pub fn validate(&self) -> Result<()> {
        if !validation_regex(r"^[a-z][a-z0-9-]{1,30}$")?.is_match(&self.name) {
            return message("--name must match ^[a-z][a-z0-9-]{1,30}$ (lowercase, 2-31 chars)");
        }
        if !validation_regex(r"^[a-z_][a-z0-9_-]{0,31}$")?.is_match(&self.username) {
            return message("--user must be a valid lowercase Linux username (max 32 chars)");
        }
        if !valid_hostname(&self.hostname) {
            return message(
                "--hostname must be a valid RFC1123 hostname (lowercase, max 63 chars)",
            );
        }
        if let Some(password) = &self.password
            && password.len() < 8
        {
            return message("--password must be at least 8 characters (or omit to generate one)");
        }
        if !(1024..=65535).contains(&self.ssh_port) {
            return message("--ssh-port must be between 1024 and 65535");
        }
        if let Some(mirror) = &self.apt_mirror {
            let valid = url::Url::parse(mirror)
                .map(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
                .unwrap_or(false);
            if !valid {
                return message(
                    "--apt-mirror must be an http(s) URL such as https://ftp.kaist.ac.kr/ubuntu-ports",
                );
            }
        }
        if !(1..=64).contains(&self.cpus) {
            return message("--cpus must be between 1 and 64");
        }
        if !(512..=65536).contains(&self.memory_mb) {
            return message("--memory must be between 512 and 65536 MiB");
        }
        if !validation_regex(r"(?i)^\s*\d+\s*[KMGTP]?\s*$")?.is_match(&self.disk_size) {
            return message("--disk-size must look like 20G (K/M/G/T allowed)");
        }
        crate::util::parse_size_bytes(&self.disk_size)?;
        if !NETWORK_MODES.contains(&self.network_mode.as_str()) {
            return message(format!(
                "--network must be one of: {}",
                NETWORK_MODES.join(", ")
            ));
        }
        if self.bridge_if.is_empty() {
            return message("--bridge requires an interface name such as en0");
        }
        if !matches!(self.socket_vmnet_mode.as_str(), "shared" | "bridged") {
            return message("--socket-vmnet-mode must be shared or bridged");
        }
        if let Some(proxy) = &self.proxy {
            let parsed = Url::parse(proxy).ok();
            match parsed {
                Some(url)
                    if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => {}
                _ => return message("--proxy must be a URL like http://127.0.0.1:7890"),
            }
        }
        crate::os::get(&self.os)?;
        // An unpinned config (no image yet) is valid: `pin_image` fills it in
        // before an install saves anything.
        if self.image_pinned() {
            if !self.image_sha256.is_empty() && !self.image_sha512.is_empty() {
                return message("set only one of image_sha256 and image_sha512");
            }
            if !self.image_checksum().is_well_formed() {
                return message(
                    "image_sha256 / image_sha512 must be a lowercase hex digest (64 or 128 characters)",
                );
            }
        }
        if self.image_pinned()
            && !(self.image_url.starts_with("http://") || self.image_url.starts_with("https://"))
        {
            return message("image_url must be an http(s) URL");
        }
        if self.provision_timeout < 60 {
            return message("provision_timeout must be at least 60 seconds");
        }
        if let Some(env) = &self.desktop_env
            && env != "sway"
        {
            return message("--desktop currently supports: sway (Wayland), none");
        }
        if !matches!(self.display_mode.as_str(), "none" | "cocoa") {
            return message("--display must be none or cocoa");
        }
        let geometry = self.display_geometry()?;
        let (logical_width, logical_height) = geometry.logical_size();
        if logical_width < 640 || logical_height < 480 {
            return message(
                "--desktop-resolution / --desktop-scale leaves less than 640x480 logical pixels",
            );
        }
        if !matches!(self.gpu_mode.as_str(), "none" | "virtio" | "ramfb") {
            return message("--gpu must be none, virtio or ramfb");
        }
        if !matches!(self.audio_backend.as_str(), "none" | "coreaudio") {
            return message("--audio must be none or coreaudio");
        }
        if self.display_mode == "cocoa" && self.gpu_mode == "none" {
            return message(
                "--display cocoa needs a guest framebuffer; use --gpu virtio (or ramfb)",
            );
        }
        if self.desktop_spice
            && (self.device_enabled()
                || self.desktop_env.as_deref() != Some("sway")
                || self.gpu_mode != "virtio")
        {
            return message(
                "SPICE requires a Sway desktop with virtio-gpu and a disabled device profile",
            );
        }
        if self.desktop_env.is_some() && self.device_enabled() && self.gpu_mode != "none" {
            return message(
                "desktop GPU and device are incompatible: virtio-gpu is a visible virtio device. Use --no-device for a GPU desktop, or --gpu none for a headless desktop profile.",
            );
        }
        if self.desktop_chrome && self.desktop_env.is_none() {
            return message("--chrome requires --desktop sway");
        }
        if self.browser_enabled() {
            crate::browser::validate(&self.browser, self)?;
        }
        if self.usb_tablet && self.device_enabled() {
            let usb_enabled = device_profile::get_path(&self.device, "devices.usb.enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if !usb_enabled {
                return message(
                    "--usb-tablet requires the device USB controller (devices.usb.enabled=true)",
                );
            }
        }
        if self.device_enabled() {
            device_profile::validate(&self.device, &self.name, self.cpus, self.memory_mb)?;
        }
        Ok(())
    }

    // ---- storage -------------------------------------------------------
    pub fn storage(&self) -> Value {
        json!({
            "name": self.name,
            "username": self.username,
            "hostname": self.hostname,
            "ssh_port": self.ssh_port,
            "cpus": self.cpus,
            "memory_mb": self.memory_mb,
            "disk_size": self.disk_size,
            "network_mode": self.network_mode,
            "bridge_if": self.bridge_if,
            "vmnet_gateway": self.vmnet_gateway,
            "socket_vmnet_socket": self.socket_vmnet_socket,
            "socket_vmnet_client": self.socket_vmnet_client,
            "socket_vmnet_mode": self.socket_vmnet_mode,
            "proxy": self.proxy,
            "os": self.os,
            "apt_mirror": self.apt_mirror,
            "desktop_env": self.desktop_env,
            "desktop_chrome": self.desktop_chrome,
            "desktop_chrome_url": self.desktop_chrome_url,
            "desktop_extra_packages": self.desktop_extra_packages,
            "desktop_extra_commands": self.desktop_extra_commands,
            "display_mode": self.display_mode,
            "desktop_resolution": self.desktop_resolution,
            "desktop_scale": self.desktop_scale,
            "gpu_mode": self.gpu_mode,
            "desktop_spice": self.desktop_spice,
            "usb_tablet": self.usb_tablet,
            "audio_backend": self.audio_backend,
            "browser": self.browser,
            "device": self.device,
            "image_url": self.image_url,
            "image_sha256": self.image_sha256,
            "image_sha512": self.image_sha512,
            "provision_timeout": self.provision_timeout,
        })
    }

    pub fn instance_meta(&self) -> Value {
        let mut fingerprints = Map::new();
        fingerprints.insert("overall".to_string(), json!(self.fingerprint()));
        fingerprints.insert("disk".to_string(), json!(self.disk_fingerprint()));
        fingerprints.insert("seed".to_string(), json!(self.seed_fingerprint()));
        if self.device_enabled() {
            fingerprints.insert(
                "device".to_string(),
                json!(self.device_fingerprint(None).unwrap_or_default()),
            );
            let mut groups = Map::new();
            for group in device_profile::GROUPS {
                groups.insert(
                    group.to_string(),
                    json!(self.device_fingerprint(Some(group)).unwrap_or_default()),
                );
            }
            fingerprints.insert("device_groups".to_string(), Value::Object(groups));
        } else {
            fingerprints.insert("device".to_string(), json!(""));
            fingerprints.insert("device_groups".to_string(), json!({}));
        }
        json!({
            "version": 1,
            "name": self.name,
            "config": self.storage(),
            "fingerprints": Value::Object(fingerprints),
            "password_source": if self.password.is_some() { "user" } else { "generated" },
        })
    }

    pub fn save_instance_meta(&self) -> Result<()> {
        let mut meta = self.instance_meta();
        let existing = read_json(&self.instance_meta_path());
        let created_at = existing
            .as_ref()
            .and_then(|value| value.get("created_at"))
            .and_then(timestamp_millis)
            .unwrap_or_else(now_millis);
        if let Some(map) = meta.as_object_mut() {
            map.insert("created_at".to_string(), Value::from(created_at));
        }
        crate::util::atomic_write_json(&self.instance_meta_path(), &meta, 0o600)
    }
}

fn valid_hostname(text: &str) -> bool {
    if text.is_empty() || text.len() > 63 {
        return false;
    }
    let bytes = text.as_bytes();
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit() {
        return false;
    }
    text.chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
}

fn mac_for(name: &str, index: u32) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("vmctl:{name}:{index}").as_bytes());
    let digest = hasher.finalize();
    format!(
        "52:54:00:{:02x}:{:02x}:{:02x}",
        digest[0], digest[1], digest[2]
    )
}

fn replace_authority(original: &str, parsed: &Url, authority: &str) -> String {
    let scheme = parsed.scheme();
    let prefix = format!("{scheme}://");
    let Some(rest) = original.strip_prefix(&prefix) else {
        return original.to_string();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let suffix = &rest[end..];
    format!("{prefix}{authority}{suffix}")
}

pub fn load_instance_meta(home: &Path, name: &str) -> Option<Value> {
    read_json(&StatePaths::new(home).instance_meta(name))
}

pub fn load_instance_config(project_root: &Path, home: &Path, name: &str) -> Result<Config> {
    let meta = load_instance_meta(home, name).ok_or_else(|| {
        Error::Message(format!(
            "instance {name:?} does not exist (no instance.json); run 'vmctl apply --name {name}' first"
        ))
    })?;
    let mut raw = meta
        .get("config")
        .and_then(Value::as_object)
        .cloned()
        .ok_or_else(|| {
            Error::Message(format!(
                "instance {name:?} does not exist (no instance.json); run 'vmctl apply --name {name}' first"
            ))
        })?;
    raw.remove("name");
    let device_override = raw
        .remove("device")
        .or_else(|| raw.remove(legacy_device_key()))
        .unwrap_or_else(|| json!({}));
    raw.insert(
        "device".to_string(),
        device_profile::deep_merge(
            &device_profile::load_defaults(project_root)?,
            &device_override,
        ),
    );
    if let Some(device) = raw.get_mut("device") {
        adopt_existing_device_identities(device, home, name)?;
    }
    let browser_override = raw.remove("browser").unwrap_or_else(|| json!({}));
    raw.insert(
        "browser".to_string(),
        crate::browser::deep_merge(
            &crate::browser::load_defaults(project_root)?,
            &browser_override,
        ),
    );
    let cfg = Config::from_value(&Value::Object(raw), project_root, home, Some(name))?;
    cfg.validate()?;
    Ok(cfg)
}

fn adopt_existing_device_identities(device: &mut Value, home: &Path, name: &str) -> Result<()> {
    if !device_profile::enabled(device) {
        return Ok(());
    }
    let paths = StatePaths::new(home).instance(name);
    let device_dir = if paths.device().is_dir() || !paths.legacy_device().is_dir() {
        paths.device()
    } else {
        paths.legacy_device()
    };
    for (file, group, fields) in [
        (
            "smbios.json",
            "smbios",
            [
                "system.serial",
                "system.uuid",
                "baseboard.serial",
                "baseboard.asset",
                "chassis.serial",
                "chassis.asset",
                "processor.serial",
                "processor.asset",
            ]
            .as_slice(),
        ),
        (
            "devices.json",
            "devices",
            ["storage.serial", "optical.serial"].as_slice(),
        ),
        ("network.json", "network", ["mac"].as_slice()),
    ] {
        let path = device_dir.join(file);
        if !path.exists() {
            continue;
        }
        let artifact = read_json(&path).ok_or_else(|| {
            Error::Message(format!(
                "could not read existing device identity from {}",
                path.display()
            ))
        })?;
        let source = artifact.get(group).unwrap_or(&Value::Null);
        for field in fields {
            let configured = device_profile::get_path(device, &format!("{group}.{field}"))
                .map(|value| !value.is_null())
                .unwrap_or(false);
            if configured {
                continue;
            }
            if let Some(value) = device_profile::get_path(source, field)
                && !value.is_null()
            {
                device_profile::set_path(device, &format!("{group}.{field}"), value.clone())?;
            }
        }
    }
    Ok(())
}

pub fn build_config(overrides: &Value, project_root: &Path) -> Result<Config> {
    let defaults = load_defaults(project_root)?;
    let Some(default_map) = defaults.as_object() else {
        return message("config defaults must be an object");
    };
    let mut merged = default_map.clone();
    let Some(overrides_map) = overrides.as_object() else {
        return message("config overrides must be an object");
    };
    let home_override = overrides_map.get("home").cloned();
    let device_override =
        device_override_at(&Value::Object(overrides_map.clone())).unwrap_or_else(|| json!({}));
    let browser_override = overrides_map
        .get("browser")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let provided_network_mode = overrides_map
        .get("network_mode")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    for (key, value) in overrides_map {
        if key == "device" || key == "browser" {
            continue;
        }
        if !value.is_null() {
            merged.insert(key.clone(), value.clone());
        }
    }
    let device = device_profile::deep_merge(
        &device_profile::load_defaults(project_root)?,
        &device_override,
    );
    let browser = crate::browser::deep_merge(
        &crate::browser::load_defaults(project_root)?,
        &browser_override,
    );
    merged.insert("device".to_string(), device.clone());
    merged.insert("browser".to_string(), browser);
    if device_profile::enabled(&device)
        && let Some(mode) = provided_network_mode
    {
        let mut device_value = device;
        device_profile::set_path(&mut device_value, "network.mode", json!(mode))?;
        merged.insert("device".to_string(), device_value);
    }
    let env_home = std::env::var("VMCTL_HOME").ok().map(PathBuf::from);
    let home = home_override
        .as_ref()
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or(env_home)
        .or_else(|| {
            merged
                .remove("home")
                .and_then(|value| value.as_str().map(PathBuf::from))
        })
        .unwrap_or_else(default_home);
    let mut cfg = Config::from_value(&Value::Object(merged), project_root, &home, None)?;
    if cfg
        .proxy
        .as_ref()
        .map(|text| text.trim().is_empty())
        .unwrap_or(false)
    {
        cfg.proxy = None;
    }
    cfg.validate()?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_moved_ssh_relay_is_used_for_connecting_but_not_configured() {
        let home = std::env::temp_dir().join(format!("vm-relay-port-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let cfg = build_config(
            &json!({"name": "relay", "home": home, "network_mode": "gateway", "device": {"enabled": false}}),
            &default_project_root(),
        )
        .unwrap();
        let fingerprint = cfg.fingerprint();
        assert_eq!(cfg.ssh_connect_port(), cfg.ssh_port);
        cfg.set_ssh_relay_port(Some(40123)).unwrap();
        assert_eq!(cfg.ssh_connect_port(), 40123);
        assert_eq!(
            cfg.fingerprint(),
            fingerprint,
            "runtime state, not configuration"
        );
        cfg.set_ssh_relay_port(Some(cfg.ssh_port)).unwrap();
        assert!(
            !cfg.ssh_relay_port_path().exists(),
            "the configured port needs no record"
        );
        assert_eq!(cfg.ssh_connect_port(), cfg.ssh_port);
        let _ = std::fs::remove_dir_all(home);
    }

    use super::*;

    #[test]
    fn legacy_device_configuration_remains_readable() {
        let mut overrides = json!({"name": "legacy-device"});
        overrides.as_object_mut().unwrap().insert(
            legacy_device_key().into(),
            json!({
                "enabled": true,
                "network": {"require_bridged": false}
            }),
        );
        let cfg = build_config(&overrides, &default_project_root()).unwrap();
        assert!(cfg.device_enabled());

        overrides
            .as_object_mut()
            .unwrap()
            .insert("device".into(), json!({"enabled": false}));
        let cfg = build_config(&overrides, &default_project_root()).unwrap();
        assert!(!cfg.device_enabled());
    }

    #[test]
    fn os_and_sha512_leave_existing_fingerprints_alone() {
        let mut cfg = build_config(&json!({"name": "fp"}), &default_project_root()).unwrap();
        cfg.image_url = "https://example.test/ubuntu.img".into();
        cfg.image_sha256 = "a".repeat(64);
        let disk = cfg.disk_fingerprint();
        let seed = cfg.seed_fingerprint();
        assert_eq!(cfg.os, crate::os::DEFAULT);
        let mut other = cfg.clone();
        other.os = "ubuntu-26.04".into();
        other.image_sha256.clear();
        other.image_sha512 = "b".repeat(128);
        assert_ne!(other.disk_fingerprint(), disk);
        assert_ne!(other.seed_fingerprint(), seed);
        // Re-reading a stored config without `os` keeps Ubuntu 24.04.
        let stored = cfg.storage();
        let mut raw = stored.as_object().unwrap().clone();
        raw.remove("os");
        raw.remove("image_sha512");
        let again = Config::from_value(
            &Value::Object(raw),
            &default_project_root(),
            &cfg.home,
            Some(&cfg.name),
        )
        .unwrap();
        assert_eq!(again.os, crate::os::DEFAULT);
        assert_eq!(again.disk_fingerprint(), disk);
        assert_eq!(again.seed_fingerprint(), seed);
    }

    #[test]
    fn device_rename_keeps_legacy_fingerprint_keys() {
        let cfg = build_config(
            &json!({
                "name": "fingerprint-compat",
                "device": {
                    "enabled": true,
                    "network": {"require_bridged": false}
                }
            }),
            &default_project_root(),
        )
        .unwrap();
        let seed = cfg.seed_fingerprint_payload();
        assert!(seed.get("stealth_guest").is_some());
        assert!(seed.get("stealth_network").is_some());
        assert!(seed.get("stealth_devices").is_some());
        assert!(seed.get("device_guest").is_none());
        let fingerprint = cfg.fingerprint_payload();
        assert!(fingerprint.get("stealth").is_some());
        assert!(fingerprint.get("device").is_none());
    }

    #[test]
    fn existing_device_identities_survive_legacy_config_loading() {
        let home = std::env::temp_dir().join(format!(
            "vmctl-legacy-device-identity-{}-{}",
            std::process::id(),
            crate::util::localstamp()
        ));
        let cfg = build_config(
            &json!({
                "name": "legacy-device",
                "home": home,
                "device": {
                    "enabled": true,
                    "network": {"require_bridged": false}
                }
            }),
            &default_project_root(),
        )
        .unwrap();
        cfg.save_instance_meta().unwrap();

        let paths = StatePaths::new(&home).instance(&cfg.name);
        crate::util::ensure_dir(&paths.legacy_device()).unwrap();
        crate::util::atomic_write_json(
            &paths.legacy_device().join("smbios.json"),
            &json!({
                "smbios": {
                    "system": {
                        "serial": "SYS-EXISTING",
                        "uuid": "0fcbf329-d597-407e-90be-3efc03621893"
                    },
                    "baseboard": {
                        "serial": "BOARD-EXISTING",
                        "asset": "BOARD-ASSET"
                    },
                    "chassis": {
                        "serial": "CHASSIS-EXISTING",
                        "asset": "CHASSIS-ASSET"
                    },
                    "processor": {
                        "serial": "CPU-EXISTING",
                        "asset": "CPU-ASSET"
                    }
                }
            }),
            0o600,
        )
        .unwrap();
        crate::util::atomic_write_json(
            &paths.legacy_device().join("devices.json"),
            &json!({
                "devices": {
                    "storage": {"serial": "DISK-EXISTING"},
                    "optical": {"serial": "CD-EXISTING"}
                }
            }),
            0o600,
        )
        .unwrap();
        crate::util::atomic_write_json(
            &paths.legacy_device().join("network.json"),
            &json!({
                "network": {"mac": "00:1b:21:e9:86:64"}
            }),
            0o600,
        )
        .unwrap();

        let loaded = load_instance_config(&default_project_root(), &home, &cfg.name).unwrap();
        let resolved = loaded.device_resolved().unwrap();
        assert_eq!(resolved["smbios"]["system"]["serial"], "SYS-EXISTING");
        assert_eq!(
            resolved["smbios"]["system"]["uuid"],
            "0fcbf329-d597-407e-90be-3efc03621893"
        );
        assert_eq!(resolved["smbios"]["baseboard"]["serial"], "BOARD-EXISTING");
        assert_eq!(resolved["smbios"]["processor"]["asset"], "CPU-ASSET");
        assert_eq!(resolved["devices"]["storage"]["serial"], "DISK-EXISTING");
        assert_eq!(resolved["devices"]["optical"]["serial"], "CD-EXISTING");
        assert_eq!(resolved["network"]["mac"], "00:1b:21:e9:86:64");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn previous_device_artifact_directory_remains_in_use() {
        let home = std::env::temp_dir().join(format!(
            "vmctl-device-path-{}-{}",
            std::process::id(),
            crate::util::localstamp()
        ));
        let cfg = Config {
            home: home.clone(),
            name: "test".into(),
            ..Config::default()
        };
        let paths = StatePaths::new(&home).instance(&cfg.name);
        std::fs::create_dir_all(paths.legacy_device()).unwrap();
        assert_eq!(cfg.device_dir(), paths.legacy_device());
        assert_eq!(cfg.smbios_path(), paths.legacy_device().join("smbios.bin"));

        std::fs::create_dir_all(paths.device()).unwrap();
        assert_eq!(cfg.device_dir(), paths.device());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn configuration_validation_returns_errors_for_invalid_inputs_and_patterns() {
        let cfg = build_config(&json!({"name": "testvm"}), &default_project_root()).unwrap();
        let mut invalid = cfg.clone();
        invalid.name = "Bad Name".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("--name")
        );
        invalid = cfg.clone();
        invalid.username = "Bad User".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("--user")
        );
        invalid = cfg.clone();
        invalid.disk_size = "not-a-size".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("--disk-size")
        );
        invalid = cfg.clone();
        invalid.image_url = "https://example.test/image.img".into();
        invalid.image_sha256 = "bad".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("image_sha256")
        );
        invalid.image_sha256 = "a".repeat(64);
        invalid.image_sha512 = "b".repeat(128);
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("only one")
        );
        invalid.image_sha256.clear();
        assert!(invalid.validate().is_ok(), "a SHA-512 pin is valid");
        assert_eq!(invalid.image_checksum(), Checksum::Sha512("b".repeat(128)));
        invalid = cfg.clone();
        invalid.os = "windows-11".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unknown operating system")
        );
        invalid = cfg.clone();
        invalid.os = "debian-13".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("no longer supported")
        );
        invalid = cfg;
        invalid.desktop_resolution = "bad".into();
        assert!(
            invalid
                .validate()
                .unwrap_err()
                .to_string()
                .contains("--desktop-resolution")
        );
        assert!(validation_regex("(").is_err());
    }
}
