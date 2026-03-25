/// Execution tracing — tree-structured observability for agent tasks.
///
/// Traces are trees: trace → spans → operations. Every `cos` command
/// automatically records its trace/span context from environment
/// variables COS_TRACE_ID and COS_SPAN_ID.
///
/// Storage: `$COS_DATA_DIR/traces/<trace-id>.json`
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::policy::{self, OpType};

fn traces_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("traces")
}

fn trace_path(trace_id: &str) -> PathBuf {
    traces_dir().join(format!("{trace_id}.json"))
}

fn audit_path() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("logs")
        .join("audit.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TraceInfo {
    trace_id: String,
    started_at: String,
    #[serde(default)]
    ended_at: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    spans: Vec<SpanInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpanInfo {
    name: String,
    span_path: String,
    started_at: String,
    #[serde(default)]
    ended_at: Option<String>,
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "start" => cmd_start(args),
        "end" => cmd_end(args),
        "span" => cmd_span(args),
        "span-end" => cmd_span_end(args),
        "show" => cmd_show(args),
        "list" => cmd_list(args),
        _ => Err(format!("unknown trace command: {command}")),
    }
}

// ---------------------------------------------------------------------------
// cos trace start <trace-id>
// ---------------------------------------------------------------------------

fn cmd_start(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;

    if args.is_empty() {
        return Err("usage: cos trace start <trace-id>".into());
    }
    let trace_id = &args[0];

    let dir = traces_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create traces dir: {e}"))?;

    let path = trace_path(trace_id);
    if path.exists() {
        return Err(format!("trace already exists: {trace_id}"));
    }

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let trace = TraceInfo {
        trace_id: trace_id.clone(),
        started_at: now.clone(),
        ended_at: None,
        status: "active".into(),
        spans: vec![],
    };

    let data = serde_json::to_string_pretty(&trace)
        .map_err(|e| format!("failed to serialize trace: {e}"))?;
    fs::write(&path, &data).map_err(|e| format!("failed to write trace file: {e}"))?;

    Ok(json!({
        "trace_id": trace_id,
        "started_at": now,
        "status": "active",
        "env": {
            "COS_TRACE_ID": trace_id,
        },
        "hint": format!("Set COS_TRACE_ID={trace_id} in your environment to auto-attach operations"),
    }))
}

// ---------------------------------------------------------------------------
// cos trace end <trace-id> [--status completed|failed]
// ---------------------------------------------------------------------------

fn cmd_end(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;

    if args.is_empty() {
        return Err("usage: cos trace end <trace-id> [--status completed|failed]".into());
    }
    let trace_id = &args[0];

    let path = trace_path(trace_id);
    let data = fs::read_to_string(&path).map_err(|_| format!("trace not found: {trace_id}"))?;
    let mut trace: TraceInfo =
        serde_json::from_str(&data).map_err(|e| format!("corrupt trace file: {e}"))?;

    // Parse --status flag
    let mut status = "completed".to_string();
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--status" {
            i += 1;
            if i < args.len() {
                let s = &args[i];
                if s != "completed" && s != "failed" {
                    return Err(format!("invalid status: {s} (use completed or failed)"));
                }
                status = s.clone();
            } else {
                return Err("--status requires a value".into());
            }
        }
        i += 1;
    }

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    trace.ended_at = Some(now.clone());
    trace.status = status.clone();

    let updated = serde_json::to_string_pretty(&trace)
        .map_err(|e| format!("failed to serialize trace: {e}"))?;
    fs::write(&path, &updated).map_err(|e| format!("failed to write trace file: {e}"))?;

    let duration_ms = compute_duration_ms(&trace.started_at, &now);

    Ok(json!({
        "trace_id": trace_id,
        "ended_at": now,
        "status": status,
        "duration_ms": duration_ms,
    }))
}

// ---------------------------------------------------------------------------
// cos trace span <span-name>
// ---------------------------------------------------------------------------

fn cmd_span(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;

    if args.is_empty() {
        return Err("usage: cos trace span <span-name>".into());
    }
    let span_name = &args[0];

    let trace_id = std::env::var("COS_TRACE_ID")
        .map_err(|_| "COS_TRACE_ID not set — start a trace first".to_string())?;
    if trace_id.is_empty() {
        return Err("COS_TRACE_ID is empty — start a trace first".into());
    }

    let path = trace_path(&trace_id);
    let data = fs::read_to_string(&path).map_err(|_| format!("trace not found: {trace_id}"))?;
    let mut trace: TraceInfo =
        serde_json::from_str(&data).map_err(|e| format!("corrupt trace file: {e}"))?;

    // Build span path: if COS_SPAN_ID is set, nest under it
    let parent_span = std::env::var("COS_SPAN_ID").unwrap_or_default();
    let span_path = if parent_span.is_empty() {
        span_name.clone()
    } else {
        format!("{parent_span}/{span_name}")
    };

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let span = SpanInfo {
        name: span_name.clone(),
        span_path: span_path.clone(),
        started_at: now.clone(),
        ended_at: None,
    };

    trace.spans.push(span);

    let updated = serde_json::to_string_pretty(&trace)
        .map_err(|e| format!("failed to serialize trace: {e}"))?;
    fs::write(&path, &updated).map_err(|e| format!("failed to write trace file: {e}"))?;

    Ok(json!({
        "trace_id": trace_id,
        "span": span_name,
        "span_path": span_path,
        "started_at": now,
        "env": {
            "COS_SPAN_ID": span_path,
        },
    }))
}

// ---------------------------------------------------------------------------
// cos trace span-end [--name <span-name>]
// ---------------------------------------------------------------------------

fn cmd_span_end(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;

    let trace_id = std::env::var("COS_TRACE_ID").map_err(|_| "COS_TRACE_ID not set".to_string())?;
    if trace_id.is_empty() {
        return Err("COS_TRACE_ID is empty".into());
    }

    // Determine which span to end: --name flag or COS_SPAN_ID
    let mut explicit_name: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--name" {
            i += 1;
            if i < args.len() {
                explicit_name = Some(args[i].clone());
            } else {
                return Err("--name requires a value".into());
            }
        }
        i += 1;
    }

    let span_path = if let Some(name) = explicit_name {
        name
    } else {
        let span_id = std::env::var("COS_SPAN_ID").unwrap_or_default();
        if span_id.is_empty() {
            return Err("no active span — set COS_SPAN_ID or use --name".into());
        }
        span_id
    };

    let path = trace_path(&trace_id);
    let data = fs::read_to_string(&path).map_err(|_| format!("trace not found: {trace_id}"))?;
    let mut trace: TraceInfo =
        serde_json::from_str(&data).map_err(|e| format!("corrupt trace file: {e}"))?;

    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Find the span by path and end it
    let mut found = false;
    for span in &mut trace.spans {
        if span.span_path == span_path && span.ended_at.is_none() {
            span.ended_at = Some(now.clone());
            found = true;
            break;
        }
    }

    if !found {
        return Err(format!("span not found or already ended: {span_path}"));
    }

    let updated = serde_json::to_string_pretty(&trace)
        .map_err(|e| format!("failed to serialize trace: {e}"))?;
    fs::write(&path, &updated).map_err(|e| format!("failed to write trace file: {e}"))?;

    // Compute parent span for env hint
    let parent_span = if let Some(pos) = span_path.rfind('/') {
        &span_path[..pos]
    } else {
        ""
    };

    // Extract the deepest name for the response
    let span_name = if let Some(pos) = span_path.rfind('/') {
        &span_path[pos + 1..]
    } else {
        &span_path
    };

    Ok(json!({
        "span": span_name,
        "ended_at": now,
        "env": {
            "COS_SPAN_ID": parent_span,
        },
    }))
}

// ---------------------------------------------------------------------------
// cos trace show <trace-id>
// ---------------------------------------------------------------------------

fn cmd_show(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    if args.is_empty() {
        return Err("usage: cos trace show <trace-id>".into());
    }
    let trace_id = &args[0];

    let path = trace_path(trace_id);
    let data = fs::read_to_string(&path).map_err(|_| format!("trace not found: {trace_id}"))?;
    let trace: TraceInfo =
        serde_json::from_str(&data).map_err(|e| format!("corrupt trace file: {e}"))?;

    // Read audit log and filter entries for this trace
    let audit = audit_path();
    let mut ops_by_span: std::collections::HashMap<String, Vec<Value>> = Default::default();
    let mut unspanned_ops: Vec<Value> = Vec::new();

    if let Ok(log_data) = fs::read_to_string(&audit) {
        for line in log_data.lines() {
            if let Ok(entry) = serde_json::from_str::<Value>(line) {
                if entry.get("trace_id").and_then(|v| v.as_str()) == Some(trace_id) {
                    let mut op = json!({
                        "app": entry.get("app").and_then(|v| v.as_str()).unwrap_or(""),
                        "command": entry.get("command").and_then(|v| v.as_str()).unwrap_or(""),
                        "status": entry.get("status").and_then(|v| v.as_str()).unwrap_or(""),
                        "duration_ms": entry.get("duration_ms").and_then(|v| v.as_u64()).unwrap_or(0),
                    });
                    if let Some(e) = entry.get("error").and_then(|v| v.as_str()) {
                        op["error"] = json!(e);
                    }

                    if let Some(span_id) = entry.get("span_id").and_then(|v| v.as_str()) {
                        if !span_id.is_empty() {
                            ops_by_span.entry(span_id.to_string()).or_default().push(op);
                        } else {
                            unspanned_ops.push(op);
                        }
                    } else {
                        unspanned_ops.push(op);
                    }
                }
            }
        }
    }

    // Build span entries
    let mut span_entries: Vec<Value> = Vec::new();
    let mut total_ops: u64 = 0;
    let mut total_errors: u64 = 0;
    let mut first_error: Option<Value> = None;

    for span in &trace.spans {
        let span_ops = ops_by_span.remove(&span.span_path).unwrap_or_default();
        let op_count = span_ops.len() as u64;
        let errors = span_ops
            .iter()
            .filter(|op| op.get("status").and_then(|v| v.as_str()) == Some("error"))
            .count() as u64;

        // Track first error
        if first_error.is_none() && errors > 0 {
            if let Some(err_op) = span_ops
                .iter()
                .find(|op| op.get("status").and_then(|v| v.as_str()) == Some("error"))
            {
                first_error = Some(json!({
                    "span": span.name,
                    "app": err_op.get("app").and_then(|v| v.as_str()).unwrap_or(""),
                    "command": err_op.get("command").and_then(|v| v.as_str()).unwrap_or(""),
                    "error": err_op.get("error").and_then(|v| v.as_str()).unwrap_or(""),
                }));
            }
        }

        let duration_ms = match &span.ended_at {
            Some(end) => compute_duration_ms(&span.started_at, end),
            None => Value::Null,
        };

        span_entries.push(json!({
            "name": span.name,
            "span_path": span.span_path,
            "duration_ms": duration_ms,
            "operations": span_ops,
            "op_count": op_count,
            "errors": errors,
        }));

        total_ops += op_count;
        total_errors += errors;
    }

    // Count unspanned ops
    let unspanned_errors = unspanned_ops
        .iter()
        .filter(|op| op.get("status").and_then(|v| v.as_str()) == Some("error"))
        .count() as u64;

    // Check unspanned for first error if none found yet
    if first_error.is_none() && unspanned_errors > 0 {
        if let Some(err_op) = unspanned_ops
            .iter()
            .find(|op| op.get("status").and_then(|v| v.as_str()) == Some("error"))
        {
            first_error = Some(json!({
                "span": null,
                "app": err_op.get("app").and_then(|v| v.as_str()).unwrap_or(""),
                "command": err_op.get("command").and_then(|v| v.as_str()).unwrap_or(""),
                "error": err_op.get("error").and_then(|v| v.as_str()).unwrap_or(""),
            }));
        }
    }

    total_ops += unspanned_ops.len() as u64;
    total_errors += unspanned_errors;

    let trace_duration = match &trace.ended_at {
        Some(end) => compute_duration_ms(&trace.started_at, end),
        None => Value::Null,
    };

    let mut result = json!({
        "trace_id": trace.trace_id,
        "started_at": trace.started_at,
        "status": trace.status,
        "spans": span_entries,
        "unspanned_ops": unspanned_ops,
        "summary": {
            "total_ops": total_ops,
            "total_errors": total_errors,
            "total_spans": trace.spans.len(),
        },
    });

    if let Some(end) = &trace.ended_at {
        result["ended_at"] = json!(end);
    }
    if !trace_duration.is_null() {
        result["duration_ms"] = trace_duration;
    }
    if let Some(fe) = first_error {
        result["summary"]["first_error"] = fe;
    }

    Ok(result)
}

// ---------------------------------------------------------------------------
// cos trace list [--status active|completed|failed] [--limit N]
// ---------------------------------------------------------------------------

fn cmd_list(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    // Parse flags
    let mut status_filter: Option<String> = None;
    let mut limit: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--status" => {
                i += 1;
                if i < args.len() {
                    status_filter = Some(args[i].clone());
                } else {
                    return Err("--status requires a value".into());
                }
            }
            "--limit" => {
                i += 1;
                if i < args.len() {
                    limit = Some(
                        args[i]
                            .parse::<usize>()
                            .map_err(|_| format!("invalid limit: {}", args[i]))?,
                    );
                } else {
                    return Err("--limit requires a value".into());
                }
            }
            _ => {}
        }
        i += 1;
    }

    let dir = traces_dir();
    let mut traces: Vec<Value> = Vec::new();

    if dir.exists() {
        let mut entries: Vec<fs::DirEntry> = fs::read_dir(&dir)
            .map_err(|e| format!("failed to read traces dir: {e}"))?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .map(|ext| ext == "json")
                    .unwrap_or(false)
            })
            .collect();

        // Sort by filename for consistent ordering
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            if let Ok(data) = fs::read_to_string(entry.path()) {
                if let Ok(trace) = serde_json::from_str::<TraceInfo>(&data) {
                    // Apply status filter
                    if let Some(ref filter) = status_filter {
                        if &trace.status != filter {
                            continue;
                        }
                    }

                    let duration_ms = match &trace.ended_at {
                        Some(end) => compute_duration_ms(&trace.started_at, end),
                        None => Value::Null,
                    };

                    let mut entry_json = json!({
                        "trace_id": trace.trace_id,
                        "status": trace.status,
                        "started_at": trace.started_at,
                        "span_count": trace.spans.len(),
                    });
                    if !duration_ms.is_null() {
                        entry_json["duration_ms"] = duration_ms;
                    }

                    traces.push(entry_json);

                    if let Some(max) = limit {
                        if traces.len() >= max {
                            break;
                        }
                    }
                }
            }
        }
    }

    let count = traces.len();
    Ok(json!({
        "traces": traces,
        "count": count,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Compute duration between two ISO-8601 timestamps, returning a JSON number
/// in milliseconds or null if parsing fails.
fn compute_duration_ms(start: &str, end: &str) -> Value {
    use chrono::NaiveDateTime;
    let fmt = "%Y-%m-%dT%H:%M:%SZ";
    let s = NaiveDateTime::parse_from_str(start, fmt);
    let e = NaiveDateTime::parse_from_str(end, fmt);
    match (s, e) {
        (Ok(s), Ok(e)) => {
            let ms = (e - s).num_milliseconds();
            json!(ms)
        }
        _ => Value::Null,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    static COUNTER: AtomicU32 = AtomicU32::new(0);
    /// Mutex to serialize tests that manipulate process-global env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn unique_trace() -> String {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("cos-trace-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("COS_DATA_DIR", &dir);
        // Clear trace env vars to prevent cross-test pollution
        std::env::remove_var("COS_TRACE_ID");
        std::env::remove_var("COS_SPAN_ID");
        format!("test-trace-{n}")
    }

    #[test]
    fn start_creates_trace_file() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        let r = cmd_start(&[id.clone()]).unwrap();
        assert_eq!(r["trace_id"], id);
        assert_eq!(r["status"], "active");
        assert!(trace_path(&id).exists());
    }

    #[test]
    fn end_updates_trace() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();
        let r = cmd_end(&[id.clone()]).unwrap();
        assert_eq!(r["status"], "completed");
        assert!(r["ended_at"].is_string());
        assert!(r["duration_ms"].is_number());
    }

    #[test]
    fn end_with_failed_status() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();
        let r = cmd_end(&[id.clone(), "--status".into(), "failed".into()]).unwrap();
        assert_eq!(r["status"], "failed");
    }

    #[test]
    fn span_adds_to_trace() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();
        std::env::set_var("COS_TRACE_ID", &id);

        let r = cmd_span(&["analyze".into()]).unwrap();
        assert_eq!(r["span"], "analyze");
        assert_eq!(r["span_path"], "analyze");

        // Verify span in trace file
        let data = fs::read_to_string(trace_path(&id)).unwrap();
        let trace: TraceInfo = serde_json::from_str(&data).unwrap();
        assert_eq!(trace.spans.len(), 1);
        assert_eq!(trace.spans[0].name, "analyze");

        std::env::remove_var("COS_TRACE_ID");
    }

    #[test]
    fn nested_span() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();
        std::env::set_var("COS_TRACE_ID", &id);
        std::env::set_var("COS_SPAN_ID", "parent");

        let r = cmd_span(&["child".into()]).unwrap();
        assert_eq!(r["span_path"], "parent/child");

        std::env::remove_var("COS_TRACE_ID");
        std::env::remove_var("COS_SPAN_ID");
    }

    #[test]
    fn span_end_closes_span() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();
        std::env::set_var("COS_TRACE_ID", &id);

        cmd_span(&["test-span".into()]).unwrap();
        std::env::set_var("COS_SPAN_ID", "test-span");

        let r = cmd_span_end(&[]).unwrap();
        assert_eq!(r["span"], "test-span");
        assert!(r["ended_at"].is_string());

        std::env::remove_var("COS_TRACE_ID");
        std::env::remove_var("COS_SPAN_ID");
    }

    #[test]
    fn show_returns_tree() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();

        // Show should work even with no spans
        let r = cmd_show(&[id.clone()]).unwrap();
        assert_eq!(r["trace_id"], id);
        assert!(r["spans"].is_array());
        assert!(r["summary"].is_object());
    }

    #[test]
    fn list_shows_traces() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();

        let r = cmd_list(&[]).unwrap();
        assert!(r["count"].as_u64().unwrap() >= 1);
        let traces = r["traces"].as_array().unwrap();
        assert!(traces.iter().any(|t| t["trace_id"] == id));
    }

    #[test]
    fn list_filter_by_status() {
        let _lock = ENV_LOCK.lock().unwrap();
        let id = unique_trace();
        cmd_start(&[id.clone()]).unwrap();
        cmd_end(&[id.clone()]).unwrap();

        let r = cmd_list(&["--status".into(), "completed".into()]).unwrap();
        let traces = r["traces"].as_array().unwrap();
        assert!(traces.iter().all(|t| t["status"] == "completed"));
    }

    #[test]
    fn start_missing_id() {
        let _lock = ENV_LOCK.lock().unwrap();
        let r = cmd_start(&[]);
        assert!(r.is_err());
    }

    #[test]
    fn show_nonexistent() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _ = unique_trace(); // set up temp dir
        let r = cmd_show(&["nonexistent-trace".into()]);
        assert!(r.is_err());
    }

    #[test]
    fn run_dispatch() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _ = unique_trace();
        let r = run("list", &[]).unwrap();
        assert!(r["traces"].is_array());

        let r = run("bogus", &[]);
        assert!(r.is_err());
    }
}
