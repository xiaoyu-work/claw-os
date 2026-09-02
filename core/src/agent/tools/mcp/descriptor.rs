//! Sanitization and stability binding for untrusted MCP tool descriptors.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

use super::protocol::ToolDescriptor;

pub(crate) const NEUTRAL_DESCRIPTION: &str =
    "Invoke a configured MCP tool using its structured arguments.";

const MAX_TOOLS: usize = 128;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;
const MAX_SCHEMA_DEPTH: usize = 24;
const MAX_SCHEMA_NODES: usize = 2_048;
const MAX_PROPERTIES: usize = 256;
const MAX_BRANCHES: usize = 32;
const MAX_COMPONENT_BYTES: usize = 128;
const MAX_PROPERTY_BYTES: usize = 64;
const MAX_MODEL_NAME_BYTES: usize = 64;

#[derive(Debug, Clone)]
pub(crate) struct DescriptorSet {
    pub descriptors: Vec<ToolDescriptor>,
    pub digest: String,
}

pub(crate) fn sanitize_descriptor_set(
    server: &str,
    descriptors: Vec<ToolDescriptor>,
) -> Result<DescriptorSet, String> {
    if descriptors.len() > MAX_TOOLS {
        return Err(format!("MCP server advertises more than {MAX_TOOLS} tools"));
    }
    let _ = normalize_component(server, "MCP server")?;
    let mut sanitized = Vec::with_capacity(descriptors.len());
    let mut model_names = HashSet::with_capacity(descriptors.len());
    for descriptor in descriptors {
        validate_remote_name(&descriptor.name)?;
        let model_name = model_tool_name(server, &descriptor.name)?;
        if !model_names.insert(model_name) {
            return Err("MCP tools collide after safe name normalization".to_string());
        }
        let schema = sanitize_root_schema(&descriptor.input_schema)?;
        sanitized.push(ToolDescriptor {
            name: descriptor.name,
            description: None,
            input_schema: schema,
        });
    }
    sanitized.sort_by(|left, right| left.name.cmp(&right.name));
    let digest = descriptor_digest(&sanitized)?;
    Ok(DescriptorSet {
        descriptors: sanitized,
        digest,
    })
}

pub(crate) fn model_tool_name(server: &str, remote: &str) -> Result<String, String> {
    let server = normalize_component(server, "MCP server")?;
    let remote_component = normalize_component(remote, "MCP tool")?;
    let unbounded = format!("mcp_{server}_{remote_component}");
    if unbounded.len() <= MAX_MODEL_NAME_BYTES {
        return Ok(unbounded);
    }
    let suffix = &crate::crypto::sha256_hex(unbounded.as_bytes())[..12];
    let keep = MAX_MODEL_NAME_BYTES
        .checked_sub("mcp___".len() + suffix.len())
        .ok_or_else(|| "MCP tool name limit is invalid".to_string())?;
    let server_keep = keep / 2;
    let remote_keep = keep - server_keep;
    Ok(format!(
        "mcp_{}_{}_{}",
        &server[..server.len().min(server_keep)],
        &remote_component[..remote_component.len().min(remote_keep)],
        suffix
    ))
}

fn validate_remote_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > MAX_COMPONENT_BYTES
        || name.trim() != name
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err("MCP tool name is not a safe identifier".to_string());
    }
    Ok(())
}

fn normalize_component(value: &str, label: &str) -> Result<String, String> {
    if value.is_empty()
        || value.len() > MAX_COMPONENT_BYTES
        || value.trim() != value
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!("{label} name is not a safe identifier"));
    }
    let mut normalized = String::with_capacity(value.len());
    let mut underscore = false;
    for byte in value.bytes() {
        let byte = byte.to_ascii_lowercase();
        if byte.is_ascii_alphanumeric() {
            normalized.push(char::from(byte));
            underscore = false;
        } else if !underscore {
            normalized.push('_');
            underscore = true;
        }
    }
    let normalized = normalized.trim_matches('_').to_string();
    if normalized.is_empty() {
        Err(format!("{label} name normalizes to an empty identifier"))
    } else {
        Ok(normalized)
    }
}

fn sanitize_root_schema(schema: &Value) -> Result<Value, String> {
    if schema.is_null() {
        return Ok(serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true
        }));
    }
    let raw_size = serde_json::to_vec(schema)
        .map_err(|error| format!("encode MCP input schema: {error}"))?
        .len();
    if raw_size > MAX_SCHEMA_BYTES {
        return Err(format!("MCP input schema exceeds {MAX_SCHEMA_BYTES} bytes"));
    }
    reject_references(schema, 0)?;
    let mut budget = SchemaBudget { nodes: 0 };
    let sanitized = sanitize_schema(schema, 0, &mut budget)?;
    let object = sanitized
        .as_object()
        .ok_or_else(|| "MCP input schema root must be an object".to_string())?;
    match object.get("type") {
        None => {}
        Some(Value::String(value)) if value == "object" => {}
        _ => return Err("MCP input schema root type must be object".to_string()),
    }

    fn reject_references(value: &Value, depth: usize) -> Result<(), String> {
        if depth > MAX_SCHEMA_DEPTH {
            return Err("MCP input schema exceeds the nesting limit".to_string());
        }
        match value {
            Value::Array(values) => {
                for value in values {
                    reject_references(value, depth + 1)?;
                }
            }
            Value::Object(values) => {
                for (key, value) in values {
                    if matches!(
                        key.to_ascii_lowercase().as_str(),
                        "$ref" | "$dynamicref" | "$recursiveref"
                    ) {
                        return Err("MCP schemas may not contain references".to_string());
                    }
                    reject_references(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut root = object.clone();
    root.entry("type".to_string())
        .or_insert_with(|| Value::String("object".to_string()));
    root.entry("properties".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    Ok(Value::Object(root))
}

struct SchemaBudget {
    nodes: usize,
}

fn sanitize_schema(
    schema: &Value,
    depth: usize,
    budget: &mut SchemaBudget,
) -> Result<Value, String> {
    if depth > MAX_SCHEMA_DEPTH {
        return Err("MCP input schema exceeds the nesting limit".to_string());
    }
    budget.nodes = budget.nodes.saturating_add(1);
    if budget.nodes > MAX_SCHEMA_NODES {
        return Err("MCP input schema exceeds the node limit".to_string());
    }
    if let Value::Bool(value) = schema {
        return Ok(Value::Bool(*value));
    }
    let object = schema
        .as_object()
        .ok_or_else(|| "MCP schema nodes must be objects or booleans".to_string())?;
    let mut output = BTreeMap::<String, Value>::new();
    if let Some(value) = object.get("type") {
        output.insert("type".to_string(), sanitize_type(value)?);
    }
    if let Some(value) = object.get("properties") {
        let properties = value
            .as_object()
            .ok_or_else(|| "MCP schema properties must be an object".to_string())?;
        if properties.len() > MAX_PROPERTIES {
            return Err(format!(
                "MCP schema has more than {MAX_PROPERTIES} properties"
            ));
        }
        let mut sanitized = BTreeMap::<String, Value>::new();
        for (name, property) in properties {
            validate_property_name(name)?;
            sanitized.insert(name.clone(), sanitize_schema(property, depth + 1, budget)?);
        }
        output.insert("properties".to_string(), ordered_object(sanitized));
    }
    if let Some(value) = object.get("required") {
        let required = value
            .as_array()
            .ok_or_else(|| "MCP schema required must be an array".to_string())?;
        if required.len() > MAX_PROPERTIES {
            return Err("MCP schema required list is too large".to_string());
        }
        let properties = output
            .get("properties")
            .and_then(Value::as_object)
            .ok_or_else(|| "MCP schema required needs declared properties".to_string())?;
        let mut names = Vec::with_capacity(required.len());
        let mut seen = HashSet::with_capacity(required.len());
        for value in required {
            let name = value
                .as_str()
                .ok_or_else(|| "MCP schema required entries must be strings".to_string())?;
            validate_property_name(name)?;
            if !properties.contains_key(name) {
                return Err("MCP schema requires an undeclared property".to_string());
            }
            if !seen.insert(name) {
                return Err("MCP schema repeats a required property".to_string());
            }
            names.push(Value::String(name.to_string()));
        }
        output.insert("required".to_string(), Value::Array(names));
    }
    if let Some(value) = object.get("items") {
        output.insert(
            "items".to_string(),
            sanitize_schema(value, depth + 1, budget)?,
        );
    }
    if let Some(value) = object.get("additionalProperties") {
        output.insert(
            "additionalProperties".to_string(),
            sanitize_schema(value, depth + 1, budget)?,
        );
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        if let Some(value) = object.get(key) {
            let branches = value
                .as_array()
                .ok_or_else(|| format!("MCP schema {key} must be an array"))?;
            if branches.is_empty() || branches.len() > MAX_BRANCHES {
                return Err(format!("MCP schema {key} has an invalid branch count"));
            }
            let mut sanitized = Vec::with_capacity(branches.len());
            for branch in branches {
                sanitized.push(sanitize_schema(branch, depth + 1, budget)?);
            }
            output.insert(key.to_string(), Value::Array(sanitized));
        }
    }
    if let Some(value) = object.get("not") {
        output.insert(
            "not".to_string(),
            sanitize_schema(value, depth + 1, budget)?,
        );
    }
    for key in [
        "minLength",
        "maxLength",
        "minimum",
        "maximum",
        "exclusiveMinimum",
        "exclusiveMaximum",
        "multipleOf",
        "minItems",
        "maxItems",
        "minProperties",
        "maxProperties",
    ] {
        if let Some(value) = object.get(key) {
            if !value.is_number() {
                return Err(format!("MCP schema {key} must be numeric"));
            }
            output.insert(key.to_string(), value.clone());
        }
    }
    if let Some(value) = object.get("uniqueItems") {
        if !value.is_boolean() {
            return Err("MCP schema uniqueItems must be boolean".to_string());
        }
        output.insert("uniqueItems".to_string(), value.clone());
    }
    Ok(ordered_object(output))
}

fn sanitize_type(value: &Value) -> Result<Value, String> {
    const TYPES: &[&str] = &[
        "object", "array", "string", "number", "integer", "boolean", "null",
    ];
    match value {
        Value::String(value) if TYPES.contains(&value.as_str()) => Ok(Value::String(value.clone())),
        Value::Array(values) if !values.is_empty() && values.len() <= TYPES.len() => {
            let mut seen = HashSet::with_capacity(values.len());
            for value in values {
                let value = value
                    .as_str()
                    .ok_or_else(|| "MCP schema type array must contain strings".to_string())?;
                if !TYPES.contains(&value) || !seen.insert(value) {
                    return Err("MCP schema type array is invalid".to_string());
                }
            }
            Ok(Value::Array(values.clone()))
        }
        _ => Err("MCP schema type is invalid".to_string()),
    }
}

fn validate_property_name(name: &str) -> Result<(), String> {
    let mut bytes = name.bytes();
    let first = bytes
        .next()
        .ok_or_else(|| "MCP schema property name is empty".to_string())?;
    if name.len() > MAX_PROPERTY_BYTES
        || !(first.is_ascii_alphabetic() || first == b'_')
        || !bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("MCP schema property name is not a safe identifier".to_string());
    }
    Ok(())
}

fn ordered_object(values: BTreeMap<String, Value>) -> Value {
    Value::Object(values.into_iter().collect())
}

fn descriptor_digest(descriptors: &[ToolDescriptor]) -> Result<String, String> {
    let value = Value::Array(
        descriptors
            .iter()
            .map(|descriptor| {
                serde_json::json!({
                    "name": descriptor.name,
                    "inputSchema": descriptor.input_schema,
                })
            })
            .collect(),
    );
    canonical_json_digest(&value)
}

pub(crate) fn canonical_json_digest(value: &Value) -> Result<String, String> {
    let canonical = canonicalize(value);
    let encoded = serde_json::to_vec(&canonical)
        .map_err(|error| format!("encode canonical JSON: {error}"))?;
    Ok(crate::crypto::sha256_hex(&encoded))
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(canonicalize).collect()),
        Value::Object(values) => {
            let ordered = values
                .iter()
                .map(|(key, value)| (key.clone(), canonicalize(value)))
                .collect::<BTreeMap<_, _>>();
            ordered_object(ordered)
        }
        _ => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/mcp/descriptor.rs"
    ));
}
