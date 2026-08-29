//! Deterministic JSON encoding.
//!
//! A signature is over bytes, so "the same document" has to mean one
//! byte string. Every release manifest, floor generation and recovery
//! authorization is written in this encoding and re-encoded before it
//! is verified: a document whose bytes are not already canonical is
//! refused rather than normalized, so nobody can ship a second
//! encoding of a signed document and have it accepted.
//!
//! Rules:
//!
//! * object keys sorted by Unicode code point, no duplicates;
//! * no insignificant whitespace;
//! * integers only — a floating point number has no single textual
//!   form, so it is rejected outright;
//! * `"` `\` and the C0 controls escaped, everything else literal
//!   UTF-8.
//!
//! This matches `json.dumps(obj, sort_keys=True, separators=(",", ":"),
//! ensure_ascii=False)`, which is what the packaging scripts use, so a
//! manifest generated at build time verifies unchanged at install time.

use serde_json::{Map, Value};

/// Encode `value` canonically.
pub fn to_string(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_value(value, &mut out)?;
    Ok(out)
}

/// Canonical bytes, newline-terminated the way the generated files are
/// written on disk.
pub fn to_bytes(value: &Value) -> Result<Vec<u8>, String> {
    let mut bytes = to_string(value)?.into_bytes();
    bytes.push(b'\n');
    Ok(bytes)
}

/// Parse `bytes` and require that they already *are* the canonical
/// encoding of what they parse to.
///
/// The trailing newline is optional so the same check works for a
/// whole file and for one line of a JSONL history.
pub fn parse_canonical(bytes: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "document is not valid UTF-8".to_string())?;
    let trimmed = text.strip_suffix('\n').unwrap_or(text);
    if trimmed.contains('\n') {
        return Err("document contains an embedded newline".to_string());
    }
    let value: Value =
        serde_json::from_str(trimmed).map_err(|_| "document is not valid JSON".to_string())?;
    let canonical = to_string(&value)?;
    if canonical != trimmed {
        return Err("document is not in canonical encoding".to_string());
    }
    Ok(value)
}

fn write_value(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => {
            let rendered = number.to_string();
            if !is_canonical_integer(&rendered) {
                return Err(format!("`{rendered}` is not a canonical integer"));
            }
            out.push_str(&rendered);
        }
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => write_object(map, out)?,
    }
    Ok(())
}

fn write_object(map: &Map<String, Value>, out: &mut String) -> Result<(), String> {
    let mut keys = map.keys().collect::<Vec<_>>();
    keys.sort();
    out.push('{');
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_string(key, out);
        out.push(':');
        let Some(child) = map.get(*key) else {
            return Err("object key vanished during encoding".to_string());
        };
        write_value(child, out)?;
    }
    out.push('}');
    Ok(())
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// `-0`, `1.0`, `1e3` and NaN all denote a number no signature can
/// pin, so only a plain optionally-negative integer without leading
/// zeros is canonical.
fn is_canonical_integer(rendered: &str) -> bool {
    let digits = rendered.strip_prefix('-').unwrap_or(rendered);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return false;
    }
    rendered != "-0"
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/update/canonical.rs"
    ));
}
