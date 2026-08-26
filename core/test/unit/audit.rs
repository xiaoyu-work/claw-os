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
