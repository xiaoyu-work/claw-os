use std::fs;
use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use serde_json::json;

/// Redact sensitive patterns from args before logging.
/// Catches bearer/token prefixes, common API key prefixes, URL
/// userinfo segments, and authorization headers (both as their own
/// arg and as a substring of a larger arg like `--header
/// "Authorization: Bearer ..."` or `-H Authorization: ...`).
fn redact_args(args: &[String]) -> Vec<String> {
    args.iter().map(|arg| redact_one(arg)).collect()
}

fn redact_one(arg: &str) -> String {
    let lower = arg.to_lowercase();
    // Whole-arg auth tokens
    if lower.starts_with("bearer ") || lower.starts_with("token ") {
        return "***REDACTED***".to_string();
    }
    // Whole-arg API key shapes
    if arg.starts_with("sk-")
        || arg.starts_with("ghp_")
        || arg.starts_with("ghs_")
        || arg.starts_with("gho_")
        || arg.starts_with("ghu_")
        || arg.starts_with("ghr_")
        || arg.starts_with("glpat-")
        || arg.starts_with("xoxb-")
        || arg.starts_with("xoxp-")
        || arg.starts_with("xoxa-")
        || arg.starts_with("xoxs-")
        || arg.starts_with("AKIA")
    {
        return "***REDACTED***".to_string();
    }
    // Authorization header — may be a whole arg ("Authorization: Bearer X")
    // or embedded inside an arg ('--header Authorization: ...').
    if let Some(idx) = lower.find("authorization:") {
        let prefix = &arg[..idx];
        return format!("{prefix}Authorization: ***REDACTED***");
    }
    // URLs with embedded credentials (https://user:pass@host).
    // Replace `user:pass@` with `***REDACTED***@` while keeping the
    // host/path visible for triage.
    if let Some(redacted) = redact_url_creds(arg) {
        return redacted;
    }
    arg.to_string()
}

/// If `arg` contains a `://user:pass@` userinfo segment, return a
/// redacted copy. Returns None when there's nothing credential-like
/// to redact.
fn redact_url_creds(arg: &str) -> Option<String> {
    let scheme_end = arg.find("://")?;
    let after_scheme = &arg[scheme_end + 3..];
    // Userinfo ends at the next '@' that comes before the path / query.
    let at_idx = after_scheme.find('@')?;
    let userinfo = &after_scheme[..at_idx];
    if !userinfo.contains(':') {
        // username-only (e.g. github.com/user@email/repo paths) — be
        // conservative and only redact when it looks like user:pass.
        return None;
    }
    // Stop scanning at the first path/query/fragment delimiter to
    // avoid pulling in '@' from inside a path.
    let stop = userinfo
        .find(|c: char| c == '/' || c == '?' || c == '#' || c == ' ')
        .unwrap_or(usize::MAX);
    if stop != usize::MAX {
        return None;
    }
    let mut out = String::with_capacity(arg.len());
    out.push_str(&arg[..scheme_end + 3]);
    out.push_str("***REDACTED***@");
    out.push_str(&after_scheme[at_idx + 1..]);
    Some(out)
}

/// Write an audit log entry to the JSONL file.
pub fn log_entry(
    audit_path: &Path,
    app: &str,
    command: &str,
    args: &[String],
    start: Instant,
    status: &str,
    error: Option<&str>,
) {
    let duration_ms = start.elapsed().as_millis() as u64;
    let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let safe_args = redact_args(args);

    let mut entry = json!({
        "timestamp": timestamp,
        "app": app,
        "command": command,
        "args": safe_args,
        "duration_ms": duration_ms,
        "status": status,
    });

    if let Some(e) = error {
        entry["error"] = json!(e);
    }

    // Attach trace context if available
    if let Ok(trace_id) = std::env::var("COS_TRACE_ID") {
        if !trace_id.is_empty() {
            entry["trace_id"] = json!(trace_id);
        }
    }
    if let Ok(span_id) = std::env::var("COS_SPAN_ID") {
        if !span_id.is_empty() {
            entry["span_id"] = json!(span_id);
        }
    }

    if let Some(parent) = audit_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = crate::filelock::append_locked(audit_path, &entry.to_string());
}

/// Append a structured JSONL audit event with arbitrary shape.
///
/// Used by callers that need a richer schema than the
/// `app/command/args/duration_ms/status` shape produced by
/// [`log_entry`] — for example, the agent runtime's `AuditHook`
/// emits `{ kind, session_id, turn, tool_name, latency_ms,
/// bytes_returned, error }`.
///
/// Behaviour:
///   - `timestamp` is auto-injected (UTC, `YYYY-MM-DDTHH:MM:SSZ`)
///     if the entry doesn't already have one.
///   - `trace_id` / `span_id` are injected from `COS_TRACE_ID` /
///     `COS_SPAN_ID` env vars when set and not already present.
///   - Parent directory of `audit_path` is created if missing.
///   - Write is appended atomically via the file-lock helper.
///
/// Failures (IO, lock contention) are silently swallowed — audit
/// is best-effort and must never block the calling agent loop.
/// Callers that need stronger guarantees should not use this API.
pub fn log_event(audit_path: &Path, mut entry: serde_json::Value) {
    if let Some(obj) = entry.as_object_mut() {
        obj.entry("timestamp")
            .or_insert_with(|| json!(Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()));
        if !obj.contains_key("trace_id") {
            if let Ok(tid) = std::env::var("COS_TRACE_ID") {
                if !tid.is_empty() {
                    obj.insert("trace_id".to_string(), json!(tid));
                }
            }
        }
        if !obj.contains_key("span_id") {
            if let Ok(sid) = std::env::var("COS_SPAN_ID") {
                if !sid.is_empty() {
                    obj.insert("span_id".to_string(), json!(sid));
                }
            }
        }
    }

    if let Some(parent) = audit_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let _ = crate::filelock::append_locked(audit_path, &entry.to_string());
}

// ---------------------------------------------------------------------------
// Capability-decision audit
// ---------------------------------------------------------------------------

/// Append a capability-decision record to `${log_dir}/caps.jsonl`.
///
/// Called by [`crate::caps::require`] on every check — both allows
/// and denials. The shape is intentionally stable so log consumers
/// (Agent permission history, permission-centre UI, downstream SIEMs)
/// can rely on the field names:
///
/// ```text
/// {
///   "ts":              "2026-05-13T20:58:00Z",  // UTC, ISO-8601
///   "session_id":      "s-1234",                // COS_SESSION
///   "pid":             4711,                    // caller pid
///   "agent":           "summarize",             // COS_AGENT_LABEL
///                                               //   or COS_APP_ID
///   "verb":            "ai.chat.untrusted",
///   "scope": {                                  // structured scope
///     "kind":  "name",
///     "value": "claude-*"
///   },
///   "target_resource": "claude-*",              // flattened scope
///   "decision":        "allow",                 // allow | deny
///   "reason":          null,                    // DenialReason kind
///   "hint":            null,                    // optional hint
///   "mode":            "strict"                 // strict | permissive
/// }
/// ```
///
/// Behaviour:
///   - Best-effort: IO failures are swallowed; enforcement never
///     blocks on the writer.
///   - Skips writing when `COS_CAPS_AUDIT=0` (used by the busy unit
///     tests so they don't spam the user's logs dir).
///   - `timestamp`, `trace_id`, and `span_id` come from
///     [`log_event`].
pub fn log_cap_decision(entry: serde_json::Value) {
    if std::env::var("COS_CAPS_AUDIT").as_deref() == Ok("0") {
        return;
    }
    crate::clawd::system_journal::record_cap_decision(&entry);
    let path = crate::paths::caps_audit_log_path();
    log_event(&path, entry);
}

#[cfg(test)]
mod cap_audit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn log_cap_decision_writes_under_log_dir() {
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("COS_LOG_DIR");
        std::env::set_var("COS_LOG_DIR", dir.path());
        std::env::remove_var("COS_CAPS_AUDIT");
        log_cap_decision(json!({
            "session_id": "s-1",
            "verb": "fs.read",
            "decision": "allow",
        }));
        let p = crate::paths::caps_audit_log_path();
        assert!(p.is_file(), "expected {} to be a file", p.display());
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(v["session_id"], json!("s-1"));
        assert_eq!(v["verb"], json!("fs.read"));
        assert!(v["timestamp"].is_string());
        match prev {
            Some(v) => std::env::set_var("COS_LOG_DIR", v),
            None => std::env::remove_var("COS_LOG_DIR"),
        }
    }

    #[test]
    fn log_cap_decision_skipped_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("COS_LOG_DIR");
        std::env::set_var("COS_LOG_DIR", dir.path());
        std::env::set_var("COS_CAPS_AUDIT", "0");
        log_cap_decision(json!({
            "session_id": "s-1",
            "verb": "fs.read",
            "decision": "deny",
        }));
        let p = crate::paths::caps_audit_log_path();
        assert!(!p.exists(), "expected no caps.jsonl to be written");
        std::env::remove_var("COS_CAPS_AUDIT");
        match prev {
            Some(v) => std::env::set_var("COS_LOG_DIR", v),
            None => std::env::remove_var("COS_LOG_DIR"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_bearer_token() {
        let args = vec!["Bearer eyJhbGciOi...".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_bearer_case_insensitive() {
        let args = vec!["BEARER my-secret-token".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_token_prefix() {
        let args = vec!["token abc123".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_openai_key() {
        let args = vec!["sk-abc123def456".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_github_pat() {
        let args = vec!["ghp_xxxxxxxxxxxxxxxxxxxx".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_github_server_token() {
        let args = vec!["ghs_xxxxxxxxxxxxxxxxxxxx".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_gitlab_token() {
        let args = vec!["glpat-xxxxxxxxxxxxxxxxxxxx".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_slack_bot_token() {
        let args = vec!["xoxb-123-456-abc".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_slack_user_token() {
        let args = vec!["xoxp-123-456-abc".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["***REDACTED***"]);
    }

    #[test]
    fn redact_authorization_header() {
        let args = vec!["Authorization: Bearer secret".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["Authorization: ***REDACTED***"]);
    }

    #[test]
    fn redact_authorization_header_case_insensitive() {
        let args = vec!["authorization:basic dXNlcjpwYXNz".to_string()];
        let result = redact_args(&args);
        assert_eq!(result, vec!["Authorization: ***REDACTED***"]);
    }

    #[test]
    fn safe_args_pass_through() {
        let args = vec![
            "--output".to_string(),
            "json".to_string(),
            "/path/to/file".to_string(),
        ];
        let result = redact_args(&args);
        assert_eq!(result, args);
    }

    #[test]
    fn mixed_safe_and_sensitive_args() {
        let args = vec![
            "--header".to_string(),
            "Authorization: Bearer secret".to_string(),
            "--url".to_string(),
            "https://api.example.com".to_string(),
        ];
        let result = redact_args(&args);
        assert_eq!(result[0], "--header");
        assert_eq!(result[1], "Authorization: ***REDACTED***");
        assert_eq!(result[2], "--url");
        assert_eq!(result[3], "https://api.example.com");
    }

    #[test]
    fn empty_args() {
        let args: Vec<String> = vec![];
        let result = redact_args(&args);
        assert!(result.is_empty());
    }

    // ---- log_event ----

    #[test]
    fn log_event_appends_jsonl_with_auto_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        log_event(&p, json!({ "kind": "smoke", "n": 1 }));
        log_event(&p, json!({ "kind": "smoke", "n": 2 }));
        let body = std::fs::read_to_string(&p).unwrap();
        let mut lines = body.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        let v: serde_json::Value = serde_json::from_str(lines.remove(0)).unwrap();
        assert_eq!(v["kind"], json!("smoke"));
        assert_eq!(v["n"], json!(1));
        assert!(v["timestamp"].is_string(), "auto-timestamp should be added");
    }

    #[test]
    fn log_event_preserves_caller_supplied_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        log_event(
            &p,
            json!({ "kind": "x", "timestamp": "2099-01-01T00:00:00Z" }),
        );
        let body = std::fs::read_to_string(&p).unwrap();
        let v: serde_json::Value = serde_json::from_str(body.trim()).unwrap();
        assert_eq!(v["timestamp"], json!("2099-01-01T00:00:00Z"));
    }

    #[test]
    fn log_event_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested").join("a").join("audit.jsonl");
        log_event(&p, json!({ "kind": "x" }));
        assert!(p.exists());
    }

    #[test]
    fn log_event_swallows_non_object_entries_via_no_inject() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.jsonl");
        // Top-level array is legal JSON; we just don't inject
        // timestamp/trace_id into it. Should still be appended.
        log_event(&p, json!(["raw", "values"]));
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body.trim(), "[\"raw\",\"values\"]");
    }
}
