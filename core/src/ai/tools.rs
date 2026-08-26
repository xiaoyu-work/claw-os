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
//!
//! Allowlist enforcement
//! ---------------------
//!
//! Every call into [`execute`] performs three gates **in order**:
//!   1. Catalog lookup — unknown names emit `unknown_tool`.
//!   2. Manifest allowlist — the App's `ai.tools[]` must include
//!      the name, else `tool_not_in_policy` (or `no_ai_policy` if
//!      the manifest has no `ai` block at all).
//!   3. Capability check — `caps::require(verb, scope)` must pass,
//!      else `caps_denied`.
//!
//! The catalog check comes first so a typo always reports as
//! "unknown tool" rather than "you didn't declare a tool that
//! doesn't exist".

use serde::Serialize;
use serde_json::{json, Value};

use crate::apps;
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
/// Identity enforcement is the caller's responsibility — `cos ai tool`
/// validates the env claim, registered App session, and process ancestry
/// before reaching this function, sharing the gate with `cos ai chat`.
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
            // Bucket the error. Order matters because some paths emit
            // structured prefixes the rest of the system also greps on:
            //
            //   * `denied: ...`                 — caps::require failed
            //   * `tool not in ai.tools: ...`   — App's manifest didn't
            //                                     declare this Tool
            //   * `no ai policy: ...`           — manifest has no `ai`
            //                                     block at all
            //   * unknown-tool path              — catalog lookup missed
            //   * everything else                — tool-impl failure
            let (decision, denial_reason) = if msg.starts_with("denied:") {
                ("denied", Some("caps_denied"))
            } else if msg.starts_with("tool not in ai.tools:") {
                ("denied", Some("tool_not_in_policy"))
            } else if msg.starts_with("no ai policy:") {
                ("denied", Some("no_ai_policy"))
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

/// Resolve `app_id` to its installed manifest and require `tool_name`
/// to appear in the manifest's `ai.tools[]` allowlist. The kernel
/// uses `COS_APPS_DIR` (default `/usr/lib/cos/apps`) to discover
/// installed Apps — same convention `cos ai chat` uses.
///
/// Error message prefixes are stable: callers of [`execute`] grep on
/// them to attribute the right `denial_reason` to the audit log, so
/// **do not** rename them without updating the bucket logic above.
fn require_tool_in_app_policy(app_id: &str, tool_name: &str) -> Result<(), String> {
    let apps_dir = std::env::var("COS_APPS_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/usr/lib/cos/apps"));
    let discovered = apps::discover(&apps_dir);
    let app = discovered
        .get(app_id)
        .ok_or_else(|| format!("unknown app: {app_id}"))?;
    let Some(policy) = app.manifest.ai.as_ref() else {
        return Err(format!(
            "no ai policy: app `{app_id}` has no `ai` block in its manifest — \
             it cannot use `cos ai tool`. Add an `ai.tools[]` allowlist and re-install."
        ));
    };
    if !policy.tools.iter().any(|t| t == tool_name) {
        return Err(format!(
            "tool not in ai.tools: `{tool_name}` is not in app `{app_id}`'s \
             manifest `ai.tools[]` allowlist (declared: {:?}). Add it to the \
             manifest and re-install.",
            policy.tools
        ));
    }
    Ok(())
}

fn execute_inner(
    tool_name: &str,
    app_id: &str,
    args: &Value,
) -> Result<ToolResult, String> {
    let tool = lookup(tool_name)
        .ok_or_else(|| format!("unknown tool: {tool_name}. try one of: {:?}", list_names()))?;

    // Argument shape is checked before the App-policy lookup so bad
    // calls report as such instead of leaking "your manifest doesn't
    // declare this tool" for a request that was malformed anyway.
    let scope = derive_scope(tool, args)?;

    require_tool_in_app_policy(app_id, tool.name)?;

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
            Ok(Scope::path(
                resolve_fs_path(p)?.to_string_lossy().into_owned(),
            ))
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
    use std::io::Read;
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `path`".to_string())?;
    let resolved_path = resolve_fs_path(path)?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(1_048_576) as usize;

    // Stream up to `max_bytes + 1` from the file so a multi-gigabyte
    // file can't first balloon the process heap. We read one byte
    // past the cap and use that as the "truncated" signal.
    let f = std::fs::File::open(&resolved_path)
        .map_err(|e| format!("read {}: {e}", resolved_path.display()))?;
    let take_cap = max_bytes.saturating_add(1);
    let mut body = Vec::with_capacity(max_bytes.min(64 * 1024));
    f.take(take_cap as u64)
        .read_to_end(&mut body)
        .map_err(|e| format!("read {path}: {e}"))?;
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
    let resolved_path = resolve_fs_path(path)?;
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .unwrap_or(256) as usize;

    let rd = std::fs::read_dir(&resolved_path)
        .map_err(|e| format!("read_dir {}: {e}", resolved_path.display()))?;
    let mut entries = Vec::new();
    let mut truncated = false;
    for (i, entry) in rd.enumerate() {
        if i >= max_entries {
            truncated = true;
            break;
        }
        let entry = entry.map_err(|e| e.to_string())?;
        let file_type = entry.file_type().ok();
        let kind = match file_type {
            Some(ft) if ft.is_file() => "file",
            Some(ft) if ft.is_dir() => "dir",
            Some(ft) if ft.is_symlink() => "symlink",
            _ => "other",
        };
        let mut value = json!({
            "name": entry.file_name().to_string_lossy(),
            "kind": kind,
        });
        if kind == "file" {
            if let Ok(metadata) = entry.metadata() {
                value["size"] = json!(metadata.len());
            }
        }
        entries.push(value);
    }

    Ok(json!({
        "path": path,
        "entries": entries,
        "truncated": truncated,
    }))
}

fn resolve_fs_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let Some(rest) = raw.strip_prefix("~/") else {
        if raw == "~" {
            return effective_tool_home();
        }
        return Ok(raw.into());
    };
    Ok(effective_tool_home()?.join(rest))
}

fn effective_tool_home() -> Result<std::path::PathBuf, String> {
    crate::paths::current_home_override()
        .or_else(|| std::env::var_os("HOME").map(Into::into))
        .ok_or_else(|| "cannot resolve `~`: HOME is not set".to_string())
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

/// Build a filesystem-safe stable identifier from an arbitrary
/// caller-supplied key. The previous version mapped every
/// non-`[A-Za-z0-9_-]` byte to `_`, which silently collided values
/// such as `"foo/bar"` and `"foo:bar"` and `"foo bar"` to the same
/// on-disk file — a stored secret under one key would be returned
/// for another. We now anchor the filename with a 16-hex prefix of
/// SHA-256(key) and append a human-readable suffix for debuggability.
/// The hash makes collisions cryptographically improbable; the suffix
/// is purely cosmetic and never compared.
fn sanitize_key(k: &str) -> String {
    use crate::crypto::Sha256Stream;
    let mut h = Sha256Stream::new();
    h.update(k.as_bytes());
    let digest = h.finalize_hex();
    let prefix: String = digest.chars().take(16).collect();
    let suffix: String = k
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(32)
        .collect();
    if suffix.is_empty() {
        prefix
    } else {
        format!("{prefix}.{suffix}")
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ai/tools.rs"
    ));
}
