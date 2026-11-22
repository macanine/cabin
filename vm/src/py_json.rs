//! Byte-compatible JSON serialisation with Python's `json` module.
//!
//! Fingerprints are hashes over canonical JSON, and the Python pipeline is the
//! migration oracle.  `serde_json` differs from Python in two visible ways:
//! float formatting (`1e-7` vs `1e-07`) and non-ASCII escaping (`é` vs
//! `\u00e9`).  Those differences would change every device/seed/disk
//! fingerprint and make existing instances look reconfigured, so this module
//! implements the Python compact and indent=2 renderings directly.

use serde_json::{Map, Value};

/// `json.dumps(value, sort_keys=True, separators=(",", ":"))`
pub fn dumps_compact(value: &Value) -> String {
    let mut out = String::new();
    write_compact(value, &mut out);
    out
}

/// `json.dumps(value, indent=2, sort_keys=True)`
pub fn dumps_pretty(value: &Value) -> String {
    let mut out = String::new();
    write_pretty(value, &mut out, 0);
    out
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`
pub fn dumps_compact_utf8(value: &Value) -> String {
    let sorted = sorted_value(value);
    serde_json::to_string(&sorted).unwrap_or_else(|_| "null".to_string())
}

fn sorted_value(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sorted_value).collect()),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, item) in sorted(map) {
                out.insert(key.clone(), sorted_value(item));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// `json.dumps(value, sort_keys=sort_keys)` with Python's default separators.
pub fn dumps_default(value: &Value, sort_keys: bool) -> String {
    let mut out = String::new();
    write_default(value, &mut out, sort_keys);
    out
}

/// `json.dumps(value, indent=2, sort_keys=sort_keys)`.
pub fn dumps_pretty_with(value: &Value, sort_keys: bool) -> String {
    let mut out = String::new();
    write_pretty_with(value, &mut out, 0, sort_keys);
    out
}

fn write_default(value: &Value, out: &mut String, sort_keys: bool) {
    match value {
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_default(item, out, sort_keys);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            let entries: Vec<(&String, &Value)> = if sort_keys {
                sorted(map)
            } else {
                map.iter().collect()
            };
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_string(key, out);
                out.push_str(": ");
                write_default(item, out, sort_keys);
            }
            out.push('}');
        }
        other => write_compact(other, out),
    }
}

fn write_pretty_with(value: &Value, out: &mut String, indent: usize, sort_keys: bool) {
    match value {
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&"  ".repeat(indent + 1));
                write_pretty_with(item, out, indent + 1, sort_keys);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&"  ".repeat(indent));
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            let entries: Vec<(&String, &Value)> = if sort_keys {
                sorted(map)
            } else {
                map.iter().collect()
            };
            for (index, (key, item)) in entries.iter().enumerate() {
                out.push_str(&"  ".repeat(indent + 1));
                write_string(key, out);
                out.push_str(": ");
                write_pretty_with(item, out, indent + 1, sort_keys);
                if index + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&"  ".repeat(indent));
            out.push('}');
        }
        other => write_default(other, out, sort_keys),
    }
}

/// Python `repr()` for the JSON-compatible values used in report details.
pub fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => write_number_string(number),
        Value::String(text) => py_repr_string(text),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(key, item)| format!("{}: {}", py_repr_string(key), py_repr(item)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub fn py_repr_string(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::new();
    out.push(quote);
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\x08"),
            '\u{0c}' => out.push_str("\\x0c"),
            ch if ch == quote => {
                out.push('\\');
                out.push(ch);
            }
            ch if (ch as u32) < 0x20 || (ch as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", ch as u32));
            }
            ch if (ch as u32) >= 0x7f => {
                let code = ch as u32;
                if code <= 0xFF {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xFFFF {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push(quote);
    out
}

fn write_number_string(number: &serde_json::Number) -> String {
    if let Some(value) = number.as_i64() {
        value.to_string()
    } else if let Some(value) = number.as_u64() {
        value.to_string()
    } else if let Some(value) = number.as_f64() {
        py_float_repr(value)
    } else {
        number.to_string()
    }
}

fn write_compact(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => write_number(number, out),
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_compact(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            for (index, (key, item)) in sorted(map).iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_compact(item, out);
            }
            out.push('}');
        }
    }
}

fn write_pretty(value: &Value, out: &mut String, indent: usize) {
    match value {
        Value::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                out.push_str(&"  ".repeat(indent + 1));
                write_pretty(item, out, indent + 1);
                if index + 1 < items.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&"  ".repeat(indent));
            out.push(']');
        }
        Value::Object(map) if !map.is_empty() => {
            out.push_str("{\n");
            let entries = sorted(map);
            for (index, (key, item)) in entries.iter().enumerate() {
                out.push_str(&"  ".repeat(indent + 1));
                write_string(key, out);
                out.push_str(": ");
                write_pretty(item, out, indent + 1);
                if index + 1 < entries.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&"  ".repeat(indent));
            out.push('}');
        }
        other => write_compact(other, out),
    }
}

fn sorted(map: &Map<String, Value>) -> Vec<(&String, &Value)> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
}

fn write_number(number: &serde_json::Number, out: &mut String) {
    if let Some(value) = number.as_i64() {
        out.push_str(&value.to_string());
    } else if let Some(value) = number.as_u64() {
        out.push_str(&value.to_string());
    } else if let Some(value) = number.as_f64() {
        out.push_str(&py_float_repr(value));
    } else {
        out.push_str(&number.to_string());
    }
}

/// Python's `repr(float)` in the common finite range.
///
/// Rust's `{:e}` yields the same shortest round-trip digit string as CPython;
/// only the presentation thresholds and exponent padding differ.
pub fn py_float_repr(value: f64) -> String {
    if value.is_nan() {
        return "NaN".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0".to_string()
        } else {
            "0.0".to_string()
        };
    }
    let negative = value < 0.0;
    let magnitude = value.abs();
    let scientific = format!("{magnitude:e}");
    let Some((mantissa, exponent)) = scientific.split_once('e') else {
        return format!("{value:?}");
    };
    let Ok(exponent) = exponent.parse::<i32>() else {
        return format!("{value:?}");
    };
    let digits: String = mantissa.chars().filter(|ch| *ch != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let count = digits.len() as i32;
    let mut text = String::new();
    if negative {
        text.push('-');
    }
    if !(-4..16).contains(&exponent) {
        // Scientific form: d[.ddd]e±XX
        text.push_str(&digits[..1]);
        if count > 1 {
            text.push('.');
            text.push_str(&digits[1..]);
        }
        let sign = if exponent < 0 { '-' } else { '+' };
        text.push('e');
        text.push(sign);
        let absolute = exponent.abs();
        if absolute < 10 {
            text.push('0');
        }
        text.push_str(&absolute.to_string());
    } else if exponent >= count - 1 {
        // Integral with trailing zeros.
        text.push_str(digits);
        for _ in 0..(exponent - (count - 1)) {
            text.push('0');
        }
        text.push_str(".0");
    } else if exponent >= 0 {
        let split = (exponent + 1) as usize;
        text.push_str(&digits[..split]);
        text.push('.');
        text.push_str(&digits[split..]);
    } else {
        text.push_str("0.");
        for _ in 0..(-exponent - 1) {
            text.push('0');
        }
        text.push_str(digits);
    }
    text
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", ch as u32));
            }
            ch if (ch as u32) < 0x7f => out.push(ch),
            ch => {
                let code = ch as u32;
                if code <= 0xFFFF {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    let adjusted = code - 0x10000;
                    let high = 0xD800 + (adjusted >> 10);
                    let low = 0xDC00 + (adjusted & 0x3FF);
                    out.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
                }
            }
        }
    }
    out.push('"');
}

/// Parse a `--set` value the way `json.loads` does when possible.
pub fn parse_loose_value(text: &str) -> Value {
    let stripped = text.trim();
    if stripped.is_empty() {
        return Value::String(String::new());
    }
    match serde_json::from_str::<Value>(stripped) {
        Ok(value) => value,
        Err(_) => Value::String(text.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_matches_python() {
        let value = json!({"b": 1, "a": [true, null, "x\né"], "c": 1.0, "d": 1e-7});
        assert_eq!(
            dumps_compact(&value),
            r#"{"a":[true,null,"x\n\u00e9"],"b":1,"c":1.0,"d":1e-07}"#
        );
    }

    #[test]
    fn pretty_matches_python() {
        let value = json!({"b": [1, 2], "a": {"c": 3}});
        assert_eq!(
            dumps_pretty(&value),
            "{\n  \"a\": {\n    \"c\": 3\n  },\n  \"b\": [\n    1,\n    2\n  ]\n}"
        );
    }

    #[test]
    fn float_repr_thresholds() {
        assert_eq!(py_float_repr(1.0), "1.0");
        assert_eq!(py_float_repr(1e-5), "1e-05");
        assert_eq!(py_float_repr(0.0001), "0.0001");
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(1.5e16), "1.5e+16");
        assert_eq!(py_float_repr(1234567890123456.0), "1234567890123456.0");
        assert_eq!(py_float_repr(-0.0001), "-0.0001");
        assert_eq!(py_float_repr(f64::MAX), "1.7976931348623157e+308");
        assert_eq!(py_float_repr(f64::MIN_POSITIVE), "2.2250738585072014e-308");
        assert_eq!(py_float_repr(f64::from_bits(1)), "5e-324");
    }
}
