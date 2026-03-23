use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

use chrono::Utc;
use serde_json::json;

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

    let mut entry = json!({
        "timestamp": timestamp,
        "app": app,
        "command": command,
        "args": args,
        "duration_ms": duration_ms,
        "status": status,
    });

    if let Some(e) = error {
        entry["error"] = json!(e);
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
