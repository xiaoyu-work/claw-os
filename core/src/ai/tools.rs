//! `cos ai tool` — App-facing Tool catalog and executor.
//!
//! A **Tool** is a single, narrowly-scoped, capability-guarded action
//! that the kernel exposes to App-level AI workflows. Tools are the
//! *primitives* an App's own LLM is allowed to call — they let the OS
//! audit every effect the AI has on the user's machine.
//!
//! Separation of concerns
//! ----------------------
//!
//! - Regular Apps use the kernel's capability system directly (their
//!   own UI shells out to `cos fs ls`, opens files, etc).
//! - When an App routes *AI-driven* actions to the system, every such
//!   action goes through one of these Tools. The App declares which
//!   Tools it wants in its manifest (`ai.tools = ["fs.read_text", ...]`);
//!   the user grants them at install time via `cos app consent grant`.
//!
//! Why a Tool registry rather than re-using the verb registry directly?
//! -------------------------------------------------------------------
//!
//! - Tools are *higher-level* than verbs. `fs.read_text` is a Tool;
//!   it requires `fs.read` (a verb). A Tool also pins a stable JSON
//!   argument schema (for prompt-time function-calling) and a stable
//!   JSON return shape. Verbs do not.
//! - Tools have a **stability tier** (`stable` / `experimental`) so
//!   we can evolve them without breaking installed Apps.
//! - The Tool catalog is the only AI-relevant surface area the kernel
//!   commits to. Verbs may proliferate; Tools are curated.
//!
//! Audit
//! -----
//!
//! Every successful or denied Tool call appends one row to
//! `<log_dir>/ai.jsonl` with `verb = "ai.tool.<name>"`, sharing the
//! same `LlmRunRecord` shape that `cos ai chat` already uses. The
//! kernel-side identity check (`enforce_identity_for`, see
//! `core/src/ai/chat.rs`) is shared so impersonation is impossible.

use serde::Serialize;
use serde_json::{json, Value};

use crate::caps::{require, Scope, Verb};

/// One entry in the Tool catalog.
///
/// `name` is the stable identifier Apps put in their manifest and in
/// LLM function-calling specs. `verb` + `derive_scope` describe the
/// capability check the kernel performs before running the Tool.
#[derive(Debug, Clone, Serialize)]
pub struct ToolDef {
    pub name: &'static str,
    pub summary: &'static str,
    pub verb: Verb,
    /// JSON Schema (draft-07 subset) of arguments. Used for both
    /// manifest validation and LLM function-call specs.
    pub args_schema: &'static str,
    /// JSON Schema of the return envelope. Stable across versions
    /// within a `stability = "stable"` tier.
    pub returns_schema: &'static str,
    pub stability: Stability,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Stability {
    Stable,
    Experimental,
}

/// The single source of truth for what Tools exist.
///
/// Adding a Tool here costs:
///   1. An entry in this slice.
///   2. A dispatch arm in [`execute`] and [`derive_scope`].
///   3. A row in `docs/app-ai-tool-catalog.md`.
pub const CATALOG: &[ToolDef] = &[
    ToolDef {
        name: "fs.read_text",
        summary: "Read a UTF-8 text file at `path`. Returns the file body. \
                  Path must lie within the App's granted fs.read scope.",
        verb: Verb::FS_READ,
        args_schema: r#"{
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string", "description": "Absolute path or ~-relative."},
                "max_bytes": {"type": "integer", "minimum": 1, "default": 1048576}
            },
            "additionalProperties": false
        }"#,
        returns_schema: r#"{
            "type": "object",
            "required": ["path", "bytes_read", "content"],
            "properties": {
                "path": {"type": "string"},
                "bytes_read": {"type": "integer"},
                "content": {"type": "string"},
                "truncated": {"type": "boolean"}
            }
        }"#,
        stability: Stability::Stable,
    },
    ToolDef {
        name: "fs.list",
        summary: "List entries in `path` (one directory level). Returns name + kind + size. \
                  Requires fs.meta on the directory.",
        verb: Verb::FS_META,
        args_schema: r#"{
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "max_entries": {"type": "integer", "minimum": 1, "default": 256}
            },
            "additionalProperties": false
        }"#,
        returns_schema: r#"{
            "type": "object",
            "required": ["path", "entries"],
            "properties": {
                "path": {"type": "string"},
                "entries": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["name", "kind"],
                        "properties": {
                            "name": {"type": "string"},
                            "kind": {"type": "string", "enum": ["file", "dir", "symlink", "other"]},
                            "size": {"type": "integer"}
                        }
                    }
                },
                "truncated": {"type": "boolean"}
            }
        }"#,
        stability: Stability::Stable,
    },
    ToolDef {
        name: "kv.get",
        summary: "Read a value from the App's per-App key-value store. Returns null if missing.",
        verb: Verb::DATA_KV_READ,
        args_schema: r#"{
            "type": "object",
            "required": ["key"],
            "properties": {
                "key": {"type": "string", "minLength": 1}
            },
            "additionalProperties": false
        }"#,
        returns_schema: r#"{
            "type": "object",
            "required": ["key", "value"],
            "properties": {
                "key": {"type": "string"},
                "value": {"type": ["string", "null"]}
            }
        }"#,
        stability: Stability::Stable,
    },
];

/// Look up a Tool by stable name. Returns `None` for unknown tools.
pub fn lookup(name: &str) -> Option<&'static ToolDef> {
    CATALOG.iter().find(|t| t.name == name)
}

/// List Tool names (for catalogs, LLM tool-spec emission, etc).
pub fn list_names() -> Vec<&'static str> {
    CATALOG.iter().map(|t| t.name).collect()
}

/// Tool execution result, written to the audit log and returned to
/// the caller as the JSON response body.
#[derive(Debug, Clone, Serialize)]
pub struct ToolResult {
    pub tool: String,
    pub app_id: String,
    pub status: String,
    pub result: Value,
}

/// Execute one Tool call. Performs:
///   1. Catalog lookup (`unknown_tool` if missing).
///   2. Capability check via `caps::require(verb, scope-derived-from-args)`.
///   3. Tool-specific impl.
///
/// Identity enforcement (matching `--app` against `COS_APP_ID`) is the
/// caller's responsibility — `cos ai tool`'s dispatcher does it before
/// reaching this function so it is shared with `cos ai chat`.
pub fn execute(
    tool_name: &str,
    app_id: &str,
    args: &Value,
) -> Result<ToolResult, String> {
    let started = std::time::Instant::now();
    let outcome = execute_inner(tool_name, app_id, args);
    let duration_ms = started.elapsed().as_millis() as u64;

    // Build the audit row. Unknown-tool (no catalog hit) records as
    // verb="" — we have no verb to attribute. All other paths know
    // the verb because the catalog gave it to us.
    let verb_str = lookup(tool_name)
        .map(|t| t.verb.as_str().to_string())
        .unwrap_or_default();
    let session_id = std::env::var("COS_SESSION_ID").ok();
    let session_ref = session_id.as_deref();

    match &outcome {
        Ok(_) => {
            let rec = crate::agent::llm::run_log::LlmRunRecord::from_tool_call(
                tool_name,
                app_id,
                &verb_str,
                "allowed",
                None,
                None,
                duration_ms,
                session_ref,
            );
            crate::agent::llm::run_log::record(&rec);
        }
        Err(msg) => {
            // Bucket the error: caps denials carry the literal "denied:"
            // prefix this module added; everything else is a tool-impl
            // failure (bad args, fs error, ...). Both are auditable.
            let (decision, denial_reason) = if msg.starts_with("denied:") {
                ("denied", Some("caps_denied"))
            } else if tool_name_unknown(tool_name) {
                ("denied", Some("unknown_tool"))
            } else {
                ("error", Some("tool_impl_error"))
            };
            let rec = crate::agent::llm::run_log::LlmRunRecord::from_tool_call(
                tool_name,
                app_id,
                &verb_str,
                decision,
                denial_reason,
                Some(msg),
                duration_ms,
                session_ref,
            );
            crate::agent::llm::run_log::record(&rec);
        }
    }
    outcome
}

fn tool_name_unknown(name: &str) -> bool {
    lookup(name).is_none()
}

fn execute_inner(
    tool_name: &str,
    app_id: &str,
    args: &Value,
) -> Result<ToolResult, String> {
    let tool = lookup(tool_name)
        .ok_or_else(|| format!("unknown tool: {tool_name}. try one of: {:?}", list_names()))?;

    let scope = derive_scope(tool, args)?;
    require(tool.verb, scope).map_err(|d| format!("denied: {}", d.to_json()))?;

    let result = match tool.name {
        "fs.read_text" => impl_fs_read_text(args)?,
        "fs.list" => impl_fs_list(args)?,
        "kv.get" => impl_kv_get(app_id, args)?,
        other => return Err(format!("tool {other} has no impl wired up")),
    };

    Ok(ToolResult {
        tool: tool.name.to_string(),
        app_id: app_id.to_string(),
        status: "ok".to_string(),
        result,
    })
}

/// Derive the [`Scope`] for a tool call from its arguments. Each Tool
/// in the catalog has a documented binding from arg field → scope.
fn derive_scope(tool: &ToolDef, args: &Value) -> Result<Scope, String> {
    match tool.name {
        "fs.read_text" | "fs.list" => {
            let p = args
                .get("path")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing required arg `path`".to_string())?;
            Ok(Scope::path(p))
        }
        "kv.get" => {
            let k = args
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing required arg `key`".to_string())?;
            Ok(Scope::name(k))
        }
        other => Err(format!("no scope-derivation rule for tool {other}")),
    }
}

fn impl_fs_read_text(args: &Value) -> Result<Value, String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `path`".to_string())?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(1_048_576) as usize;

    let body = std::fs::read(path).map_err(|e| format!("read {path}: {e}"))?;
    let truncated = body.len() > max_bytes;
    let slice = if truncated { &body[..max_bytes] } else { &body[..] };
    let content = String::from_utf8(slice.to_vec())
        .map_err(|e| format!("file not valid utf-8: {e}"))?;

    Ok(json!({
        "path": path,
        "bytes_read": slice.len(),
        "content": content,
        "truncated": truncated,
    }))
}

fn impl_fs_list(args: &Value) -> Result<Value, String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `path`".to_string())?;
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .unwrap_or(256) as usize;

    let rd = std::fs::read_dir(path).map_err(|e| format!("read_dir {path}: {e}"))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for (i, entry) in rd.enumerate() {
        if i >= max_entries {
            truncated = true;
            break;
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().ok();
        let kind = match meta.as_ref().map(|m| m.file_type()) {
            Some(ft) if ft.is_file() => "file",
            Some(ft) if ft.is_dir() => "dir",
            Some(ft) if ft.is_symlink() => "symlink",
            _ => "other",
        };
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        entries.push(json!({
            "name": entry.file_name().to_string_lossy(),
            "kind": kind,
            "size": size,
        }));
    }

    Ok(json!({
        "path": path,
        "entries": entries,
        "truncated": truncated,
    }))
}

fn impl_kv_get(app_id: &str, args: &Value) -> Result<Value, String> {
    let key = args
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `key`".to_string())?;

    // Minimal stub backed by per-App on-disk JSON. The full KV impl
    // is tracked separately; this gets us an end-to-end loop for App
    // AI tool calls without blocking on the larger data.kv design.
    let dir = crate::paths::data_dir().join("apps").join(app_id).join("kv");
    let file = dir.join(format!("{}.json", sanitize_key(key)));
    let value: Option<String> = match std::fs::read_to_string(&file) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("kv read {key}: {e}")),
    };
    Ok(json!({ "key": key, "value": value }))
}

fn sanitize_key(k: &str) -> String {
    k.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_known_tools() {
        assert!(lookup("fs.read_text").is_some());
        assert!(lookup("fs.list").is_some());
        assert!(lookup("kv.get").is_some());
        assert!(lookup("does.not.exist").is_none());
    }

    #[test]
    fn list_names_covers_catalog() {
        let names = list_names();
        assert!(names.contains(&"fs.read_text"));
        assert_eq!(names.len(), CATALOG.len());
    }

    #[test]
    fn catalog_names_are_unique_and_sane() {
        let mut seen = std::collections::HashSet::new();
        for t in CATALOG {
            assert!(
                seen.insert(t.name),
                "duplicate tool name in catalog: {}",
                t.name
            );
            assert!(t.name.contains('.'), "tool names should be ns.name: {}", t.name);
            assert!(!t.summary.is_empty());
            assert!(!t.args_schema.is_empty());
            assert!(!t.returns_schema.is_empty());
        }
    }

    #[test]
    fn execute_rejects_unknown_tool() {
        let err = execute("nope.nope", "app", &json!({})).unwrap_err();
        assert!(err.contains("unknown tool"), "got: {err}");
    }

    #[test]
    fn execute_rejects_missing_required_arg() {
        let err = execute("fs.read_text", "app", &json!({})).unwrap_err();
        assert!(err.contains("path"), "got: {err}");
    }

    #[test]
    fn schemas_parse_as_json() {
        for t in CATALOG {
            serde_json::from_str::<Value>(t.args_schema).expect(t.name);
            serde_json::from_str::<Value>(t.returns_schema).expect(t.name);
        }
    }

    #[test]
    fn derive_scope_uses_path_for_fs_tools() {
        let tool = lookup("fs.read_text").unwrap();
        let scope = derive_scope(tool, &json!({"path": "/tmp/x"})).unwrap();
        assert!(matches!(scope, Scope::Path(p) if p == "/tmp/x"));
    }

    #[test]
    fn derive_scope_uses_name_for_kv_tools() {
        let tool = lookup("kv.get").unwrap();
        let scope = derive_scope(tool, &json!({"key": "user_pref"})).unwrap();
        assert!(matches!(scope, Scope::Name(n) if n == "user_pref"));
    }

    #[test]
    fn sanitize_key_replaces_special_chars() {
        assert_eq!(sanitize_key("a-b_c"), "a-b_c");
        assert_eq!(sanitize_key("a/b"), "a_b");
        assert_eq!(sanitize_key("../etc/passwd"), "___etc_passwd");
    }
}
