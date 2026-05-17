use std::fs::{self, OpenOptions};
use std::io::Write;
use std::time::Duration;

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;

use super::protocol::Response;

#[derive(Debug, Serialize)]
struct RequestAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    command: &'a str,
    ok: bool,
    duration_ms: u128,
    params: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<&'a str>,
}

#[derive(Debug, Serialize)]
struct InvalidRequestAudit<'a> {
    ts: chrono::DateTime<Utc>,
    event: &'static str,
    ok: bool,
    duration_ms: u128,
    raw: &'a str,
    error_code: &'a str,
    error_message: &'a str,
}

pub fn record_request(
    command: &str,
    params: &Value,
    response: &Response,
    duration: Duration,
) -> Result<(), String> {
    let audit = RequestAudit {
        ts: Utc::now(),
        event: "clawd.request",
        command,
        ok: response.ok,
        duration_ms: duration.as_millis(),
        params,
        error_code: response.error.as_ref().map(|err| err.code.as_str()),
        error_message: response.error.as_ref().map(|err| err.message.as_str()),
    };
    append_jsonl(&audit)
}

pub fn record_invalid(raw: &str, response: &Response, duration: Duration) -> Result<(), String> {
    let (error_code, error_message) = response
        .error
        .as_ref()
        .map(|err| (err.code.as_str(), err.message.as_str()))
        .unwrap_or(("invalid_json", "invalid JSON request"));
    let audit = InvalidRequestAudit {
        ts: Utc::now(),
        event: "clawd.invalid-request",
        ok: response.ok,
        duration_ms: duration.as_millis(),
        raw,
        error_code,
        error_message,
    };
    append_jsonl(&audit)
}

fn append_jsonl<T: Serialize>(record: &T) -> Result<(), String> {
    let path = crate::paths::data_dir().join("clawd").join("audit.jsonl");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            format!(
                "failed to create clawd audit dir {}: {err}",
                parent.display()
            )
        })?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|err| format!("failed to open clawd audit log {}: {err}", path.display()))?;
    let line = serde_json::to_string(record).map_err(|err| err.to_string())?;
    writeln!(file, "{line}")
        .map_err(|err| format!("failed to write clawd audit log {}: {err}", path.display()))
}
