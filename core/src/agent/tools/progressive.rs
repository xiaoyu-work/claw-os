//! Stable progressive disclosure for non-core model tools.
//!
//! Provider requests keep a fixed bridge surface while App and MCP schemas stay
//! behind a searchable catalog. The registry and runtime still execute the
//! underlying tool directly so guardrails, approval, hooks, progress, evidence,
//! and audit retain the real tool identity.

use serde_json::{json, Value};

use crate::agent::llm::{Tool as LlmTool, ToolCall};

use super::ToolResult;

pub const TOOL_SEARCH: &str = "cos_tool_search";
pub const TOOL_DESCRIBE: &str = "cos_tool_describe";
pub const TOOL_CALL: &str = "cos_tool_call";

const MAX_SEARCH_QUERIES: usize = 10;
const MAX_DESCRIBE_NAMES: usize = 10;
const DEFAULT_SEARCH_LIMIT: usize = 5;
const MAX_SEARCH_LIMIT: usize = 25;
const MAX_FUNCTION_DESCRIPTION_CHARS: usize = 1024;

pub fn is_bridge_tool(name: &str) -> bool {
    matches!(name, TOOL_SEARCH | TOOL_DESCRIBE | TOOL_CALL)
}

/// Initial rollout defers extensible App/MCP surfaces while keeping kernel
/// primitives direct. More core domains can move behind the bridge once their
/// Skills and typed schemas no longer assume direct visibility.
pub fn is_deferred_tool_name(name: &str) -> bool {
    if is_bridge_tool(name) {
        return false;
    }
    if name.starts_with("mcp_") || name.starts_with("app_") {
        return true;
    }
    name.starts_with("cos_app_")
        && !matches!(name, "cos_app_catalog" | "cos_app_run" | "cos_app_memory")
}

pub fn partition_tools(tools: Vec<LlmTool>) -> (Vec<LlmTool>, Vec<LlmTool>) {
    tools
        .into_iter()
        .partition(|tool| !is_deferred_tool_name(&tool.name))
}

pub fn bridge_tools(deferred: &[LlmTool]) -> Vec<LlmTool> {
    if deferred.is_empty() {
        return Vec::new();
    }
    let search_prefix = format!(
        "Search {} deferred App and MCP tools by capability. Returns matching names, \
         short descriptions, and required fields. Follow with `{TOOL_DESCRIBE}` when \
         the full schema is needed, then invoke through `{TOOL_CALL}`.",
        deferred.len()
    );
    let listing_budget = MAX_FUNCTION_DESCRIPTION_CHARS
        .saturating_sub(search_prefix.chars().count())
        .saturating_sub(2);
    let listing = compact_listing(deferred, listing_budget);
    let search_description = format!("{search_prefix}\n\n{listing}");
    vec![
        LlmTool {
            name: TOOL_SEARCH.to_string(),
            description: truncate_with_ellipsis(
                &search_description,
                MAX_FUNCTION_DESCRIPTION_CHARS,
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "queries": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "maxItems": MAX_SEARCH_QUERIES,
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_SEARCH_LIMIT,
                        "default": DEFAULT_SEARCH_LIMIT,
                    },
                },
                "required": ["queries"],
                "additionalProperties": false,
            }),
        },
        LlmTool {
            name: TOOL_DESCRIBE.to_string(),
            description: format!(
                "Return full JSON schemas for deferred tools found by `{TOOL_SEARCH}`. Batch \
                 every needed name into one call."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "names": {
                        "type": "array",
                        "items": {"type": "string"},
                        "minItems": 1,
                        "maxItems": MAX_DESCRIBE_NAMES,
                    },
                },
                "required": ["names"],
                "additionalProperties": false,
            }),
        },
        LlmTool {
            name: TOOL_CALL.to_string(),
            description: format!(
                "Invoke a deferred tool using the argument object returned by \
                 `{TOOL_DESCRIBE}`. The underlying tool keeps its normal policy, approval, \
                 hooks, audit, progress, and timeout behavior."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "arguments": {"type": "object"},
                },
                "required": ["name", "arguments"],
                "additionalProperties": false,
            }),
        },
    ]
}

fn compact_listing(deferred: &[LlmTool], max_chars: usize) -> String {
    let mut entries = deferred.iter().collect::<Vec<_>>();
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    let mut output = String::from("Deferred tools (full schemas are not loaded):");
    for tool in entries {
        let description = short_description(&tool.description, 96);
        let line = if description.is_empty() {
            format!("\n- {}", tool.name)
        } else {
            format!("\n- {}: {}", tool.name, description)
        };
        if output.chars().count() + line.chars().count() > max_chars {
            let marker = "\n- ... use cos_tool_search for remaining tools";
            if output.chars().count() + marker.chars().count() <= max_chars {
                output.push_str(marker);
            }
            break;
        }
        output.push_str(&line);
    }
    output
}

fn truncate_with_ellipsis(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut output = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    output.push('…');
    output
}

fn short_description(value: &str, max_chars: usize) -> String {
    let first_line = value.lines().next().unwrap_or_default().trim();
    if first_line.chars().count() <= max_chars {
        return first_line.to_string();
    }
    let mut output = first_line.chars().take(max_chars).collect::<String>();
    output.push('…');
    output
}

fn parse_string_list(input: &Value, key: &str, max_items: usize) -> Result<Vec<String>, String> {
    let raw = input
        .get(key)
        .ok_or_else(|| format!("`{key}` is required"))?;
    let values = match raw {
        Value::String(value) => vec![value.clone()],
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("`{key}` must contain only strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(format!("`{key}` must be a string or array of strings")),
    };
    let values = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(format!("`{key}` must contain at least one non-empty value"));
    }
    if values.len() > max_items {
        return Err(format!(
            "`{key}` accepts at most {max_items} values (got {})",
            values.len()
        ));
    }
    Ok(values)
}

fn search_terms(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

fn search_score(tool: &LlmTool, query: &str) -> usize {
    let query = query.trim().to_ascii_lowercase();
    let name = tool.name.to_ascii_lowercase();
    let description = tool.description.to_ascii_lowercase();
    let mut score = 0;
    if name == query {
        score += 1_000;
    } else if !query.is_empty() && name.contains(&query) {
        score += 250;
    }
    for term in search_terms(&query) {
        if name.contains(&term) {
            score += 40;
        }
        if description.contains(&term) {
            score += 10;
        }
    }
    score
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

pub fn search_tools(deferred: &[LlmTool], input: &Value) -> ToolResult {
    let queries = match parse_string_list(input, "queries", MAX_SEARCH_QUERIES) {
        Ok(queries) => queries,
        Err(error) => return ToolResult::err(error),
    };
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);

    let mut results = Vec::with_capacity(queries.len());
    for query in &queries {
        let mut matches = deferred
            .iter()
            .filter_map(|tool| {
                let score = search_score(tool, query);
                (score > 0).then_some((score, tool))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.name.cmp(&right.name))
        });
        let matches = matches
            .into_iter()
            .take(limit)
            .map(|(_, tool)| {
                json!({
                    "name": tool.name,
                    "description": short_description(&tool.description, 400),
                    "required": required_fields(tool),
                })
            })
            .collect::<Vec<_>>();
        results.push(json!({
            "query": query,
            "matches": matches,
        }));
    }

    ToolResult::ok(fence_metadata(
        json!({
            "queries": queries,
            "total_available": deferred.len(),
            "results": results,
            "hint": format!(
                "Use {TOOL_DESCRIBE} for full schemas, then {TOOL_CALL} to invoke."
            ),
        })
        .to_string(),
    ))
}

pub fn describe_tools(deferred: &[LlmTool], input: &Value) -> ToolResult {
    let names = match parse_string_list(input, "names", MAX_DESCRIBE_NAMES) {
        Ok(names) => names,
        Err(error) => return ToolResult::err(error),
    };
    let by_name = deferred
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect::<std::collections::HashMap<_, _>>();
    let mut tools = serde_json::Map::new();
    let mut not_found = Vec::new();
    for name in names {
        match by_name.get(name.as_str()) {
            Some(tool) => {
                tools.insert(
                    name,
                    json!({
                        "description": tool.description,
                        "parameters": tool.input_schema,
                    }),
                );
            }
            None => not_found.push(name),
        }
    }
    ToolResult::ok(fence_metadata(
        json!({
            "tools": tools,
            "not_found": not_found,
        })
        .to_string(),
    ))
}

/// Fence a bridge result.
///
/// A deferred tool's description and JSON schema are authored by an App
/// manifest or a remote MCP server. Surfacing them through a tool result
/// puts third-party text in the model's context, so it is labelled
/// extension metadata and fenced like any other third-party payload.
/// Which tools exist is still decided by the registry, guardrails and
/// the capability authority before the model call; nothing inside this
/// payload can add, rename or unlock one.
fn fence_metadata(body: String) -> String {
    crate::agent::safety::untrusted::wrap_labeled(
        crate::agent::trust::SourceKind::McpToolMetadata,
        None,
        &body,
    )
}

pub fn resolve_call_envelope(input: &Value) -> Result<(String, Value), String> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "`name` is required".to_string())?
        .to_string();
    if is_bridge_tool(&name) {
        return Err(format!("`{TOOL_CALL}` cannot invoke bridge tool `{name}`"));
    }
    let arguments = match input.get("arguments") {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
        Some(Value::String(arguments)) => serde_json::from_str::<Value>(arguments)
            .map_err(|error| format!("`arguments` is not valid JSON: {error}"))?,
        Some(_) => return Err("`arguments` must be an object or JSON object string".into()),
    };
    if !arguments.is_object() {
        return Err("`arguments` must decode to a JSON object".into());
    }
    Ok((name, arguments))
}

pub fn resolve_visible_identity(name: &str, input: &Value) -> Option<(String, Value)> {
    if name != TOOL_CALL {
        return None;
    }
    resolve_call_envelope(input).ok()
}

pub fn validate_required(tool: &LlmTool, input: &Value) -> Result<(), String> {
    let Some(arguments) = input.as_object() else {
        return Err("tool arguments must be a JSON object".into());
    };
    let missing = required_fields(tool)
        .into_iter()
        .filter(|required| !arguments.contains_key(required))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "missing required argument(s) for `{}`: {}. Expected schema: {}",
            tool.name,
            missing.join(", "),
            tool.input_schema
        ))
    }
}

pub fn resolved_tool_call(call: &ToolCall, target_name: String, input: Value) -> ToolCall {
    ToolCall {
        id: call.id.clone(),
        name: target_name,
        input,
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/progressive.rs"
    ));
}
