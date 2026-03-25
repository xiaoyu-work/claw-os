use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use serde_json::json;

/// Redact sensitive patterns from args before logging.
/// Catches bearer/token prefixes, common API key prefixes, and authorization headers.
fn redact_args(args: &[String]) -> Vec<String> {
    args.iter()
        .map(|arg| {
            let lower = arg.to_lowercase();
            // Redact values that follow auth/token/key/password patterns
            if lower.starts_with("bearer ") || lower.starts_with("token ") {
                return "***REDACTED***".to_string();
            }
            // Redact common API key patterns (sk-..., ghp_..., etc.)
            if arg.starts_with("sk-")
                || arg.starts_with("ghp_")
                || arg.starts_with("ghs_")
                || arg.starts_with("glpat-")
                || arg.starts_with("xoxb-")
                || arg.starts_with("xoxp-")
            {
                return "***REDACTED***".to_string();
            }
            // Redact Authorization header values
            if lower.contains("authorization:") {
                return "Authorization: ***REDACTED***".to_string();
            }
            arg.clone()
        })
        .collect()
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

    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(audit_path)
    {
        let _ = writeln!(f, "{}", entry);
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
}
