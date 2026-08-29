//! Budget-driven progressive disclosure for non-core tool catalogs.
//!
//! Core tools remain directly visible. When the visible extension schema
//! exceeds the configured budget, the provider receives three fixed bridge
//! schemas while discovery and invocation are resolved against the current
//! trusted [`super::exposure::ToolExposureContext`].

use serde_json::{json, Map, Value};

use crate::agent::context::compressor::estimate_tools_tokens;
use crate::agent::llm::{Tool as LlmTool, ToolCall};

use super::ToolResult;

pub const TOOL_SEARCH: &str = "cos_tool_search";
pub const TOOL_DESCRIBE: &str = "cos_tool_describe";
pub const TOOL_CALL: &str = "cos_tool_call";
pub const DEFAULT_TOOL_SCHEMA_BUDGET_TOKENS: u32 = 8_192;

const MAX_QUERY_CHARS: usize = 256;
const MAX_FILTER_CHARS: usize = 128;
const MAX_TAGS: usize = 8;
const MAX_TAG_CHARS: usize = 64;
const DEFAULT_SEARCH_LIMIT: usize = 8;
const MAX_SEARCH_LIMIT: usize = 25;
const MAX_RESULT_DESCRIPTION_CHARS: usize = 400;
const MAX_TOOL_NAME_CHARS: usize = 512;

/// Immutable discovery metadata cached beside a tool descriptor.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ToolDisclosure {
    pub defer_eligible: bool,
    pub source: Option<String>,
    pub server: Option<String>,
    pub remote_name: Option<String>,
    pub tags: Vec<String>,
}

impl ToolDisclosure {
    pub fn extension(
        source: impl Into<String>,
        server: Option<String>,
        remote_name: Option<String>,
        tags: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut tags = tags.into_iter().collect::<Vec<_>>();
        tags.sort();
        tags.dedup();
        Self {
            defer_eligible: true,
            source: Some(source.into()),
            server,
            remote_name,
            tags,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct CatalogEntry {
    pub descriptor: LlmTool,
    pub disclosure: ToolDisclosure,
}

pub fn is_bridge_tool(name: &str) -> bool {
    matches!(name, TOOL_SEARCH | TOOL_DESCRIBE | TOOL_CALL)
}

/// Stable provider-facing bridge descriptors. Their contents never depend on
/// the attached catalog, preserving prompt-cache identity between turns.
pub fn bridge_tools() -> Vec<LlmTool> {
    vec![
        LlmTool {
            name: TOOL_SEARCH.to_string(),
            description: format!(
                "Search the currently attached and permitted extension tools by name, \
                 description, server, or tags. Use `{TOOL_DESCRIBE}` for the exact schema \
                 and `{TOOL_CALL}` to invoke a result."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Capability, tool name, or keyword to search for.",
                        "maxLength": MAX_QUERY_CHARS,
                    },
                    "server": {
                        "type": "string",
                        "description": "Optional exact server filter.",
                        "maxLength": MAX_FILTER_CHARS,
                    },
                    "tags": {
                        "type": "array",
                        "description": "Optional tags that every result must contain.",
                        "items": {"type": "string", "maxLength": MAX_TAG_CHARS},
                        "maxItems": MAX_TAGS,
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_SEARCH_LIMIT,
                        "default": DEFAULT_SEARCH_LIMIT,
                    },
                },
                "required": ["query"],
                "additionalProperties": false,
            }),
        },
        LlmTool {
            name: TOOL_DESCRIBE.to_string(),
            description: format!(
                "Return the exact current JSON schema and discovery metadata for one \
                 extension tool returned by `{TOOL_SEARCH}`."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Exact registered tool name.",
                        "maxLength": MAX_TOOL_NAME_CHARS,
                    },
                },
                "required": ["name"],
                "additionalProperties": false,
            }),
        },
        LlmTool {
            name: TOOL_CALL.to_string(),
            description: format!(
                "Invoke one currently attached and permitted deferred extension tool. \
                 Use the exact name and arguments from `{TOOL_DESCRIBE}`. The underlying \
                 tool keeps its normal guardrails, capabilities, approval, audit, timeout, \
                 cancellation, and result-safety behavior."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Exact registered tool name.",
                        "maxLength": MAX_TOOL_NAME_CHARS,
                    },
                    "arguments": {
                        "type": "object",
                        "description": "Arguments matching the described tool schema.",
                    },
                },
                "required": ["name", "arguments"],
                "additionalProperties": false,
            }),
        },
    ]
}

pub fn schema_tokens(tools: &[LlmTool]) -> u32 {
    estimate_tools_tokens(tools)
}

pub(crate) fn search_tools(
    deferred: &[CatalogEntry],
    catalog_generation: u64,
    input: &Value,
) -> ToolResult {
    let object = match input_object(
        input,
        &[
            ("query", true),
            ("server", false),
            ("tags", false),
            ("limit", false),
        ],
    ) {
        Ok(object) => object,
        Err(error) => return ToolResult::err(error),
    };
    let query = match bounded_string(object, "query", MAX_QUERY_CHARS, true) {
        Ok(query) => query,
        Err(error) => return ToolResult::err(error),
    };
    let server = match optional_bounded_string(object, "server", MAX_FILTER_CHARS) {
        Ok(server) => server,
        Err(error) => return ToolResult::err(error),
    };
    let tags = match string_array(object, "tags", MAX_TAGS, MAX_TAG_CHARS) {
        Ok(tags) => tags,
        Err(error) => return ToolResult::err(error),
    };
    let limit = match object.get("limit") {
        Some(Value::Number(value)) => match value.as_u64() {
            Some(value) if (1..=MAX_SEARCH_LIMIT as u64).contains(&value) => value as usize,
            _ => {
                return ToolResult::err(format!(
                    "`limit` must be an integer between 1 and {MAX_SEARCH_LIMIT}"
                ))
            }
        },
        Some(_) => return ToolResult::err("`limit` must be an integer"),
        None => DEFAULT_SEARCH_LIMIT,
    };

    let server_filter = server.as_deref().map(str::to_ascii_lowercase);
    let tag_filters = tags
        .iter()
        .map(|tag| tag.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut matches = deferred
        .iter()
        .filter(|entry| {
            server_filter.as_ref().is_none_or(|expected| {
                entry
                    .disclosure
                    .server
                    .as_deref()
                    .is_some_and(|actual| actual.to_ascii_lowercase() == *expected)
            }) && tag_filters.iter().all(|expected| {
                entry
                    .disclosure
                    .tags
                    .iter()
                    .any(|actual| actual.eq_ignore_ascii_case(expected))
            })
        })
        .filter_map(|entry| {
            let score = search_score(entry, &query);
            (score > 0).then_some((score, entry))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.descriptor.name.cmp(&right.descriptor.name))
    });

    let matches = matches
        .into_iter()
        .take(limit)
        .map(|(_, entry)| {
            json!({
                "name": entry.descriptor.name,
                "remote_name": entry.disclosure.remote_name,
                "description": truncate(&entry.descriptor.description, MAX_RESULT_DESCRIPTION_CHARS),
                "source": entry.disclosure.source,
                "server": entry.disclosure.server,
                "tags": entry.disclosure.tags,
                "required": required_fields(&entry.descriptor),
            })
        })
        .collect::<Vec<_>>();

    catalog_result(json!({
        "catalog_generation": catalog_generation,
        "query": query,
        "server": server,
        "tags": tags,
        "total_available": deferred.len(),
        "matches": matches,
        "hint": format!("Use `{TOOL_DESCRIBE}` for the exact schema, then `{TOOL_CALL}`."),
    }))
}

pub(crate) fn describe_tool(
    deferred: &[CatalogEntry],
    catalog_generation: u64,
    input: &Value,
) -> ToolResult {
    let object = match input_object(input, &[("name", true)]) {
        Ok(object) => object,
        Err(error) => return ToolResult::err(error),
    };
    let name = match bounded_string(object, "name", MAX_TOOL_NAME_CHARS, true) {
        Ok(name) => name,
        Err(error) => return ToolResult::err(error),
    };
    let Some(entry) = deferred.iter().find(|entry| entry.descriptor.name == name) else {
        return ToolResult::err(format!(
            "tool `{name}` is not available in the current deferred catalog"
        ));
    };

    catalog_result(json!({
        "catalog_generation": catalog_generation,
        "name": entry.descriptor.name,
        "remote_name": entry.disclosure.remote_name,
        "description": entry.descriptor.description,
        "source": entry.disclosure.source,
        "server": entry.disclosure.server,
        "tags": entry.disclosure.tags,
        "input_schema": entry.descriptor.input_schema,
    }))
}

pub fn resolve_call_envelope(input: &Value) -> Result<(String, Value), String> {
    let object = input_object(input, &[("name", true), ("arguments", true)])?;
    let name = bounded_string(object, "name", MAX_TOOL_NAME_CHARS, true)?;
    if is_bridge_tool(&name) {
        return Err(format!("`{TOOL_CALL}` cannot invoke bridge tool `{name}`"));
    }
    let arguments = object
        .get("arguments")
        .and_then(Value::as_object)
        .cloned()
        .map(Value::Object)
        .ok_or_else(|| "`arguments` must be a JSON object".to_string())?;
    Ok((name, arguments))
}

pub fn resolve_visible_identity(name: &str, input: &Value) -> Option<(String, Value)> {
    (name == TOOL_CALL)
        .then(|| resolve_call_envelope(input).ok())
        .flatten()
}

pub fn resolved_tool_call(call: &ToolCall, target_name: String, input: Value) -> ToolCall {
    ToolCall {
        id: call.id.clone(),
        name: target_name,
        input,
    }
}

fn catalog_result(value: Value) -> ToolResult {
    ToolResult::ok(crate::agent::safety::untrusted::wrap_untrusted(
        crate::agent::safety::untrusted::TOOL_RESULT_TAG,
        &value.to_string(),
    ))
}

fn input_object<'a>(
    input: &'a Value,
    fields: &[(&str, bool)],
) -> Result<&'a Map<String, Value>, String> {
    let object = input
        .as_object()
        .ok_or_else(|| "tool input must be a JSON object".to_string())?;
    for key in object.keys() {
        if !fields.iter().any(|(field, _)| key == field) {
            return Err(format!("unknown field `{key}`"));
        }
    }
    for (field, required) in fields {
        if *required && !object.contains_key(*field) {
            return Err(format!("`{field}` is required"));
        }
    }
    Ok(object)
}

fn bounded_string(
    object: &Map<String, Value>,
    key: &str,
    max_chars: usize,
    non_empty: bool,
) -> Result<String, String> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` must be a string"))?
        .trim()
        .to_string();
    if non_empty && value.is_empty() {
        return Err(format!("`{key}` must not be empty"));
    }
    if value.chars().count() > max_chars || value.chars().any(char::is_control) {
        return Err(format!(
            "`{key}` must be at most {max_chars} characters without control characters"
        ));
    }
    Ok(value)
}

fn optional_bounded_string(
    object: &Map<String, Value>,
    key: &str,
    max_chars: usize,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(_) => bounded_string(object, key, max_chars, true).map(Some),
    }
}

fn string_array(
    object: &Map<String, Value>,
    key: &str,
    max_items: usize,
    max_chars: usize,
) -> Result<Vec<String>, String> {
    let Some(value) = object.get(key) else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("`{key}` must be an array of strings"))?;
    if values.len() > max_items {
        return Err(format!("`{key}` accepts at most {max_items} values"));
    }
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let value = value
            .as_str()
            .ok_or_else(|| format!("`{key}` must contain only strings"))?
            .trim();
        if value.is_empty()
            || value.chars().count() > max_chars
            || value.chars().any(char::is_control)
        {
            return Err(format!(
                "`{key}` values must be non-empty and at most {max_chars} characters"
            ));
        }
        output.push(value.to_string());
    }
    output.sort();
    output.dedup();
    Ok(output)
}

fn search_score(entry: &CatalogEntry, query: &str) -> usize {
    if query == "*" {
        return 1;
    }
    let query = query.to_ascii_lowercase();
    let name = entry.descriptor.name.to_ascii_lowercase();
    let remote_name = entry
        .disclosure
        .remote_name
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let description = entry.descriptor.description.to_ascii_lowercase();
    let server = entry
        .disclosure
        .server
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let source = entry
        .disclosure
        .source
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let tags = entry
        .disclosure
        .tags
        .iter()
        .map(|tag| tag.to_ascii_lowercase())
        .collect::<Vec<_>>();

    let mut score = 0;
    if name == query {
        score += 2_000;
    }
    if remote_name == query {
        score += 1_500;
    }
    if name.contains(&query) {
        score += 400;
    }
    if remote_name.contains(&query) {
        score += 300;
    }
    for term in query_terms(&query) {
        if name.contains(&term) {
            score += 80;
        }
        if remote_name.contains(&term) {
            score += 70;
        }
        if server.contains(&term) {
            score += 50;
        }
        if source.contains(&term) {
            score += 30;
        }
        if tags.iter().any(|tag| tag.contains(&term)) {
            score += 30;
        }
        if description.contains(&term) {
            score += 20;
        }
    }
    score
}

fn query_terms(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn required_fields(tool: &LlmTool) -> Vec<String> {
    tool.input_schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn truncate(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut output = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/progressive.rs"
    ));
}
