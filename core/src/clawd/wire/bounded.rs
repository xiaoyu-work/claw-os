//! Bounded scalar and structured types the broker accepts on the wire.
//!
//! Every field a route declares is one of these. They exist so a typed
//! request struct carries its own limits: a `String` field would accept
//! a gigabyte of caller text and a `serde_json::Value` field would
//! accept an arbitrarily wide object, and neither limit would be
//! visible at the route definition.
//!
//! Validation failures are `&'static str`. Caller text — including the
//! value that failed — never appears in the message, so a rejection can
//! be answered to the peer and classified for audit without carrying a
//! secret out of the request.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Deepest structure a route-declared [`Structured`] payload may carry.
///
/// `serde_json` already refuses more than 128 levels while parsing the
/// frame, so this is a second, much tighter bound applied to the value
/// the route actually receives.
pub const MAX_STRUCTURED_DEPTH: usize = 12;
/// Total values (scalars, arrays and objects) in one payload.
pub const MAX_STRUCTURED_NODES: usize = 4096;
/// Longest string inside a structured payload.
pub const MAX_STRUCTURED_STRING_BYTES: usize = 64 * 1024;
/// Longest array inside a structured payload.
pub const MAX_STRUCTURED_ARRAY_LEN: usize = 1024;
/// Widest object inside a structured payload.
pub const MAX_STRUCTURED_OBJECT_LEN: usize = 256;
/// Longest object key inside a structured payload.
pub const MAX_STRUCTURED_KEY_BYTES: usize = 256;
/// CLI JSON arguments leave 16 KiB of the broker frame for selectors/envelopes.
/// Keep the SDK's `APP_ARGS_STDIN_MAX_BYTES` in sync.
pub const APP_ARGS_STDIN_MAX_BYTES: usize = super::MAX_REQUEST_BYTES - 16 * 1024;

/// Longest a client may ask a long-polling route to wait: one day,
/// which is the ceiling `cos agent ask` already uses. Without a bound
/// here `Instant::now() + Duration::from_millis(v)` overflows and the
/// connection is pinned for as long as the caller likes.
pub const MAX_WAIT_MS: u64 = 24 * 60 * 60 * 1_000;

const TOO_LONG: &str = "value exceeds the field's maximum length";
const TOKEN_CHARSET: &str = "value contains characters outside [A-Za-z0-9._-]";
const NAME_CHARSET: &str = "value contains characters outside [A-Za-z0-9._:/@+~-]";
const TEXT_CONTROL: &str = "value contains control characters";
const LIST_TOO_LONG: &str = "list exceeds the field's maximum length";
const WAIT_TOO_LONG: &str = "wait exceeds the broker's maximum";

/// A short selector: an action name, a status, a session id, an
/// approval id, a launch handle.
///
/// Surrounding whitespace is trimmed exactly as every route handler
/// already trims it, so the value a handler sees is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Token<const MAX: usize = 128>(String);

impl<const MAX: usize> Token<MAX> {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        let trimmed = raw.trim();
        if trimmed.len() > MAX {
            return Err(TOO_LONG);
        }
        if !trimmed
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(TOKEN_CHARSET);
        }
        Ok(Self(trimmed.to_string()))
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for Token<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

/// A resource identifier: a unit name, a package, a device node, a
/// credential name, a mime type, a CIDR.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Name<const MAX: usize = 256>(String);

impl<const MAX: usize> Name<MAX> {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        let trimmed = raw.trim();
        if trimmed.len() > MAX {
            return Err(TOO_LONG);
        }
        if !trimmed.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/' | b'@' | b'+' | b'~')
        }) {
            return Err(NAME_CHARSET);
        }
        Ok(Self(trimmed.to_string()))
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for Name<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

/// Free text a route genuinely needs to carry verbatim: a prompt, a
/// filesystem path, a print job title, an approval note.
///
/// Only the length and the control range are constrained — tab,
/// newline and carriage return are kept because prompts and file
/// contents contain them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Text<const MAX: usize>(String);

impl<const MAX: usize> Text<MAX> {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        if raw.len() > MAX {
            return Err(TOO_LONG);
        }
        if raw
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\t' | '\n' | '\r'))
        {
            return Err(TEXT_CONTROL);
        }
        Ok(Self(raw.to_string()))
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for Text<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(de::Error::custom)
    }
}

/// A bounded list of bounded strings — App operation arguments,
/// approval request ids.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TextList<const MAX_ITEMS: usize, const MAX_ITEM: usize>(Vec<String>);

impl<const MAX_ITEMS: usize, const MAX_ITEM: usize> TextList<MAX_ITEMS, MAX_ITEM> {
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl<'de, const MAX_ITEMS: usize, const MAX_ITEM: usize> Deserialize<'de>
    for TextList<MAX_ITEMS, MAX_ITEM>
{
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Vec::<String>::deserialize(deserializer)?;
        if raw.len() > MAX_ITEMS {
            return Err(de::Error::custom(LIST_TOO_LONG));
        }
        for item in &raw {
            Text::<MAX_ITEM>::parse(item).map_err(de::Error::custom)?;
        }
        Ok(Self(raw))
    }
}

/// Milliseconds a long-polling route may be asked to wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct WaitMillis(u64);

impl WaitMillis {
    pub fn get(self) -> u64 {
        self.0
    }
}

impl<'de> Deserialize<'de> for WaitMillis {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = u64::deserialize(deserializer)?;
        if raw > MAX_WAIT_MS {
            return Err(de::Error::custom(WAIT_TOO_LONG));
        }
        Ok(Self(raw))
    }
}

/// A payload whose *shape* is part of a route's public contract — a
/// capability set, a canonical scope, an App session tool call, a
/// scheduler argument vector, a context source document.
///
/// The broker does not interpret these; the owning authority
/// re-validates them against its own typed model. What this type
/// guarantees is that the value is finite before it is handed on:
/// bounded depth, node count, string length, array length and object
/// width.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Structured(Value);

impl Structured {
    pub fn as_value(&self) -> &Value {
        &self.0
    }

    pub fn into_value(self) -> Value {
        self.0
    }

    pub fn parse(value: Value) -> Result<Self, &'static str> {
        let mut nodes = 0usize;
        validate_structured(&value, 0, &mut nodes, MAX_STRUCTURED_STRING_BYTES)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for Structured {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

const STRUCTURED_TOO_DEEP: &str = "structured payload is nested too deeply";
const STRUCTURED_TOO_MANY_NODES: &str = "structured payload has too many values";
const STRUCTURED_STRING_TOO_LONG: &str = "structured payload contains an oversized string";
const STRUCTURED_ARRAY_TOO_LONG: &str = "structured payload contains an oversized array";
const STRUCTURED_OBJECT_TOO_WIDE: &str = "structured payload contains an oversized object";
const STRUCTURED_KEY_TOO_LONG: &str = "structured payload contains an oversized key";

/// Business arguments for a human CLI MCP call. Unlike general broker metadata,
/// content strings may fill the bounded request; all structural limits remain.
#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct McpArguments(Value);

impl McpArguments {
    pub fn parse(value: Value) -> Result<Self, &'static str> {
        if !value.is_object() {
            return Err("MCP arguments must be a JSON object");
        }
        if serde_json::to_vec(&value)
            .map_err(|_| "cannot encode MCP arguments")?
            .len()
            > APP_ARGS_STDIN_MAX_BYTES
        {
            return Err("MCP arguments exceed the JSON stdin byte limit");
        }
        validate_structured(&value, 0, &mut 0, APP_ARGS_STDIN_MAX_BYTES)?;
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for McpArguments {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(Value::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

fn validate_structured(
    value: &Value,
    depth: usize,
    nodes: &mut usize,
    max_string_bytes: usize,
) -> Result<(), &'static str> {
    if depth > MAX_STRUCTURED_DEPTH {
        return Err(STRUCTURED_TOO_DEEP);
    }
    *nodes += 1;
    if *nodes > MAX_STRUCTURED_NODES {
        return Err(STRUCTURED_TOO_MANY_NODES);
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(text) => {
            if text.len() > max_string_bytes {
                Err(STRUCTURED_STRING_TOO_LONG)
            } else {
                Ok(())
            }
        }
        Value::Array(items) => {
            if items.len() > MAX_STRUCTURED_ARRAY_LEN {
                return Err(STRUCTURED_ARRAY_TOO_LONG);
            }
            for item in items {
                validate_structured(item, depth + 1, nodes, max_string_bytes)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            if map.len() > MAX_STRUCTURED_OBJECT_LEN {
                return Err(STRUCTURED_OBJECT_TOO_WIDE);
            }
            for (key, item) in map {
                if key.len() > MAX_STRUCTURED_KEY_BYTES {
                    return Err(STRUCTURED_KEY_TOO_LONG);
                }
                validate_structured(item, depth + 1, nodes, max_string_bytes)?;
            }
            Ok(())
        }
    }
}

/// A route that takes no arguments at all.
///
/// Declared rather than assumed: `deny_unknown_fields` on an empty
/// struct is what makes `daemon.health` refuse a params object instead
/// of ignoring it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/wire/bounded.rs"
    ));
}
