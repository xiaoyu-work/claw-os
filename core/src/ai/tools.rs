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
    use std::io::Read;
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing `path`".to_string())?;
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(1_048_576) as usize;

    // Stream up to `max_bytes + 1` from the file so a multi-gigabyte
    // file can't first balloon the process heap. We read one byte
    // past the cap and use that as the "truncated" signal.
    let f = std::fs::File::open(path).map_err(|e| format!("read {path}: {e}"))?;
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
        // sanitize_key now anchors the on-disk name with a 16-hex
        // SHA-256 prefix so visually-similar keys ("a/b" vs "a:b")
        // get distinct files. The human-readable suffix only
        // contains [A-Za-z0-9_-] and is informational. We check both
        // shape and uniqueness.
        let a = sanitize_key("a-b_c");
        let b = sanitize_key("a/b");
        let c = sanitize_key("../etc/passwd");
        // 16 hex chars + `.` + suffix (or just 16 hex chars when the
        // suffix is empty).
        for k in [&a, &b, &c] {
            let prefix: String = k.chars().take(16).collect();
            assert_eq!(prefix.len(), 16);
            assert!(prefix.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(
                k.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'),
                "unexpected non-alphanumeric byte in sanitized key {k:?}",
            );
        }
        // Visually-similar but semantically distinct keys must NOT
        // collide on disk.
        assert_ne!(sanitize_key("a/b"), sanitize_key("a:b"));
        assert_ne!(sanitize_key("foo bar"), sanitize_key("foo_bar"));
    }

    // ---- ai.tools[] allowlist enforcement -----------------------------
    //
    // These tests mutate $COS_APPS_DIR which is process-global, so the
    // module shares one Mutex with itself (same pattern as
    // `agent::tools::cos_apps`). We never go through the real
    // `/usr/lib/cos/apps` filesystem.

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn with_tmp_apps<F: FnOnce()>(label: &str, manifests: &[(&str, &str)], f: F) {
        let _g = env_lock();
        let dir = std::env::temp_dir().join(format!(
            "cos-tools-allow-{}-{}",
            label,
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        for (id, body) in manifests {
            let app_dir = dir.join(id);
            std::fs::create_dir_all(&app_dir).unwrap();
            std::fs::write(app_dir.join("app.json"), body).unwrap();
        }
        let prev = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", &dir);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        match prev {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = result {
            std::panic::resume_unwind(e);
        }
    }

    fn manifest_with_tools(id: &str, tools: &[&str]) -> String {
        let tools_json = serde_json::to_string(tools).unwrap();
        format!(
            r#"{{
                "id": "{id}",
                "version": "0.1.0",
                "name": {{ "en": "test" }},
                "summary": {{ "en": "test fixture" }},
                "runtime": "python",
                "entry": "main.py",
                "operations": {{}},
                "ai": {{
                    "budget": {{ "monthly_units": 0 }},
                    "origins": ["trusted"],
                    "tools": {tools_json}
                }}
            }}"#
        )
    }

    fn manifest_no_ai(id: &str) -> String {
        format!(
            r#"{{
                "id": "{id}",
                "version": "0.1.0",
                "name": {{ "en": "test" }},
                "summary": {{ "en": "test fixture" }},
                "runtime": "python",
                "entry": "main.py",
                "operations": {{}}
            }}"#
        )
    }

    #[test]
    fn execute_rejects_tool_not_in_allowlist() {
        let app = "demo-app";
        let m = manifest_with_tools(app, &["kv.get"]);
        // Self-check the fixture parses; if it doesn't, the apps
        // discovery silently drops it and the assertion below fires
        // with the cryptic "unknown app" message instead of the
        // intended allowlist error.
        crate::caps::manifest::Manifest::from_json(&m)
            .expect("test fixture manifest must parse");
        with_tmp_apps("not-in-allowlist", &[(app, &m)], || {
            // valid path arg so we get past derive_scope; allowlist
            // check must still trip.
            let err = execute("fs.read_text", app, &json!({"path": "/tmp/x"}))
                .unwrap_err();
            assert!(
                err.starts_with("tool not in ai.tools:"),
                "wrong error bucket: {err}"
            );
            assert!(err.contains("fs.read_text"), "{err}");
            assert!(err.contains(app), "{err}");
        });
    }

    #[test]
    fn execute_rejects_app_without_ai_block() {
        let app = "no-ai-app";
        let m = manifest_no_ai(app);
        with_tmp_apps("no-ai-block", &[(app, &m)], || {
            let err = execute("fs.read_text", app, &json!({"path": "/tmp/x"}))
                .unwrap_err();
            assert!(
                err.starts_with("no ai policy:"),
                "wrong error bucket: {err}"
            );
            assert!(err.contains(app), "{err}");
        });
    }

    #[test]
    fn execute_rejects_unknown_app() {
        let other = "different-app";
        let m = manifest_with_tools(other, &["kv.get"]);
        with_tmp_apps("unknown-app", &[(other, &m)], || {
            let err = execute("kv.get", "nope", &json!({"key": "x"}))
                .unwrap_err();
            assert!(err.starts_with("unknown app:"), "got: {err}");
        });
    }

    #[test]
    fn execute_allowlist_runs_after_arg_shape_check() {
        // Even when the tool IS in the allowlist, malformed args still
        // fail with a tool-impl error (not the allowlist message). This
        // preserves the order baked into execute_inner — bad args
        // short-circuit before the manifest lookup.
        let app = "demo-app";
        let m = manifest_with_tools(app, &["fs.read_text"]);
        with_tmp_apps("args-shape-first", &[(app, &m)], || {
            let err = execute("fs.read_text", app, &json!({})).unwrap_err();
            assert!(err.contains("path"), "expected arg error, got: {err}");
            assert!(
                !err.starts_with("tool not in ai.tools:"),
                "allowlist must not fire on bad args: {err}"
            );
        });
    }
}
