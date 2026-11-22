//! Browser fingerprint profile: CDP controller, JS overrides, atomic install.
//!
//! Branded Google Chrome ignores `--load-extension`, so the profile is applied
//! by a guest-side native CDP controller that attaches to a loopback
//! DevTools port.  The host only writes files and the guest verifies the
//! effective values.

use std::path::Path;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::config::Config;
use crate::paths::ResourcePaths;
use crate::util::{message, read_json, shell_quote as q};
use crate::vm::Runtime;

const INJECT: &[u8] = include_bytes!("../assets/browser_inject.js");
// Hash of the historical controller + injection + profile reference. Keep it
// after the Python assets are removed so old state can be recognized.
const LEGACY_CODE_HASH: &str = "b831a509eea044ae17c1c0ef4e356cd5012cb928e945515496c7d89b93222902";
const NATIVE_FINGERPRINT_VERSION: &[u8] = b"cabin-native-browser-v1\0";

pub fn load_defaults(project_root: &Path) -> Result<Value> {
    let path = ResourcePaths::new(project_root).browser_config();
    match read_json(&path) {
        Some(Value::Object(map)) if !map.is_empty() => Ok(Value::Object(map)),
        _ => message(format!(
            "missing browser profile defaults: {}",
            path.display()
        )),
    }
}

pub fn deep_merge(base: &Value, override_value: &Value) -> Value {
    crate::device::profile::deep_merge(base, override_value)
}

pub fn set_path(data: &mut Value, path: &str, value: Value) -> Result<()> {
    crate::device::profile::set_path(data, path, value)
}

pub fn enabled(cfg: &Config) -> bool {
    cfg.browser_enabled()
}

pub fn memory_bucket(memory_mb: u32) -> f64 {
    let gib = (memory_mb as f64 / 1024.0).max(0.25);
    for bucket in [0.25, 0.5, 1.0, 2.0, 4.0, 8.0] {
        if gib <= bucket {
            return bucket;
        }
    }
    8.0
}

pub fn bcp47_locale(value: &str) -> String {
    let text = if value.trim().is_empty() {
        "en-US".to_string()
    } else {
        value.trim().to_string()
    };
    if matches!(text.as_str(), "C" | "POSIX" | "C.UTF-8") {
        return "en-US".to_string();
    }
    let text = text.split('.').next().unwrap_or("").replace('_', "-");
    let parts: Vec<&str> = text.split('-').collect();
    if parts.len() >= 2 {
        return format!(
            "{}-{}",
            parts[0].to_lowercase(),
            parts[1..]
                .iter()
                .map(|part| if part.len() == 2 {
                    part.to_uppercase()
                } else {
                    part.to_string()
                })
                .collect::<Vec<_>>()
                .join("-")
        );
    }
    if parts.first().map(|part| !part.is_empty()).unwrap_or(false) {
        parts[0].to_lowercase()
    } else {
        "en-US".to_string()
    }
}

pub fn posix_locale(value: &str) -> String {
    let text = bcp47_locale(value);
    if let Some((lang, region)) = text.split_once('-') {
        return format!("{lang}_{region}.UTF-8");
    }
    if text == "en" {
        "en_US.UTF-8".to_string()
    } else {
        format!("{text}.UTF-8")
    }
}

fn ensure_object(data: &mut Value, key: &str) {
    if !data.get(key).map(Value::is_object).unwrap_or(false)
        && let Some(map) = data.as_object_mut()
    {
        map.insert(key.to_string(), Value::Object(Map::new()));
    }
}

fn object_mut<'a>(data: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>> {
    ensure_object(data, key);
    data.get_mut(key)
        .and_then(Value::as_object_mut)
        .ok_or_else(|| crate::Error::Message("browser profile must be an object".into()))
}

pub fn resolve(cfg: &Config) -> Result<Value> {
    let mut data = deep_merge(&load_defaults(&cfg.project_root)?, &cfg.browser);
    if !data.is_object() {
        return message("browser profile must be an object");
    }
    let seed = data
        .get("seed")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            let digest = crate::util::sha256_text(&format!("vmctl-browser:{}", cfg.name));
            digest[..16].to_string()
        });
    if let Some(map) = data.as_object_mut() {
        map.insert("seed".to_string(), json!(seed.clone()));
    }
    ensure_object(&mut data, "navigator");
    ensure_object(&mut data, "chrome");
    ensure_object(&mut data, "screen");
    if let Some(proxy) = cfg.guest_proxy_url() {
        let flags = object_mut(&mut data, "chrome")?
            .entry("flags".to_string())
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(items) = flags.as_array_mut() {
            let has_proxy = items.iter().any(|value| {
                value
                    .as_str()
                    .map(|item| item.starts_with("--proxy-server="))
                    .unwrap_or(false)
            });
            if !has_proxy {
                items.push(json!(format!("--proxy-server={proxy}")));
            }
        }
    }
    let sync = data
        .get("sync_with_system")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if sync {
        let geometry = cfg.display_geometry()?;
        let scale = geometry.scale();
        let (width, height) = geometry.logical_size();
        let (window_width, window_height) = geometry.chrome_window_size();
        let avail_height = height;
        {
            let navigator = object_mut(&mut data, "navigator")?;
            navigator.insert("hardware_concurrency".to_string(), json!(cfg.cpus));
            navigator.insert(
                "device_memory".to_string(),
                json!(memory_bucket(cfg.memory_mb)),
            );
            navigator.insert("platform".to_string(), json!("Linux aarch64"));
        }
        {
            let screen = object_mut(&mut data, "screen")?;
            screen.insert("width".to_string(), json!(width));
            screen.insert("height".to_string(), json!(height));
            screen.insert("avail_width".to_string(), json!(width));
            screen.insert("avail_height".to_string(), json!(avail_height));
            screen.insert("color_depth".to_string(), json!(24));
            screen.insert("pixel_depth".to_string(), json!(24));
            screen.insert("device_pixel_ratio".to_string(), json!(scale));
        }
        {
            let chrome = object_mut(&mut data, "chrome")?;
            chrome.insert(
                "window_size".to_string(),
                json!(format!("{window_width},{window_height}")),
            );
            chrome.insert("force_device_scale_factor".to_string(), Value::Null);
        }
        let configured_locale = data
            .get("locale")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| cfg.current_locale());
        let locale = bcp47_locale(&configured_locale);
        let timezone = data
            .get("timezone")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| cfg.current_timezone());
        if let Some(map) = data.as_object_mut() {
            map.insert("locale".to_string(), json!(locale));
            map.insert("timezone".to_string(), json!(timezone));
        }
        let mut languages = vec![locale.clone()];
        if locale.contains('-') {
            languages.push(locale.split('-').next().unwrap_or("").to_string());
        }
        {
            let navigator = object_mut(&mut data, "navigator")?;
            navigator.insert("language".to_string(), json!(locale));
            navigator.insert("languages".to_string(), json!(languages));
        }
        object_mut(&mut data, "chrome")?.insert("lang".to_string(), json!(locale));
    } else {
        let timezone = data
            .get("timezone")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| cfg.current_timezone());
        let configured_locale = data
            .get("locale")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| cfg.current_locale());
        if let Some(map) = data.as_object_mut() {
            map.insert("timezone".to_string(), json!(timezone));
            map.insert(
                "locale".to_string(),
                json!(bcp47_locale(&configured_locale)),
            );
        }
    }
    let canvas_seed = data
        .get("seed")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    {
        let canvas = object_mut(&mut data, "canvas")?;
        let has_seed = canvas
            .get("seed")
            .and_then(Value::as_str)
            .map(|value| !value.is_empty())
            .unwrap_or(false);
        if !has_seed {
            canvas.insert("seed".to_string(), json!(canvas_seed));
        }
    }
    let user_data = data
        .get("user_data_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            format!(
                "/home/{}/.config/google-chrome-vmctl",
                cfg.current_username()
            )
        });
    let username = cfg.current_username();
    if let Some(map) = data.as_object_mut() {
        map.insert("user_data_dir".to_string(), json!(user_data));
        map.insert(
            "resolved".to_string(),
            json!({
                "name": cfg.name,
                "username": username,
                "home": format!("/home/{username}"),
                "seed": seed,
            }),
        );
    }
    Ok(data)
}

/// Keep the generated profile compatible with Chrome's canonical UTC name.
/// The in-memory profile remains unchanged so existing fingerprint contracts
/// do not drift for an alias with identical semantics.
pub fn install_profile(data: &Value) -> Value {
    let mut profile = data.clone();
    let is_utc_alias = profile
        .get("timezone")
        .and_then(Value::as_str)
        .is_some_and(|timezone| {
            matches!(
                timezone,
                "Etc/UTC"
                    | "Etc/GMT"
                    | "Etc/GMT+0"
                    | "Etc/GMT-0"
                    | "Etc/GMT0"
                    | "Etc/UCT"
                    | "Etc/Universal"
                    | "Etc/Zulu"
                    | "GMT"
                    | "GMT0"
                    | "Greenwich"
                    | "UCT"
                    | "Universal"
                    | "Zulu"
            )
        });
    if is_utc_alias && let Some(profile) = profile.as_object_mut() {
        profile.insert("timezone".to_string(), json!("UTC"));
    }
    profile
}

fn fingerprint_with_code(cfg: &Config, code_hash: &str) -> Result<String> {
    let mut data = resolve(cfg)?;
    if let Some(map) = data.as_object_mut() {
        map.remove("resolved");
        map.insert("_code".to_string(), json!(code_hash));
    }
    Ok(crate::util::sha256_text(&crate::py_json::dumps_compact(&data)))
}

fn native_code_hash(inject: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(NATIVE_FINGERPRINT_VERSION);
    hasher.update(inject);
    crate::util::hex_lower(&hasher.finalize())
}

/// The old hash is recognized for migration, but never marks a native install
/// as current. The native version also changes whenever the injection changes.
pub fn legacy_fingerprint(cfg: &Config) -> Result<String> {
    fingerprint_with_code(cfg, LEGACY_CODE_HASH)
}

pub fn fingerprint(cfg: &Config) -> Result<String> {
    fingerprint_with_code(cfg, &native_code_hash(INJECT))
}

/// The managed browser's editable settings, as its panel shows them.
///
/// With `follow_machine` the time zone, language, CPU and memory hints,
/// platform and screen come from the machine (and keep following it);
/// otherwise the values here are used as given. The user agent is always
/// the configured one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub follow_machine: bool,
    pub timezone: String,
    pub locale: String,
    pub user_agent: String,
    pub platform: String,
    pub hardware_concurrency: u32,
    pub device_memory: f64,
}

/// The effective settings (after following the machine, when it does).
pub fn settings(cfg: &Config) -> Result<Settings> {
    let data = resolve(cfg)?;
    let text = |pointer: &str| data.pointer(pointer).and_then(Value::as_str).unwrap_or("").to_string();
    Ok(Settings {
        follow_machine: data.get("sync_with_system").and_then(Value::as_bool).unwrap_or(true),
        timezone: text("/timezone"),
        locale: text("/locale"),
        user_agent: text("/navigator/user_agent"),
        platform: text("/navigator/platform"),
        hardware_concurrency: data.pointer("/navigator/hardware_concurrency").and_then(Value::as_u64).unwrap_or(0) as u32,
        device_memory: data.pointer("/navigator/device_memory").and_then(Value::as_f64).unwrap_or(0.0),
    })
}

/// A copy of `cfg` with `settings` applied to its browser profile. Fields the
/// machine provides while following it are left to the machine.
pub fn with_settings(cfg: &Config, settings: &Settings) -> Result<Config> {
    let user_agent = settings.user_agent.trim();
    if user_agent.is_empty() || user_agent.len() > 512 || user_agent.chars().any(char::is_control) {
        return message("The user agent must be one line of up to 512 characters");
    }
    let mut updated = cfg.clone();
    let browser = &mut updated.browser;
    if !browser.is_object() {
        *browser = json!({});
    }
    set_path(browser, "sync_with_system", json!(settings.follow_machine))?;
    set_path(browser, "navigator.user_agent", json!(user_agent))?;
    if settings.follow_machine {
        // The machine's current values; following keeps them current.
        set_path(browser, "timezone", json!(cfg.current_timezone()))?;
        set_path(browser, "locale", json!(cfg.current_locale()))?;
    } else {
        let timezone = settings.timezone.trim();
        let locale = settings.locale.trim();
        let platform = settings.platform.trim();
        if timezone.is_empty() || !timezone.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c)) {
            return message("Choose a time zone such as Europe/Berlin");
        }
        if locale.is_empty() || !locale.chars().all(|c| c.is_ascii_alphanumeric() || "_-.@".contains(c)) {
            return message("Choose a language such as en-US");
        }
        if platform.is_empty() || platform.len() > 64 || platform.chars().any(char::is_control) {
            return message("The platform must be a short name such as Win32 or MacIntel");
        }
        if !(1..=128).contains(&settings.hardware_concurrency) {
            return message("CPU cores must be between 1 and 128");
        }
        if ![0.25, 0.5, 1.0, 2.0, 4.0, 8.0].contains(&settings.device_memory) {
            return message("Device memory must be one of 0.25, 0.5, 1, 2, 4 or 8 GB");
        }
        set_path(browser, "timezone", json!(timezone))?;
        set_path(browser, "locale", json!(locale))?;
        set_path(browser, "navigator.platform", json!(platform))?;
        set_path(browser, "navigator.hardware_concurrency", json!(settings.hardware_concurrency))?;
        // Whole buckets are stored as integers, like the defaults, so an
        // unchanged value hashes the same.
        let memory = settings.device_memory;
        let memory = if memory.fract() == 0.0 { json!(memory as i64) } else { json!(memory) };
        set_path(browser, "navigator.device_memory", memory)?;
    }
    Ok(updated)
}

/// The profile without its region (time zone, language and the values
/// derived from them). Two configurations with the same identity differ
/// only in what `sync_environment` can change live; anything else needs the
/// profile written again.
pub fn identity_fingerprint(cfg: &Config) -> Result<String> {
    let mut data = resolve(cfg)?;
    if let Some(map) = data.as_object_mut() {
        map.remove("resolved");
        map.remove("timezone");
        map.remove("locale");
    }
    if let Some(navigator) = data.get_mut("navigator").and_then(Value::as_object_mut) {
        navigator.remove("language");
        navigator.remove("languages");
    }
    if let Some(chrome) = data.get_mut("chrome").and_then(Value::as_object_mut) {
        chrome.remove("lang");
    }
    Ok(crate::util::sha256_text(&crate::py_json::dumps_compact(&data)))
}

pub fn validate(data: &Value, cfg: &Config) -> Result<()> {
    let Some(map) = data.as_object() else {
        return message("browser profile must be an object");
    };
    if !map.get("enabled").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(());
    }
    if cfg.desktop_env.is_none() {
        return message(
            "browser fingerprinting requires --desktop sway (Chrome must be installed)",
        );
    }
    let start_url = map.get("start_url").and_then(Value::as_str).unwrap_or("");
    if !(start_url.starts_with("http://")
        || start_url.starts_with("https://")
        || start_url == "about:blank")
    {
        return message("browser.start_url must be an http(s) URL or about:blank");
    }
    let port = map
        .get("remote_debugging_port")
        .and_then(Value::as_i64)
        .unwrap_or(9222);
    if !(1024..=65535).contains(&port) {
        return message("browser.remote_debugging_port must be between 1024 and 65535");
    }
    let navigator = map
        .get("navigator")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in ["user_agent", "platform", "vendor"] {
        match navigator.get(key).and_then(Value::as_str) {
            Some(value) if !value.is_empty() => {}
            _ => {
                return message(format!(
                    "browser.navigator.{key} must be a non-empty string"
                ));
            }
        }
    }
    match navigator.get("languages").and_then(Value::as_array) {
        Some(items) if !items.is_empty() && items.iter().all(|item| item.as_str().is_some()) => {}
        _ => return message("browser.navigator.languages must be a non-empty list of strings"),
    }
    match navigator
        .get("hardware_concurrency")
        .and_then(Value::as_i64)
    {
        Some(value) if (1..=256).contains(&value) => {}
        _ => return message("browser.navigator.hardware_concurrency must be 1..256"),
    }
    match navigator.get("device_memory").and_then(Value::as_f64) {
        Some(value) if (0.25..=1024.0).contains(&value) => {}
        _ => return message("browser.navigator.device_memory must be 0.25..1024"),
    }
    let screen = map
        .get("screen")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in [
        "width",
        "height",
        "avail_width",
        "avail_height",
        "color_depth",
        "pixel_depth",
        "device_pixel_ratio",
    ] {
        match screen.get(key).and_then(Value::as_f64) {
            Some(value) if value > 0.0 => {}
            _ => return message(format!("browser.screen.{key} must be positive")),
        }
    }
    let webgl = map
        .get("webgl")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in ["vendor", "renderer", "version", "shading_language_version"] {
        if webgl.get(key).and_then(Value::as_str).is_none() {
            return message(format!("browser.webgl.{key} must be a string"));
        }
    }
    let parameters = webgl
        .get("parameters")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, value) in parameters {
        let parsed: u32 = key.parse().unwrap_or(u32::MAX);
        if parsed > 65535 {
            return message(format!(
                "browser.webgl.parameters key {key:?} must be a WebGL enum"
            ));
        }
        if !(value.is_number() || value.is_array()) {
            return message(format!(
                "browser.webgl.parameters[{key}] must be a number or array"
            ));
        }
    }
    if !map
        .get("sync_with_system")
        .map(Value::is_boolean)
        .unwrap_or(true)
    {
        return message("browser.sync_with_system must be true or false");
    }
    let canvas = map
        .get("canvas")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    match canvas.get("noise").and_then(Value::as_f64) {
        None => {}
        Some(value) if (0.0..=32.0).contains(&value) => {}
        _ => return message("browser.canvas.noise must be 0..32"),
    }
    let audio = map
        .get("audio")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    match audio.get("noise").and_then(Value::as_f64) {
        None => {}
        Some(value) if (0.0..=1.0).contains(&value) => {}
        _ => return message("browser.audio.noise must be 0..1"),
    }
    if let Some(rate) = audio.get("sample_rate").filter(|value| !value.is_null()) {
        match rate.as_i64() {
            Some(value) if (8000..=192000).contains(&value) => {}
            _ => return message("browser.audio.sample_rate must be 8000..192000"),
        }
    }
    match map.get("extra_js") {
        None => {}
        Some(Value::String(text)) if text.len() <= 200000 => {}
        _ => return message("browser.extra_js must be a string under 200000 bytes"),
    }
    if !map
        .get("chrome")
        .and_then(|value| value.get("flags"))
        .map(Value::is_array)
        .unwrap_or(true)
    {
        return message("browser.chrome.flags must be a list");
    }
    if !map.get("policy").map(Value::is_object).unwrap_or(true) {
        return message("browser.policy must be an object");
    }
    let webrtc = map
        .get("webrtc")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let policy = webrtc
        .get("policy")
        .and_then(Value::as_str)
        .unwrap_or("default");
    if !matches!(
        policy,
        "default" | "default_public_interface_only" | "disable_non_proxied_udp"
    ) {
        return message("browser.webrtc.policy must be a Chrome WebRtcIPHandlingPolicy value");
    }
    Ok(())
}

pub fn insert_script(data: &Value) -> String {
    let template = String::from_utf8_lossy(INJECT).into_owned();
    let extra = data.get("extra_js").and_then(Value::as_str).unwrap_or("");
    format!(
        "{}\n{}",
        template.replace(
            "__VMCTL_BROWSER_PROFILE__",
            &crate::py_json::dumps_default(data, true)
        ),
        extra
    )
}

/// Atomically update the generated injection used by an existing machine.
/// The native controller is the deployed guest binary; the browser profile
/// belongs to the display transaction and is deliberately not part of this
/// script.
pub fn runtime_install_script(data: &Value) -> String {
    let inject = insert_script(data);
    format!(
        "set -euo pipefail\n\
         umask 077\n\
         runtime_tmp=\"$(mktemp -d)\"\n\
         cleanup_runtime() {{ sudo -n rm -rf \"$runtime_tmp\"; sudo -n rm -f /opt/vmctl-fp/.inject.js.new; }}\n\
         trap cleanup_runtime EXIT\n\
         printf '%s' {inject_b64} | base64 -d > \"$runtime_tmp/inject.js\"\n\
         chmod 0644 \"$runtime_tmp/inject.js\"\n\
         sudo -n install -d -m 0755 /opt/vmctl-fp\n\
         sudo -n install -m 0644 \"$runtime_tmp/inject.js\" /opt/vmctl-fp/.inject.js.new\n\
         sudo -n mv -f /opt/vmctl-fp/.inject.js.new /opt/vmctl-fp/inject.js\n\
         sudo -n rm -f /opt/vmctl-fp/controller.py\n",
        inject_b64 = q(&b64(inject.as_bytes())),
    )
}

pub fn install_script(cfg: &Config, data: &Value) -> Result<String> {
    let user = cfg.current_username();
    let home = format!("/home/{user}");
    let geometry = cfg.display_geometry()?;
    let (width, height) = geometry.physical_size();
    let scale = geometry.scale();
    let profile_timezone = data
        .get("timezone")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| cfg.current_timezone());
    let profile_locale = posix_locale(
        data.get("locale")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(&cfg.current_locale()),
    );
    let config_json = format!("{}\n", crate::py_json::dumps_pretty_with(data, true));
    let inject = insert_script(data);
    let helper = crate::agentd::HELPER_BIN;
    let policy = format!(
        "{}\n",
        crate::py_json::dumps_pretty_with(data.get("policy").unwrap_or(&Value::Null), true)
    );
    let screen_probe = format!("{} guest-desktop screen", helper);
    let start = format!("#!/bin/sh\n# Keep one managed Chrome/controller alive if Chrome is closed or crashes.\nexport DISPLAY=\"${{DISPLAY:-:0}}\"\nwhile :; do\n  {} browser run\n  code=$?\n  [ \"$code\" -eq 75 ] && exit 0\n  sleep 2\ndone\n", helper);
    let expected_screen = format!("{width}x{height}@{}", crate::py_json::py_float_repr(scale));
    Ok(format!(
        "set -euo pipefail\nUSER_NAME={user_q}\nHOME_DIR={home_q}\nsay() {{ printf '\n[fp] %s\n' \"$*\"; }}\ninstall -d -m 0755 /opt/vmctl-fp\nprintf '%s' {config_b64} | base64 -d > /opt/vmctl-fp/config.json\nprintf '%s' {inject_b64} | base64 -d > /opt/vmctl-fp/inject.js\nchmod 0644 /opt/vmctl-fp/config.json /opt/vmctl-fp/inject.js\ninstall -d -m 0755 /etc/opt/chrome/policies/managed\nprintf '%s' {policy_b64} | base64 -d > /etc/opt/chrome/policies/managed/vmctl-fp.json\nchmod 0644 /etc/opt/chrome/policies/managed/vmctl-fp.json\nsay 'aligning guest timezone/locale to the browser profile'\ntimedatectl set-timezone {profile_tz_q}\nprintf '%s\n' {profile_tz_q} > /etc/timezone\n[ -f /etc/locale.gen ] || touch /etc/locale.gen\ngrep -q '^{profile_locale}[[:space:]]' /etc/locale.gen 2>/dev/null || printf '%s UTF-8\n' {profile_locale_q} >> /etc/locale.gen\nlocale-gen >/dev/null\nupdate-locale LANG={profile_locale_q} LC_ALL={profile_locale_q} LANGUAGE={profile_locale_q}\ntest \"$(timedatectl show -p Timezone --value)\" = {profile_tz_q}\nsay 'checking browser-visible resources against the guest'\ntest \"$(nproc)\" -eq {cpus}\nsway_msg() {{ runuser -u {user_q} -- env XDG_RUNTIME_DIR=/run/user/$(id -u {user_q}) SWAYSOCK=$(ls /run/user/$(id -u {user_q})/sway-ipc.* 2>/dev/null | head -1) swaymsg \"$@\"; }}\nprobe_screen() {{ sway_msg -t get_outputs -r 2>/dev/null | {screen_probe} 2>/dev/null || true; }}\nACTUAL_SCREEN=\"$(probe_screen)\"\nif [ -n \"$ACTUAL_SCREEN\" ] && [ \"$ACTUAL_SCREEN\" != \"{expected_screen}\" ]; then\n  say \"Sway output is $ACTUAL_SCREEN; applying the configured {expected_screen}\"\n  OUTPUT_NAME=\"$(sway_msg -t get_outputs -r 2>/dev/null | {helper} guest-desktop output-name 2>/dev/null || true)\"\n  if [ -n \"$OUTPUT_NAME\" ]; then\n    sway_msg output \"$OUTPUT_NAME\" mode {width}x{height} >/dev/null 2>&1 || true\n    sway_msg output \"$OUTPUT_NAME\" scale {scale_arg} >/dev/null 2>&1 || true\n    sleep 1\n  fi\n  ACTUAL_SCREEN=\"$(probe_screen)\"\n  if [ \"$ACTUAL_SCREEN\" != \"{expected_screen}\" ]; then\n    echo \"Sway output is $ACTUAL_SCREEN, expected {expected_screen}\" >&2\n    sway_msg -t get_outputs -r >&2 || true\n    exit 41\n  fi\nfi\ninstall -d -o {user_q} -g {user_q} {home_sway_q}\nprintf '%s' {start_b64} | base64 -d > {start_path_q}\nchmod 0755 {start_path_q}\nchown {user_q}:{user_q} {start_path_q}\nsay 'verifying fingerprint injection with headless Chrome'\nrunuser -u {user_q} -- env HOME={home_q} {helper} browser verify\nsay 'browser fingerprint profile installed'\n",
        user_q = q(&user),
        home_q = q(&home),
        config_b64 = q(&b64(config_json.as_bytes())),
        inject_b64 = q(&b64(inject.as_bytes())),
        policy_b64 = q(&b64(policy.as_bytes())),
        helper = q(helper),
        profile_tz_q = q(&profile_timezone),
        profile_locale_q = q(&profile_locale),
        profile_locale = profile_locale,
        cpus = cfg.cpus,
        screen_probe = screen_probe,
        expected_screen = expected_screen,
        width = width,
        height = height,
        scale_arg = crate::py_json::py_float_repr(scale),
        home_sway_q = q(&format!("{home}/.config/sway")),
        start_b64 = q(&b64(start.as_bytes())),
        start_path_q = q(&format!("{home}/.config/sway/start-chrome.sh")),
    ))
}

pub fn verify_script(cfg: &Config, data: Option<&Value>) -> Result<String> {
    let resolved_owned;
    let data = match data {
        Some(data) => data,
        None => {
            resolved_owned = resolve(cfg)?;
            &resolved_owned
        }
    };
    let timezone = data
        .get("timezone")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| cfg.current_timezone());
    let locale = posix_locale(
        data.get("locale")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(&cfg.current_locale()),
    );
    Ok(format!(
        "set -e\ntest -x {helper}\ntest -f /opt/vmctl-fp/config.json\ntest -f /opt/vmctl-fp/inject.js\ntest -f /etc/opt/chrome/policies/managed/vmctl-fp.json\n{helper} guest-system validate-json /opt/vmctl-fp/config.json\n{helper} guest-system validate-json /etc/opt/chrome/policies/managed/vmctl-fp.json\ncommand -v google-chrome-stable >/dev/null\ntest \"$(timedatectl show -p Timezone --value)\" = {tz}\ngrep -q '^LANG={locale}$' /etc/default/locale\ntest \"$(nproc)\" -eq {cpus}\n",
        helper = q(crate::agentd::HELPER_BIN),
        tz = q(&timezone),
        cpus = cfg.cpus
    ))
}

/// Existing machines keep their own timezone and locale. The new-machine
/// pipeline retains the historical alignment behavior above.
pub fn install_script_preserving_system(cfg: &Config, data: &Value) -> Result<String> {
    let mut script = install_script(cfg, data)?;
    let start = script
        .find("say 'aligning guest timezone/locale to the browser profile'\n")
        .ok_or_else(|| crate::Error::Message("Browser alignment section is missing".into()))?;
    let end = script[start..]
        .find("say 'checking browser-visible resources against the guest'\n")
        .map(|offset| start + offset)
        .ok_or_else(|| crate::Error::Message("Browser verification section is missing".into()))?;
    script.replace_range(start..end, "");
    Ok(script)
}

pub fn verify_script_preserving_system(cfg: &Config, data: &Value) -> Result<String> {
    let script = verify_script(cfg, Some(data))?;
    let filtered: Vec<&str> = script
        .lines()
        .filter(|line| {
            !line.starts_with("test \"$(timedatectl show -p Timezone")
                && !line.starts_with("grep -q '^LANG=")
        })
        .collect();
    Ok(filtered.join("\n") + "\n")
}

pub fn summary(cfg: &Config, data: Option<&Value>) -> Result<Value> {
    let resolved_owned;
    let data = match data {
        Some(data) => data,
        None => {
            resolved_owned = resolve(cfg)?;
            &resolved_owned
        }
    };
    Ok(json!({
        "enabled": data.get("enabled").and_then(Value::as_bool).unwrap_or(false),
        "fingerprint": fingerprint(cfg)?,
        "seed": data.get("seed").cloned().unwrap_or(Value::Null),
        "timezone": data.get("timezone").cloned().unwrap_or(Value::Null),
        "locale": data.get("locale").cloned().unwrap_or(Value::Null),
        "hardware_concurrency": data.get("navigator").and_then(|value| value.get("hardware_concurrency")).cloned().unwrap_or(Value::Null),
        "device_memory": data.get("navigator").and_then(|value| value.get("device_memory")).cloned().unwrap_or(Value::Null),
        "webgl_vendor": data.get("webgl").and_then(|value| value.get("vendor")).cloned().unwrap_or(Value::Null),
        "webgl_renderer": data.get("webgl").and_then(|value| value.get("renderer")).cloned().unwrap_or(Value::Null),
        "canvas_noise": data.get("canvas").and_then(|value| value.get("noise")).cloned().unwrap_or(Value::Null),
        "audio_noise": data.get("audio").and_then(|value| value.get("noise")).cloned().unwrap_or(Value::Null),
        "webrtc_policy": data.get("webrtc").and_then(|value| value.get("policy")).cloned().unwrap_or(Value::Null),
        "chrome_flags": data.get("chrome").and_then(|value| value.get("flags")).cloned().unwrap_or(Value::Null),
    }))
}

fn b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn sync_runtime_controller(cfg: &Config) -> Result<()> {
    let runtime = Runtime::new(cfg);
    if !runtime.running() {
        return message("Start the machine before checking the browser environment");
    }
    let profile = install_profile(&resolve(cfg)?);
    let (code, output) = runtime.ssh_capture(
        &runtime_install_script(&profile),
        std::time::Duration::from_secs(30),
    );
    if code != 0 {
        return message(format!(
            "The browser environment check could not update its runtime: {}",
            output.trim()
        ));
    }
    Ok(())
}

fn expected_environment(profile: &Value) -> Value {
    let navigator = profile
        .get("navigator")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let languages = navigator
        .get("languages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "timezone": profile.get("timezone").cloned().unwrap_or(Value::Null),
        "language": navigator
            .get("language")
            .cloned()
            .or_else(|| languages.first().cloned())
            .unwrap_or(Value::Null),
        "languages": languages,
        "platform": navigator.get("platform").cloned().unwrap_or(Value::Null),
    })
}

fn same_environment(left: &Value, right: &Value) -> bool {
    let field = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
    };
    ["timezone", "language", "platform"]
        .into_iter()
        .all(|key| field(left, key) == field(right, key))
        && left.get("languages") == right.get("languages")
}

/// The browser environment as the network check needs it: the configured
/// values, the guest's installed profile (`installed`, `error` when it is
/// unreadable or differs) and the live Chrome (`live`, `observed`,
/// `liveDetail`). Only the installed part can block the network; a closed or
/// sleeping Chrome is a normal state. No caller input reaches the command,
/// and the live observation only reads built-in page values.
pub fn audit_environment(cfg: &Config) -> Result<Value> {
    if !cfg.browser_enabled() {
        return Ok(json!({
            "enabled": false,
            "expected": {},
            "installed": null,
            "live": "notRunning",
            "observed": null,
            "liveDetail": null,
            "error": null,
        }));
    }
    let profile = install_profile(&resolve(cfg)?);
    let expected = expected_environment(&profile);
    // Keeps Chrome's page script current. A failure here affects the live
    // browser only, so it is reported as a live note, never as a blocker.
    let runtime_note = sync_runtime_controller(cfg).err().map(|error| error.to_string());
    let runtime = Runtime::new(cfg);
    let (code, output) = runtime.ssh_capture(
        &format!("{} browser audit", crate::agentd::HELPER_BIN),
        std::time::Duration::from_secs(12),
    );
    if code != 0 {
        let reason = output.trim();
        return Ok(json!({
            "enabled": true,
            "expected": expected,
            "installed": null,
            "live": "unavailable",
            "observed": null,
            "liveDetail": runtime_note,
            "error": if reason.is_empty() {
                "The installed browser settings could not be read".to_string()
            } else {
                format!("The installed browser settings could not be read: {reason}")
            },
        }));
    }
    let report: Value = serde_json::from_str(output.trim()).map_err(|error| {
        crate::Error::Message(format!(
            "The browser environment audit returned invalid JSON: {error}"
        ))
    })?;
    Ok(audit_result(expected, &report, runtime_note))
}

/// Combine the configured values with the guest's report (pure, tested).
fn audit_result(expected: Value, report: &Value, runtime_note: Option<String>) -> Value {
    let installed = report.get("installed").cloned().unwrap_or(Value::Null);
    let error = if installed.is_null() {
        Some("The guest did not report its installed browser settings")
    } else if !same_environment(&expected, &installed) {
        Some("The installed browser settings differ from the current settings")
    } else {
        None
    };
    let mut live = report.get("live").cloned().unwrap_or_else(|| json!("unavailable"));
    let mut observed = report.get("observed").cloned().unwrap_or(Value::Null);
    let mut live_detail = report.get("liveDetail").cloned().unwrap_or(Value::Null);
    if let Some(note) = runtime_note {
        if live == json!("observed") {
            // The values seen are real; the note explains a possible drift.
            live_detail = json!(note);
        } else {
            live = json!("unavailable");
            observed = Value::Null;
            live_detail = json!(note);
        }
    }
    json!({
        "enabled": true,
        "expected": expected,
        "installed": installed,
        "live": live,
        "observed": observed,
        "liveDetail": live_detail,
        "error": error,
    })
}

#[cfg(test)]
mod tests {
    use super::{Settings, audit_result, identity_fingerprint, install_profile, object_mut, settings, with_settings};
    use serde_json::json;

    fn env(timezone: &str) -> serde_json::Value {
        json!({"timezone": timezone, "language": "en-US", "languages": ["en-US", "en"], "platform": "Win32"})
    }

    fn browser_config() -> crate::config::Config {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        crate::config::build_config(
            &json!({
                "desktop_env": "sway",
                "desktop_chrome": true,
                "desktop_resolution": "1600x900",
                "browser": {"enabled": true, "sync_with_system": false, "timezone": "Asia/Tokyo", "locale": "ja-JP"}
            }),
            root,
        )
        .unwrap()
    }

    #[test]
    fn a_region_change_keeps_the_identity_and_a_user_agent_change_does_not() {
        let cfg = browser_config();
        let current = settings(&cfg).unwrap();
        assert!(!current.follow_machine);
        assert_eq!(current.timezone, "Asia/Tokyo");

        let region = with_settings(&cfg, &Settings { timezone: "Europe/Berlin".into(), locale: "de-DE".into(), ..current.clone() }).unwrap();
        assert_eq!(settings(&region).unwrap().timezone, "Europe/Berlin");
        assert_eq!(identity_fingerprint(&cfg).unwrap(), identity_fingerprint(&region).unwrap());

        let agent = with_settings(&cfg, &Settings { user_agent: "Mozilla/5.0 Test".into(), ..current }).unwrap();
        assert_ne!(identity_fingerprint(&cfg).unwrap(), identity_fingerprint(&agent).unwrap());
    }

    #[test]
    fn settings_are_validated() {
        let cfg = browser_config();
        let current = settings(&cfg).unwrap();
        assert!(with_settings(&cfg, &Settings { user_agent: "two\nlines".into(), ..current.clone() }).is_err());
        assert!(with_settings(&cfg, &Settings { timezone: "Europe/Berlin; rm -rf".into(), ..current.clone() }).is_err());
        assert!(with_settings(&cfg, &Settings { device_memory: 3.0, ..current.clone() }).is_err());
        assert!(with_settings(&cfg, &Settings { hardware_concurrency: 0, ..current }).is_err());
    }

    #[test]
    fn a_sleeping_browser_is_a_live_state_not_an_error() {
        let report = json!({"installed": env("Asia/Tokyo"), "live": "idle", "observed": null, "liveDetail": "Chrome's pages are asleep"});
        let result = audit_result(env("Asia/Tokyo"), &report, None);
        assert_eq!(result["error"], serde_json::Value::Null);
        assert_eq!(result["live"], "idle");
        assert_eq!(result["liveDetail"], "Chrome's pages are asleep");
    }

    #[test]
    fn installed_settings_that_differ_are_the_blocking_error() {
        let report = json!({"installed": env("Europe/Berlin"), "live": "observed", "observed": env("Europe/Berlin")});
        let result = audit_result(env("Asia/Tokyo"), &report, None);
        assert!(result["error"].as_str().unwrap().contains("differ"));
        // The live values are passed through unjudged.
        assert_eq!(result["observed"]["timezone"], "Europe/Berlin");
    }

    #[test]
    fn a_runtime_refresh_failure_only_affects_the_live_part() {
        let report = json!({"installed": env("Asia/Tokyo"), "live": "notRunning", "observed": null});
        let result = audit_result(env("Asia/Tokyo"), &report, Some("sudo failed".into()));
        assert_eq!(result["error"], serde_json::Value::Null);
        assert_eq!(result["live"], "unavailable");
        assert_eq!(result["liveDetail"], "sudo failed");
    }

    #[test]
    fn utc_aliases_are_normalized_only_for_profile_installation() {
        let profile = json!({"timezone": "Etc/UTC", "locale": "en-US"});
        assert_eq!(install_profile(&profile)["timezone"], "UTC");
        assert_eq!(profile["timezone"], "Etc/UTC");

        let profile = json!({"timezone": "Asia/Shanghai"});
        assert_eq!(install_profile(&profile)["timezone"], "Asia/Shanghai");
    }

    #[test]
    fn object_access_rejects_a_non_object_root() {
        let mut value = json!(true);
        assert!(object_mut(&mut value, "chrome").is_err());
        let mut value = json!({"chrome": 1});
        object_mut(&mut value, "chrome")
            .unwrap()
            .insert("enabled".into(), json!(true));
        assert_eq!(value["chrome"]["enabled"], true);
    }

    #[test]
    fn existing_machine_browser_setup_does_not_rewrite_system_locale_or_timezone() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg = crate::config::build_config(
            &json!({"desktop_env":"sway", "desktop_chrome":true, "browser":{"enabled":true}}),
            root,
        )
        .unwrap();
        let profile = super::install_profile(&super::resolve(&cfg).unwrap());
        let script = super::install_script_preserving_system(&cfg, &profile).unwrap();
        assert!(!script.contains("timedatectl set-timezone"));
        assert!(!script.contains("update-locale"));
        assert!(script.contains("browser fingerprint profile installed"));
        assert!(!script.contains("python3"));
        let verify = super::verify_script_preserving_system(&cfg, &profile).unwrap();
        assert!(!verify.contains("/etc/default/locale"));
        assert!(verify.contains("command -v google-chrome-stable"));
    }

    #[test]
    fn browser_geometry_matches_the_compositor_scale() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg = crate::config::build_config(
            &json!({
                "desktop_env": "sway",
                "desktop_chrome": true,
                "desktop_resolution": "1920x1080",
                "desktop_scale": 2.0,
                "browser": {"enabled": true}
            }),
            root,
        )
        .unwrap();
        let profile = super::resolve(&cfg).unwrap();
        assert_eq!(profile["screen"]["width"], 960);
        assert_eq!(profile["screen"]["height"], 540);
        assert_eq!(profile["screen"]["device_pixel_ratio"], 2.0);
        assert_eq!(profile["chrome"]["window_size"], "960,512");
        assert_eq!(profile["chrome"]["force_device_scale_factor"], json!(null));
    }
    #[test]
    fn the_install_script_applies_the_configured_screen_and_is_valid_shell() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let cfg = crate::config::build_config(
            &json!({
                "desktop_env": "sway",
                "desktop_chrome": true,
                "desktop_resolution": "1600x900",
                "desktop_scale": 1.0,
                "browser": {"enabled": true}
            }),
            root,
        )
        .unwrap();
        let profile = super::resolve(&cfg).unwrap();
        let data = super::install_profile(&profile);
        let script = super::install_script(&cfg, &data).unwrap();
        // The configured geometry is applied before it is verified, and a failure shows every output.
        assert!(script.contains("mode 1600x900"));
        assert!(script.contains("scale 1.0"));
        assert!(script.contains("expected 1600x900@1.0"));
        assert!(script.contains("get_outputs -r >&2"));
        let path = std::env::temp_dir().join(format!("cabin-install-{}.sh", std::process::id()));
        std::fs::write(&path, &script).unwrap();
        let status = std::process::Command::new("bash").arg("-n").arg(&path).status().unwrap();
        let _ = std::fs::remove_file(&path);
        assert!(status.success(), "the generated install script is not valid shell");
    }

}
