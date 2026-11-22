use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Result, error};

// This is the same serializer used for existing VM fingerprints. Python's
// default JSON whitespace, ASCII escaping and float rendering are significant.
// The VM crate's serializer, compiled here from the same source so profile
// hashes match byte for byte on both sides; only `dumps_default` is used.
#[expect(dead_code, reason = "shared source file; this crate uses one function of it")]
#[path = "../../../vm/src/py_json.rs"]
mod py_json;

const INJECT: &str = include_str!("../../../vm/assets/browser_inject.js");

pub(super) struct Profile {
    pub(super) data: Value,
    pub(super) source: String,
}

impl Profile {
    pub(super) fn load(config: &Path, inject: &Path, explicit_inject: bool) -> Result<Self> {
        let mut data: Value = serde_json::from_slice(&fs::read(config)?)?;
        let hash = format!(
            "{:x}",
            Sha256::digest(py_json::dumps_default(&data, true).as_bytes())
        );
        data.as_object_mut()
            .ok_or_else(|| error("Browser profile must be an object"))?
            .insert("fingerprint".into(), json!(hash));
        let source = match fs::read_to_string(inject) {
            Ok(source) => replace_profile(&source, &data)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit_inject => {
                format!(
                    "{}\n{}",
                    INJECT.replace(
                        "__VMCTL_BROWSER_PROFILE__",
                        &py_json::dumps_default(&data, true)
                    ),
                    data.get("extra_js").and_then(Value::as_str).unwrap_or("")
                )
            }
            Err(e) => return Err(e.into()),
        };
        Ok(Self { data, source })
    }

    pub(super) fn port(&self) -> Result<u16> {
        let port = self
            .data
            .get("remote_debugging_port")
            .and_then(Value::as_u64)
            .filter(|v| *v != 0)
            .unwrap_or(9222);
        u16::try_from(port).map_err(|_| error("Invalid Chrome DevTools port"))
    }
    pub(super) fn native_display(&self) -> bool {
        self.data.pointer("/native_display/enabled") == Some(&json!(true))
    }
}

fn replace_profile(source: &str, data: &Value) -> Result<String> {
    let mut output = String::with_capacity(source.len());
    let mut replaced = false;
    for line in source.split_inclusive('\n') {
        if !replaced && line.starts_with("  const C = ") && line.trim_end().ends_with(';') {
            output.push_str("  const C = ");
            output.push_str(&py_json::dumps_default(data, true));
            output.push_str(";\n");
            replaced = true;
        } else {
            output.push_str(line);
        }
    }
    if replaced {
        Ok(output)
    } else {
        Err(error(
            "The browser injection script has no profile declaration.",
        ))
    }
}

pub(super) fn default_config() -> PathBuf {
    PathBuf::from("/opt/vmctl-fp/config.json")
}
pub(super) fn default_inject() -> PathBuf {
    PathBuf::from("/opt/vmctl-fp/inject.js")
}

pub(super) fn metadata(data: &Value) -> Value {
    let ua = &data["ua_data"];
    let get = |name: &str, default: Value| {
        ua.get(name)
            .filter(|v| !v.is_null())
            .cloned()
            .unwrap_or(default)
    };
    json!({"brands":get("brands",json!([])),"fullVersionList":get("full_version_list",json!([])),
        "fullVersion":get("full_version",json!("")),"platform":get("platform",json!("")),
        "platformVersion":get("platform_version",json!("")),"architecture":get("architecture",json!("")),
        "model":get("model",json!("")),"mobile":ua["mobile"].as_bool().unwrap_or(false),
        "bitness":get("bitness",json!("")),"wow64":ua["wow64"].as_bool().unwrap_or(false)})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injection_replaces_stale_profile_and_keeps_extra_script() {
        let source = "(() => {\n  const C = {\"old\": true};\n})();\nextra();\n";
        let result = replace_profile(source, &json!({"name":"中文"})).unwrap();
        assert!(result.contains(r#"  const C = {"name": "\u4e2d\u6587"};"#));
        assert!(result.ends_with("extra();\n"));
        assert!(replace_profile("no declaration", &json!({})).is_err());
    }
    #[test]
    fn fingerprint_matches_python_sorted_ascii_json() {
        let data = json!({"z":1.0,"name":"é", "a":[true,1e-7]});
        assert_eq!(
            py_json::dumps_default(&data, true),
            r#"{"a": [true, 1e-07], "name": "\u00e9", "z": 1.0}"#
        );
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(py_json::dumps_default(&data, true).as_bytes())
            ),
            "5ce244672db8f554bf85a61a460c3e1e24de849a99b5df5d4a022a8e940ffb49"
        );
    }
}
