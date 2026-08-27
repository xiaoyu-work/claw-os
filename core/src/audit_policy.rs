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
//! This module is the only place that decides what may survive. It is
//! an **allowlist**, never a denylist of suspicious names: a command or
//! tool is described by an explicit list of fields the owning route has
//! classified as safe, and anything the policy has never heard of
//! contributes no payload at all. A new route therefore starts safe and
//! stays silent until someone adds a policy entry, and there is no
//! fallback that serializes the raw value.
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

/// The audit policy for one broker command.
pub struct CommandPolicy {
    pub command: &'static str,
    pub fields: &'static [(&'static str, FieldRule)],
}

/// Every command `clawd` dispatches, with the fields its owning route
/// has classified as safe to persist.
///
/// Adding a broker command means adding a row here. Until then the
/// command is audited by outcome alone: no name, no arguments. Fields
/// absent from a row are never written in any form, only counted — that
/// covers `handle` (replayable launch authority), `token` (restore and
/// approval bearer values), `credential`/`password` material, `prompt`,
/// `content`, `payload`, `metadata`, `parent_caps`, `call`, free-text
/// reasons and titles, and every path or host no route has classified.
#[rustfmt::skip]
const COMMAND_POLICIES: &[CommandPolicy] = &[
    CommandPolicy { command: "daemon.health", fields: &[] },
    CommandPolicy { command: "daemon.status", fields: &[] },
    CommandPolicy {
        command: "task.submit",
        fields: &[
            ("session_id", FieldRule::Token),
            ("max_turns", FieldRule::Count),
            ("prompt", FieldRule::Size),
        ],
    },
    CommandPolicy { command: "task.list", fields: &[("status", FieldRule::Token)] },
    CommandPolicy { command: "task.get", fields: &[("id", FieldRule::Token)] },
    CommandPolicy { command: "task.status", fields: &[("id", FieldRule::Token)] },
    CommandPolicy { command: "task.cancel", fields: &[("id", FieldRule::Token)] },
    CommandPolicy { command: "task.stream", fields: &[("id", FieldRule::Token)] },
    CommandPolicy { command: "task.result", fields: &[("id", FieldRule::Token)] },
    CommandPolicy { command: "task.count", fields: &[] },
    CommandPolicy { command: "context.snapshot", fields: &[] },
    CommandPolicy { command: "context.sources", fields: &[] },
    CommandPolicy { command: "context.update", fields: &[("source", FieldRule::Token)] },
    CommandPolicy {
        command: "context.event.append",
        fields: &[
            ("source", FieldRule::Token),
            ("event_type", FieldRule::Token),
            ("app_id", FieldRule::Token),
            ("entity_id", FieldRule::Token),
            ("task_id", FieldRule::Token),
            ("session_id", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "context.event.query",
        fields: &[
            ("source", FieldRule::Token),
            ("event_type", FieldRule::Token),
            ("session_id", FieldRule::Token),
            ("order", FieldRule::Token),
            ("limit", FieldRule::Count),
        ],
    },
    CommandPolicy { command: "permission.pending", fields: &[("limit", FieldRule::Count)] },
    CommandPolicy { command: "permission.recent", fields: &[("limit", FieldRule::Count)] },
    CommandPolicy { command: "permission.status", fields: &[("ids", FieldRule::Size)] },
    CommandPolicy {
        command: "permission.request",
        fields: &[
            ("verb", FieldRule::Identifier),
            ("session", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "permission.decide",
        fields: &[
            ("id", FieldRule::Token),
            ("decision", FieldRule::Token),
            ("owner_uid", FieldRule::Count),
        ],
    },
    CommandPolicy {
        command: "system.operations",
        fields: &[("source", FieldRule::Token), ("limit", FieldRule::Count)],
    },
    CommandPolicy {
        command: "memory.history",
        fields: &[("session_id", FieldRule::Token), ("limit", FieldRule::Count)],
    },
    CommandPolicy { command: "memory.sessions", fields: &[("limit", FieldRule::Count)] },
    // The credential name is an opaque reference, but only the two
    // literals this route can act on are safe to store: anything else
    // is caller text that may be the secret itself.
    CommandPolicy {
        command: "credential.oauth-refresh",
        fields: &[
            ("session", FieldRule::Token),
            ("namespace", FieldRule::Token),
            (
                "credential",
                FieldRule::Enum(&["GOOGLE_ACCESS_TOKEN", "MICROSOFT_ACCESS_TOKEN"]),
            ),
        ],
    },
    CommandPolicy {
        command: "system.audio.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("target", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "system.accessibility.control",
        fields: &[("session", FieldRule::Token), ("action", FieldRule::Token)],
    },
    CommandPolicy {
        command: "system.backup.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("snapshot", FieldRule::Token),
            ("keep_daily", FieldRule::Count),
            ("keep_weekly", FieldRule::Count),
            ("keep_monthly", FieldRule::Count),
        ],
    },
    CommandPolicy {
        command: "system.bluetooth.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("adapter", FieldRule::Token),
            ("device", FieldRule::Token),
            ("pairing_id", FieldRule::Token),
            ("state", FieldRule::Token),
            ("seconds", FieldRule::Count),
        ],
    },
    CommandPolicy {
        command: "system.camera.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("node_id", FieldRule::Token),
            ("format", FieldRule::Token),
            ("width", FieldRule::Count),
            ("height", FieldRule::Count),
        ],
    },
    CommandPolicy {
        command: "system.clipboard.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("mime", FieldRule::Identifier),
        ],
    },
    CommandPolicy {
        command: "system.container.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("runtime", FieldRule::Token),
            ("namespace", FieldRule::Token),
            ("target", FieldRule::Identifier),
            ("signal", FieldRule::Token),
            ("lines", FieldRule::Count),
        ],
    },
    // `target` is the /etc file the editor may change; `source` and
    // `token` are caller paths and restore bearer values.
    CommandPolicy {
        command: "system.config.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("target", FieldRule::Identifier),
            ("confirm", FieldRule::Flag),
        ],
    },
    CommandPolicy {
        command: "system.crash.inspect",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("id", FieldRule::Token),
            ("limit", FieldRule::Count),
            ("since_minutes", FieldRule::Count),
        ],
    },
    CommandPolicy {
        command: "system.desktop.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("app_id", FieldRule::Token),
            ("identifier", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "system.display.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("output", FieldRule::Token),
            ("transform", FieldRule::Token),
            ("percent", FieldRule::Count),
            ("adaptive_sync", FieldRule::Token),
            ("backlight", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "system.events.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("source", FieldRule::Token),
            ("limit", FieldRule::Count),
            ("pid", FieldRule::Count),
        ],
    },
    CommandPolicy {
        command: "system.firewall.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("direction", FieldRule::Token),
            ("interface", FieldRule::Token),
            ("port", FieldRule::Token),
            ("protocol", FieldRule::Token),
            ("rule_action", FieldRule::Token),
            ("rule_id", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "system.hardware.inspect",
        fields: &[("session", FieldRule::Token), ("action", FieldRule::Token)],
    },
    CommandPolicy {
        command: "system.location.query",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("accuracy", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "system.network.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("state", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "system.package.install",
        fields: &[
            ("session", FieldRule::Token),
            ("mutation_session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("package", FieldRule::Identifier),
            ("version", FieldRule::Identifier),
        ],
    },
    CommandPolicy {
        command: "system.package.control",
        fields: &[
            ("session", FieldRule::Token),
            ("mutation_session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("package", FieldRule::Identifier),
            ("version", FieldRule::Identifier),
        ],
    },
    CommandPolicy {
        command: "system.package.restore",
        fields: &[
            ("session", FieldRule::Token),
            ("mutation_session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("package", FieldRule::Identifier),
            ("previous_version", FieldRule::Identifier),
            ("was_held", FieldRule::Flag),
        ],
    },
    CommandPolicy {
        command: "system.power.control",
        fields: &[("session", FieldRule::Token), ("action", FieldRule::Token)],
    },
    CommandPolicy {
        command: "system.printer.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("printer", FieldRule::Token),
            ("job_id", FieldRule::Token),
            ("media", FieldRule::Token),
            ("sides", FieldRule::Token),
            ("copies", FieldRule::Count),
        ],
    },
    CommandPolicy {
        command: "system.security.inspect",
        fields: &[("session", FieldRule::Token), ("action", FieldRule::Token)],
    },
    CommandPolicy {
        command: "system.service.control",
        fields: &[
            ("session", FieldRule::Token),
            ("mutation_session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("unit", FieldRule::Identifier),
        ],
    },
    CommandPolicy {
        command: "system.service.restore",
        fields: &[
            ("session", FieldRule::Token),
            ("mutation_session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("unit", FieldRule::Identifier),
            ("active", FieldRule::Flag),
            ("enabled", FieldRule::Flag),
        ],
    },
    CommandPolicy {
        command: "system.snapshot.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("id", FieldRule::Token),
        ],
    },
    CommandPolicy {
        command: "system.storage.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("device", FieldRule::Identifier),
        ],
    },
    CommandPolicy {
        command: "system.usb.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("device", FieldRule::Token),
            ("rule_id", FieldRule::Token),
            ("state", FieldRule::Token),
        ],
    },
    // `user` and `group` are account names the route acts on;
    // `full_name`, `shell`, `groups`, `credential` and `token` are
    // personal data or authority and are never stored.
    CommandPolicy {
        command: "system.users.control",
        fields: &[
            ("session", FieldRule::Token),
            ("action", FieldRule::Token),
            ("user", FieldRule::Token),
            ("group", FieldRule::Token),
        ],
    },
    CommandPolicy { command: "transaction.begin", fields: &[] },
    CommandPolicy { command: "transaction.list", fields: &[] },
    CommandPolicy { command: "transaction.commit", fields: &[("id", FieldRule::Token)] },
    CommandPolicy { command: "transaction.rollback", fields: &[("id", FieldRule::Token)] },
    // App-session routes: the launch handle is bearer authority, the
    // MCP `command` is a child command line, and `call`/`parent_caps`
    // are arbitrary nested caller data.
    CommandPolicy {
        command: "app_session.register",
        fields: &[
            ("app_id", FieldRule::Token),
            ("kind", FieldRule::Token),
            ("operation", FieldRule::Token),
            ("args", FieldRule::Size),
        ],
    },
    CommandPolicy {
        command: "app_session.register_native",
        fields: &[("app_id", FieldRule::Token)],
    },
    CommandPolicy { command: "mcp_session.register", fields: &[("command", FieldRule::Size)] },
    CommandPolicy {
        command: "app_session.bind",
        fields: &[("session_id", FieldRule::Token), ("pid", FieldRule::Count)],
    },
    CommandPolicy {
        command: "app_session.set_transient",
        fields: &[("session_id", FieldRule::Token), ("call", FieldRule::Size)],
    },
    CommandPolicy {
        command: "app_session.deregister",
        fields: &[("session_id", FieldRule::Token)],
    },
    // Scheduler arguments carry `--credential` names, prompts and
    // shell command lines, so only their count is recorded.
    CommandPolicy {
        command: "scheduler.run",
        fields: &[
            ("subsystem", FieldRule::Enum(&["cron", "triggers"])),
            ("command", FieldRule::Token),
            ("args", FieldRule::Size),
        ],
    },
];

pub fn command_policy(command: &str) -> Option<&'static CommandPolicy> {
    COMMAND_POLICIES
        .iter()
        .find(|policy| policy.command == command)
}

pub fn known_commands() -> impl Iterator<Item = &'static str> {
    COMMAND_POLICIES.iter().map(|policy| policy.command)
}

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
    match command_policy(command) {
        Some(policy) => {
            let (fields, omitted) = project_fields(policy.fields, params);
            RequestFacts {
                command: policy.command,
                command_text: None,
                params: fields,
                params_kind: value_kind(params),
                params_omitted: omitted,
            }
        }
        None => {
            let (fields, omitted) = project_fields(&[], params);
            RequestFacts {
                command: UNRECOGNIZED,
                command_text: Some(text_digest(command)),
                params: fields,
                params_kind: value_kind(params),
                params_omitted: omitted,
            }
        }
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
