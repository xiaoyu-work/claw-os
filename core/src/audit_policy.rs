//! The single redaction policy every durable audit projection applies.
//!
//! Broker requests, agent turns and tool calls all carry caller- or
//! model-controlled data: request `params`, malformed request bodies,
//! handler error text, tool input. Those values reach four durable
//! sinks — the broker audit log, the user-visible system operations
//! journal, the chained agent audit log and the session mutation log —
//! so serializing any of them verbatim writes credentials, OAuth codes,
//! launch handles, prompts and arbitrary nested caller data to disk
//! where they outlive the request.
//!
//! This module is the only place that defines how approved fields are
//! projected. The allowlist itself is never a denylist of suspicious
//! names: each broker route carries its own field rules, while tools
//! use the table below. Anything without a rule contributes no payload,
//! and there is no fallback that serializes the raw value.
//!
//! What survives is bounded metadata: identities the broker itself
//! produced, validated resource identifiers, enumerated selectors,
//! booleans, counts, sizes and — for the strings that may never be
//! stored — a byte length plus a per-process keyed digest so repeats
//! can be correlated without the text being recoverable.
//!
//! Consumers: [`crate::clawd::audit`], [`crate::clawd::system_journal`],
//! [`crate::agent::runtime::hooks`]. They must project through this
//! module rather than reimplement masking, so no sink can sanitize one
//! copy of a value while another sink writes it in the clear.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

/// Recorded in place of a command the policy does not recognise.
pub const UNRECOGNIZED: &str = "<unrecognized>";
/// Recorded in place of a value that failed its field rule.
pub const UNLOGGABLE: &str = "<unloggable>";
/// Recorded for an enumerated field whose value is not one of the
/// literals the route classified as safe.
pub const OTHER: &str = "<other>";
/// Error class used when a handler message carries no classification,
/// meaning the message itself must not be persisted.
pub const UNCLASSIFIED: &str = "unclassified";

/// Longest value accepted by [`FieldRule::Token`].
const MAX_TOKEN_BYTES: usize = 64;
/// Longest value accepted by [`FieldRule::Identifier`].
const MAX_IDENTIFIER_BYTES: usize = 256;
/// Hex characters kept from a keyed digest. 16 nibbles (64 bits) is
/// ample for correlating repeats and useless for enumeration.
const DIGEST_HEX_LEN: usize = 16;

// ---------------------------------------------------------------------------
// Bounded text
// ---------------------------------------------------------------------------

/// A string that may never be persisted, reduced to its length and a
/// keyed digest.
///
/// The digest key is random per process, so two records from the same
/// daemon can be recognised as the same text while nothing outside that
/// process — including anyone holding the log — can test a guess
/// against it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextDigest {
    pub bytes: usize,
    pub digest: String,
}

pub fn text_digest(value: &str) -> TextDigest {
    TextDigest {
        bytes: value.len(),
        digest: keyed_digest(value.as_bytes()),
    }
}

pub fn optional_text_digest(value: Option<&str>) -> Option<TextDigest> {
    value.map(text_digest)
}

fn keyed_digest(data: &[u8]) -> String {
    static KEY: OnceLock<[u8; 16]> = OnceLock::new();
    let key = KEY.get_or_init(|| *uuid::Uuid::new_v4().as_bytes());
    let mut hex = crate::crypto::hmac_sha256_hex(key, data);
    hex.truncate(DIGEST_HEX_LEN);
    hex
}

// ---------------------------------------------------------------------------
// Field rules
// ---------------------------------------------------------------------------

/// How one allowlisted field may be projected.
///
/// Every rule is bounded. A value that does not satisfy its rule is
/// replaced by a shape summary (type plus length), never by the value
/// itself and never by a truncation of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldRule {
    /// Short selector: 1..=64 bytes of `[A-Za-z0-9._-]`. Actions,
    /// statuses, job ids, session ids.
    Token,
    /// Resource identifier: 1..=256 bytes of `[A-Za-z0-9._:/@+~-]`.
    /// Unit names, package names, absolute system paths the owning
    /// route has classified as safe.
    Identifier,
    /// Recorded only when it equals one of these literals; anything
    /// else becomes [`OTHER`].
    Enum(&'static [&'static str]),
    /// Boolean.
    Flag,
    /// Non-negative integer.
    Count,
    /// Container or string recorded as its element count or byte
    /// length only. Use for fields whose contents are never safe but
    /// whose size is useful.
    Size,
}

impl FieldRule {
    fn project(self, value: &Value) -> Value {
        match self {
            FieldRule::Token => project_str(value, is_token),
            FieldRule::Identifier => project_str(value, is_identifier),
            FieldRule::Enum(allowed) => match value.as_str() {
                Some(text) if allowed.contains(&text) => Value::String(text.to_string()),
                Some(_) => Value::String(OTHER.to_string()),
                None => shape(value),
            },
            FieldRule::Flag => match value.as_bool() {
                Some(flag) => Value::Bool(flag),
                None => shape(value),
            },
            FieldRule::Count => match value.as_u64() {
                Some(count) => json!(count),
                None => shape(value),
            },
            FieldRule::Size => shape(value),
        }
    }
}

fn project_str(value: &Value, accept: fn(&str) -> bool) -> Value {
    match value.as_str() {
        Some(text) if accept(text) => Value::String(text.to_string()),
        Some(_) => Value::String(UNLOGGABLE.to_string()),
        None => shape(value),
    }
}

/// Type and size of a value, with none of its contents.
fn shape(value: &Value) -> Value {
    match value {
        Value::Null => json!({"type": "null"}),
        Value::Bool(_) => json!({"type": "bool"}),
        Value::Number(_) => json!({"type": "number"}),
        Value::String(text) => json!({"type": "string", "bytes": text.len()}),
        Value::Array(items) => json!({"type": "array", "len": items.len()}),
        Value::Object(map) => json!({"type": "object", "len": map.len()}),
    }
}

pub fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TOKEN_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_BYTES
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/' | b'@' | b'+' | b'~')
        })
}

/// Project a value under [`FieldRule::Token`], for broker-generated
/// identifiers that should still be bounded before they are stored.
fn token_or_unloggable(value: &str) -> String {
    if is_token(value) {
        value.to_string()
    } else {
        UNLOGGABLE.to_string()
    }
}

fn project_fields(fields: &'static [(&'static str, FieldRule)], value: &Value) -> (Value, usize) {
    let Some(source) = value.as_object() else {
        let omitted = usize::from(!value.is_null());
        return (Value::Object(Map::new()), omitted);
    };
    let mut out = Map::new();
    for (name, rule) in fields {
        if let Some(field) = source.get(*name) {
            out.insert((*name).to_string(), rule.project(field));
        }
    }
    let omitted = source.len().saturating_sub(out.len());
    (Value::Object(out), omitted)
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

// ---------------------------------------------------------------------------
// Broker request policy
// ---------------------------------------------------------------------------

// Every command's safe fields live on its typed `clawd` route
// descriptor. This module owns only the projection rules, so adding a
// command never requires updating a second name-indexed table.
//
// Fields absent from a route's metadata are never written in any form,
// only counted. This covers bearer handles and tokens, credential and
// password material, prompts, nested payloads, and free-text reasons.

/// Everything a durable record may say about a broker request.
#[derive(Debug, Clone, Serialize)]
pub struct RequestFacts {
    /// The policy's own static name for the command, or
    /// [`UNRECOGNIZED`]. Never the caller's string.
    pub command: &'static str,
    /// Length and keyed digest of an unrecognised command name, so an
    /// unknown route is still countable and correlatable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_text: Option<TextDigest>,
    /// Allowlisted parameter fields, always a JSON object.
    pub params: Value,
    /// JSON type the caller actually sent for `params`.
    pub params_kind: &'static str,
    /// Top-level parameter keys the policy refused to record.
    pub params_omitted: usize,
}

pub fn request_facts(command: &str, params: &Value) -> RequestFacts {
    let Some(route) = crate::clawd::routes::Command::parse(command).map(|command| command.route())
    else {
        return unrecognized_request_facts(command, params);
    };
    request_facts_for_route(route.name, route.audit_fields, params)
}

pub fn request_facts_for_route(
    command: &'static str,
    fields: &'static [(&'static str, FieldRule)],
    params: &Value,
) -> RequestFacts {
    let (fields, omitted) = project_fields(fields, params);
    RequestFacts {
        command,
        command_text: None,
        params: fields,
        params_kind: value_kind(params),
        params_omitted: omitted,
    }
}

fn unrecognized_request_facts(command: &str, params: &Value) -> RequestFacts {
    let (fields, omitted) = project_fields(&[], params);
    RequestFacts {
        command: UNRECOGNIZED,
        command_text: Some(text_digest(command)),
        params: fields,
        params_kind: value_kind(params),
        params_omitted: omitted,
    }
}

/// Everything a durable record may say about a request the broker
/// refused before it could run.
///
/// The frame is not recorded in any form — not verbatim, not as a
/// digest. A refused frame is unparsed caller input that may be a
/// credential, an ancillary payload or a fragment of another protocol,
/// and its size and classification are enough to count and correlate
/// probes. The route name, when one was resolved, is the registry's own
/// `&'static str`, never the caller's string.
#[derive(Debug, Clone, Serialize)]
pub struct ProtocolFailureFacts {
    /// Stable class from `clawd::wire::Fault`.
    pub class: &'static str,
    /// Bytes the daemon had accepted when it refused.
    pub bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<&'static str>,
}

pub fn protocol_failure_facts(
    class: &'static str,
    bytes: usize,
    command: Option<&'static str>,
) -> ProtocolFailureFacts {
    ProtocolFailureFacts {
        class,
        bytes,
        command,
    }
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// A broker failure reduced to what may be stored.
///
/// `code` is broker-generated and stable. `class` is the route's own
/// classification of the failure, set through
/// [`crate::clawd::protocol::BrokerError::classified`]; without one the
/// message is treated as caller-derived and only its size and keyed
/// digest survive.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorFacts {
    pub code: String,
    pub class: &'static str,
    pub message: TextDigest,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResponseFacts {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorFacts>,
}

pub fn error_facts(code: &str, class: Option<&'static str>, message: &str) -> ErrorFacts {
    ErrorFacts {
        code: token_or_unloggable(code),
        class: class.unwrap_or(UNCLASSIFIED),
        message: text_digest(message),
    }
}

// ---------------------------------------------------------------------------
// Tool policy
// ---------------------------------------------------------------------------

/// The audit policy for one agent tool.
pub struct ToolPolicy {
    pub tool: &'static str,
    pub fields: &'static [(&'static str, FieldRule)],
}

/// Tool input fields that are safe to persist in the session mutation
/// log and the agent audit trail.
///
/// Everything else is model-authored text — prompts, queries, note
/// contents, clarification questions, file paths the model chose — and
/// is never stored. App-proxy and MCP tools have registry-derived names
/// and caller-defined schemas, so they match no row and contribute no
/// input at all.
#[rustfmt::skip]
const TOOL_POLICIES: &[ToolPolicy] = &[
    ToolPolicy { tool: "echo", fields: &[("text", FieldRule::Size)] },
    ToolPolicy { tool: "now", fields: &[] },
    ToolPolicy { tool: "cos_clarify", fields: &[("options", FieldRule::Size)] },
    ToolPolicy { tool: "cos_help", fields: &[("path", FieldRule::Size)] },
    ToolPolicy {
        tool: "cos_usage",
        fields: &[("command", FieldRule::Token), ("args", FieldRule::Size)],
    },
    ToolPolicy {
        tool: "cos_app_catalog",
        fields: &[("command", FieldRule::Token), ("args", FieldRule::Size)],
    },
    ToolPolicy {
        tool: "cos_app_run",
        fields: &[
            ("app", FieldRule::Token),
            ("command", FieldRule::Token),
            ("args", FieldRule::Size),
        ],
    },
    ToolPolicy { tool: "cos_app_session_open", fields: &[("app", FieldRule::Token)] },
    ToolPolicy { tool: "cos_app_session_close", fields: &[("app", FieldRule::Token)] },
    ToolPolicy {
        tool: "cos_delegate",
        fields: &[
            ("provider", FieldRule::Token),
            ("model", FieldRule::Token),
            ("allowed_tools", FieldRule::Size),
            ("task", FieldRule::Size),
        ],
    },
    ToolPolicy {
        tool: "cos_skill",
        fields: &[
            ("command", FieldRule::Token),
            ("id", FieldRule::Token),
            ("offset", FieldRule::Count),
        ],
    },
    ToolPolicy {
        tool: "cos_todo",
        fields: &[
            ("command", FieldRule::Token),
            ("session_id", FieldRule::Token),
            ("id", FieldRule::Token),
            ("status", FieldRule::Token),
            ("items", FieldRule::Size),
        ],
    },
    ToolPolicy {
        tool: "cos_tts",
        fields: &[
            ("provider", FieldRule::Token),
            ("voice", FieldRule::Token),
            ("format", FieldRule::Token),
            ("text", FieldRule::Size),
        ],
    },
    ToolPolicy {
        tool: "cos_stt",
        fields: &[
            ("provider", FieldRule::Token),
            ("language", FieldRule::Token),
            ("format", FieldRule::Token),
        ],
    },
    ToolPolicy {
        tool: "cos_imagegen",
        fields: &[
            ("provider", FieldRule::Token),
            ("width", FieldRule::Count),
            ("height", FieldRule::Count),
            ("steps", FieldRule::Count),
            ("n", FieldRule::Count),
            ("prompt", FieldRule::Size),
        ],
    },
    ToolPolicy { tool: "cos_memory", fields: &[("command", FieldRule::Token)] },
    ToolPolicy {
        tool: "cos_app_memory",
        fields: &[
            ("command", FieldRule::Token),
            ("source", FieldRule::Token),
            ("kind", FieldRule::Token),
        ],
    },
    ToolPolicy {
        tool: "cos_recall",
        fields: &[
            ("command", FieldRule::Token),
            ("session_id", FieldRule::Token),
            ("limit", FieldRule::Count),
        ],
    },
    ToolPolicy {
        tool: "cos_recall_semantic",
        fields: &[
            ("command", FieldRule::Token),
            ("session_id", FieldRule::Token),
            ("limit", FieldRule::Count),
        ],
    },
    ToolPolicy {
        tool: "cos_oauth_login",
        fields: &[
            ("provider", FieldRule::Enum(&["google", "microsoft"])),
            ("timeout_seconds", FieldRule::Count),
        ],
    },
];

pub fn tool_policy(tool: &str) -> Option<&'static ToolPolicy> {
    TOOL_POLICIES.iter().find(|policy| policy.tool == tool)
}

/// Everything a durable record may say about a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolFacts {
    /// Bounded tool name: the policy's static name when known, the
    /// registry name when it is a well-formed token, otherwise
    /// [`UNLOGGABLE`].
    pub tool: String,
    /// Whether the name matched an entry in [`TOOL_POLICIES`].
    pub known: bool,
    /// Allowlisted input fields, always a JSON object.
    pub input: Value,
    /// Top-level input keys the policy refused to record.
    pub input_omitted: usize,
}

pub fn tool_facts(tool: &str, input: &Value) -> ToolFacts {
    match tool_policy(tool) {
        Some(policy) => {
            let (fields, omitted) = project_fields(policy.fields, input);
            ToolFacts {
                tool: policy.tool.to_string(),
                known: true,
                input: fields,
                input_omitted: omitted,
            }
        }
        None => {
            let (fields, omitted) = project_fields(&[], input);
            ToolFacts {
                tool: token_or_unloggable(tool),
                known: false,
                input: fields,
                input_omitted: omitted,
            }
        }
    }
}

/// Re-apply the projection to facts that arrived from another process.
///
/// An `agentd` worker computes its own [`ToolFacts`], so nothing it
/// sends may be written verbatim: the tool name is re-bounded and the
/// recorded input is re-projected through the same policy, which drops
/// any field the policy does not allow no matter what the sender
/// claimed. The reported omission count is kept when it is larger, so a
/// worker cannot understate what it withheld either.
pub fn reproject_tool_facts(facts: &ToolFacts) -> ToolFacts {
    let mut reprojected = tool_facts(&facts.tool, &facts.input);
    reprojected.input_omitted = reprojected.input_omitted.max(facts.input_omitted);
    reprojected
}

/// Bound an identifier the broker or a provider produced (a tool-use
/// id, a stop reason) before it is written to a durable record.
pub fn safe_identity(value: &str) -> String {
    token_or_unloggable(value)
}

/// Bound a reference that legitimately carries separators — a session
/// name, a `uid:…` requester string, a resource path a route has
/// classified — before it is written to a durable record.
pub fn safe_reference(value: &str) -> String {
    if is_identifier(value) {
        value.to_string()
    } else {
        UNLOGGABLE.to_string()
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/audit_policy.rs"
    ));
}
