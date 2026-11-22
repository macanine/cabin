use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

use crate::{Error, Result};

const BEGIN: &str = "# BEGIN Cabin native display";
const END: &str = "# END Cabin native display";
const VERSION: u32 = 2;
pub(super) const LEGACY_FILES: [&str; 6] = [
    "original-config",
    "original-controller",
    "original-inject",
    "managed-config",
    "managed-controller",
    "managed-inject",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(super) struct Output {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl Output {
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty()
            || !self
                .name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
        {
            return Err(Error::invalid("The desktop output name is not supported."));
        }
        if self.width == 0
            || self.height == 0
            || self.width > u16::MAX.into()
            || self.height > u16::MAX.into()
            || !self.scale.is_finite()
            || self.scale <= 0.0
        {
            return Err(Error::invalid("The desktop output geometry is invalid."));
        }
        Ok(())
    }

    pub fn logical_size(&self) -> (u32, u32) {
        let logical = |physical| ((f64::from(physical) / self.scale) as u32).max(1);
        (logical(self.width), logical(self.height))
    }

    pub fn same_mode(&self, other: &Self) -> bool {
        self.name == other.name && self.width == other.width && self.height == other.height
    }

    pub fn dimensions(&self) -> String {
        format!("{}x{}", self.width, self.height)
    }

    pub fn mode_args(&self) -> Vec<String> {
        if self.name == "Virtual-1" {
            // virtio-gpu's DRM output supports custom modes on the bundled QEMU.
            vec!["mode".into(), "--custom".into(), self.dimensions()]
        } else {
            vec!["resolution".into(), self.dimensions()]
        }
    }

    fn native_fields(&self) -> DisplayFields {
        let (width, height) = self.logical_size();
        let window_height = height.saturating_sub(28).max(1);
        Map::from_iter([
            ("native_display".into(), json!({"enabled": true})),
            (
                "window_size".into(),
                json!(format!("{width},{window_height}")),
            ),
        ])
    }
}

pub(super) fn validate_request(width: u32, height: u32, scale: f64, zoom: u32) -> Result<()> {
    if !(640..=8192).contains(&width) || !(360..=8192).contains(&height) {
        return Err(Error::invalid(
            "The requested display resolution is outside the supported range.",
        ));
    }
    if !(1.0..=4.0).contains(&scale) || ![100, 125, 150, 175, 200].contains(&zoom) {
        return Err(Error::invalid(
            "The requested display scale is outside the supported range.",
        ));
    }
    Ok(())
}

fn sway_block(text: &str) -> Result<Option<(usize, usize)>> {
    match (text.find(BEGIN), text.find(END)) {
        (None, None) => Ok(None),
        (Some(start), Some(end))
            if start < end
                && text.matches(BEGIN).count() == 1
                && text.matches(END).count() == 1 =>
        {
            Ok(Some((start, end + END.len())))
        }
        _ => Err(Error::invalid(
            "The native display block is incomplete. Restore it before continuing.",
        )),
    }
}

pub(super) fn sway_config(text: &str, output: Option<&Output>) -> Result<Vec<u8>> {
    let mut next = text.to_owned();
    if let Some((start, end)) = sway_block(text)? {
        let end = end + usize::from(text.as_bytes().get(end) == Some(&b'\n'));
        next.replace_range(start..end, "");
    }
    if let Some(output) = output {
        output.validate()?;
        if !next.is_empty() && !next.ends_with('\n') {
            next.push('\n');
        }
        next.push_str(&format!(
            "{BEGIN}\noutput \"{}\" {} scale {}\n{END}\n",
            output.name,
            output.mode_args().join(" "),
            output.scale
        ));
    }
    Ok(next.into_bytes())
}

// A v2 state predating managed_display records its last applied fields only
// through the exact Sway block the Python helper installed.
fn fields_from_sway(text: &str, baseline: &Output) -> Option<DisplayFields> {
    let (start, end) = sway_block(text).ok()??;
    let block = text[start..end].strip_prefix(BEGIN)?.strip_suffix(END)?;
    let line = block.strip_prefix('\n')?.strip_suffix('\n')?;
    if line.contains('\n') {
        return None;
    }
    let (name, geometry) = line.strip_prefix("output \"")?.split_once("\" ")?;
    let geometry = geometry
        .strip_prefix("resolution ")
        .or_else(|| geometry.strip_prefix("mode --custom "))?;
    let (mode, scale) = geometry.split_once(" scale ")?;
    let (width, height) = mode.split_once('x')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    if name != baseline.name
        || !digits(width)
        || !digits(height)
        || !scale
            .split_once('.')
            .map_or_else(|| digits(scale), |(a, b)| digits(a) && digits(b))
    {
        return None;
    }
    let output = Output {
        name: name.into(),
        width: width.parse().ok()?,
        height: height.parse().ok()?,
        scale: scale.parse().ok()?,
    };
    validate_request(output.width, output.height, output.scale, 100).ok()?;
    Some(output.native_fields())
}

type DisplayFields = Map<String, Value>;

pub(super) fn native(profile: &Value) -> bool {
    profile["native_display"]["enabled"].as_bool() == Some(true)
}

pub(super) fn profile(bytes: &[u8], saved: bool) -> Result<Value> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .filter(|value| value.is_object() && value.get("chrome").is_none_or(Value::is_object))
        .ok_or_else(|| {
            Error::invalid(if saved {
                "The saved Chrome settings are invalid; native display was left active."
            } else {
                "Chrome settings are missing or invalid; restore them before continuing."
            })
        })
}

fn fields(profile: &Value) -> DisplayFields {
    [
        ("native_display", profile.get("native_display")),
        ("window_size", profile["chrome"].get("window_size")),
    ]
    .into_iter()
    .filter_map(|(key, value)| Some((key.into(), value?.clone())))
    .collect()
}

fn replace_fields(config: &mut Value, mut fields: DisplayFields) {
    let object = config.as_object_mut().expect("parsed profile");
    if let Some(value) = fields.shift_remove("native_display") {
        object.insert("native_display".into(), value);
    } else {
        object.shift_remove("native_display");
    }
    if let Some(size) = fields.shift_remove("window_size") {
        config
            .as_object_mut()
            .expect("parsed profile")
            .entry("chrome")
            .or_insert_with(|| json!({}))["window_size"] = size;
    } else if let Some(chrome) = config.get_mut("chrome").and_then(Value::as_object_mut) {
        chrome.shift_remove("window_size");
    }
}

pub(super) fn json_bytes(value: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

// Python's old state compared numeric JSON values by value, while keeping
// booleans distinct from numbers and preserving absent vs. null fields.
fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) if left.is_f64() || right.is_f64() => {
            left.as_f64() == right.as_f64()
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| json_equal(a, b))
        }
        (Value::Object(left), Value::Object(right)) => fields_equal(left, right),
        _ => left == right,
    }
}

fn fields_equal(left: &DisplayFields, right: &DisplayFields) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .all(|(key, value)| right.get(key).is_some_and(|other| json_equal(value, other)))
}

pub(super) fn launch_profile_changed(old: &Value, next: &Value) -> bool {
    if native(old) && native(next) {
        let (mut old, mut next) = (old.clone(), next.clone());
        for config in [&mut old, &mut next] {
            if let Some(chrome) = config.get_mut("chrome").and_then(Value::as_object_mut) {
                // Sway tiles the Wayland window. Chrome uses this value only on X11.
                chrome.shift_remove("window_size");
            }
        }
        !json_equal(&old, &next)
    } else {
        !json_equal(old, next)
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::path("read", path, error)),
    }
}

#[derive(Deserialize)]
struct StateHeader {
    #[serde(default)]
    version: Option<u32>,
    baseline: Output,
    zoom: u32,
}

#[derive(Serialize)]
struct StateV2 {
    version: u32,
    baseline: Output,
    zoom: u32,
    // Retain the v2 field for older binaries. Both viewers share this source.
    transport: &'static str,
    managed_config_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    managed_display: Option<DisplayFields>,
    original_config: String,
}

#[derive(Deserialize)]
struct RestoreV2 {
    managed_config_sha256: String,
    #[serde(default, deserialize_with = "deserialize_fields")]
    managed_display: Option<DisplayFields>,
    original_config: String,
}

fn deserialize_fields<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<Option<DisplayFields>, D::Error> {
    DisplayFields::deserialize(deserializer).map(Some)
}

pub(super) struct Saved {
    pub baseline: Output,
    pub zoom: u32,
    original: Option<Vec<u8>>,
    managed_sha: Option<String>,
    managed_fields: Option<DisplayFields>,
    legacy_managed: Option<Vec<u8>>,
}

fn invalid_state() -> Error {
    Error::invalid("The native display restore state is invalid.")
}

impl Saved {
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let Some(bytes) = read_optional(&dir.join("state.json"))? else {
            return Ok(None);
        };
        let StateHeader {
            version,
            baseline,
            zoom,
        } = serde_json::from_slice(&bytes).map_err(|_| invalid_state())?;
        baseline.validate()?;
        let (original, managed_sha, managed_fields, legacy_managed) = match version {
            Some(VERSION) => {
                let raw: RestoreV2 = serde_json::from_slice(&bytes).map_err(|_| invalid_state())?;
                let hash = raw.managed_config_sha256;
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                    || raw.managed_display.as_ref().is_some_and(|fields| {
                        fields
                            .keys()
                            .any(|key| !matches!(key.as_str(), "native_display" | "window_size"))
                    })
                {
                    return Err(invalid_state());
                }
                (
                    Some(
                        STANDARD
                            .decode(raw.original_config)
                            .map_err(|_| invalid_state())?,
                    ),
                    Some(hash),
                    raw.managed_display,
                    None,
                )
            }
            None | Some(1) => {
                let original = read_optional(&dir.join("original-config"))?;
                let managed = read_optional(&dir.join("managed-config"))?;
                (original, managed.as_deref().map(digest), None, managed)
            }
            _ => return Err(invalid_state()),
        };
        Ok(Some(Self {
            baseline,
            zoom,
            original,
            managed_sha,
            managed_fields,
            legacy_managed,
        }))
    }

    pub fn original<'a>(&'a self, current: &'a [u8]) -> Result<&'a [u8]> {
        if let Some(original) = &self.original {
            Ok(original)
        } else if self.legacy_managed.is_some() {
            Err(Error::invalid(
                "The legacy display restore state is incomplete.",
            ))
        } else {
            Ok(current)
        }
    }

    pub fn matches_hash(&self, current: &[u8]) -> bool {
        self.managed_sha
            .as_ref()
            .is_some_and(|sha| sha == &digest(current))
    }

    fn managed_fields(&self, current: Option<&[u8]>, sway: &str) -> Result<Option<DisplayFields>> {
        if let Some(fields) = &self.managed_fields {
            return Ok(Some(fields.clone()));
        }
        if let Some(bytes) = &self.legacy_managed {
            return Ok(Some(fields(&profile(bytes, false)?)));
        }
        if let Some(current) = current.filter(|bytes| self.matches_hash(bytes)) {
            let profile = profile(current, false)?;
            if profile.get("native_display") == Some(&json!({"enabled": true})) {
                return Ok(Some(fields(&profile)));
            }
        }
        Ok(fields_from_sway(sway, &self.baseline))
    }

    pub fn check(&self, current: &[u8], config: &Value, sway: &str) -> Result<()> {
        let original = profile(self.original(current)?, true)?;
        let current_fields = fields(config);
        if self.matches_hash(current)
            || fields_equal(&current_fields, &fields(&original))
            || self
                .managed_fields(None, sway)?
                .is_some_and(|managed| fields_equal(&current_fields, &managed))
        {
            return Ok(());
        }
        Err(Error::Conflict(
            "The Chrome display settings changed. Restore or review them before continuing.".into(),
        ))
    }

    pub fn sync(
        &self,
        current: &[u8],
        next: &[u8],
        sway: &str,
        environment: &Environment,
    ) -> Result<Vec<u8>> {
        let mut original = profile(self.original(current)?, true)?;
        environment.patch(&mut original)?;
        state_bytes(
            self.baseline.clone(),
            self.zoom,
            &json_bytes(&original)?,
            next,
            self.managed_fields(Some(current), sway)?,
        )
    }
}

fn state_bytes(
    baseline: Output,
    zoom: u32,
    original: &[u8],
    next: &[u8],
    managed_display: Option<DisplayFields>,
) -> Result<Vec<u8>> {
    json_bytes(&StateV2 {
        version: VERSION,
        baseline,
        zoom,
        transport: "vnc",
        managed_config_sha256: digest(next),
        managed_display,
        original_config: STANDARD.encode(original),
    })
}

pub(super) fn apply_profile(
    current: &[u8],
    mut config: Value,
    target: &Output,
    baseline: Output,
    zoom: u32,
    original: &[u8],
) -> Result<(Vec<u8>, Vec<u8>)> {
    if !config["chrome"].is_object() {
        return Err(Error::invalid(
            "Chrome settings are missing or invalid; restore them before continuing.",
        ));
    }
    let managed = target.native_fields();
    let current_fields = fields(&config);
    replace_fields(&mut config, managed.clone());
    let next = if fields_equal(&managed, &current_fields) {
        current.to_vec()
    } else {
        json_bytes(&config)?
    };
    let saved = state_bytes(baseline, zoom, original, &next, Some(managed))?;
    Ok((next, saved))
}

pub(super) fn restore_profile(
    current: &[u8],
    mut config: Value,
    original: &[u8],
) -> Result<Vec<u8>> {
    let baseline = profile(original, true)?;
    let before = config.clone();
    replace_fields(&mut config, fields(&baseline));
    if json_equal(&config, &before) {
        Ok(current.to_vec())
    } else if json_equal(&config, &baseline) {
        Ok(original.to_vec())
    } else {
        json_bytes(&config)
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Environment {
    timezone: String,
    locale: String,
    language: String,
    languages: Vec<String>,
    chrome_language: String,
}

impl Environment {
    pub fn patch(&self, config: &mut Value) -> Result<()> {
        if self.timezone.is_empty()
            || self.locale.is_empty()
            || self.language.is_empty()
            || self.chrome_language.is_empty()
            || self.languages.is_empty()
            || self.languages.iter().any(String::is_empty)
        {
            return Err(Error::invalid("The browser environment is incomplete."));
        }
        config["timezone"] = json!(self.timezone);
        config["locale"] = json!(self.locale);
        if !config["navigator"].is_object() {
            config["navigator"] = json!({});
        }
        config["navigator"]["language"] = json!(self.language);
        config["navigator"]["languages"] = json!(self.languages);
        config
            .as_object_mut()
            .expect("parsed profile")
            .entry("chrome")
            .or_insert_with(|| json!({}))["lang"] = json!(self.chrome_language);
        Ok(())
    }
}
