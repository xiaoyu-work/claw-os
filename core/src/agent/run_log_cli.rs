//! `cos agent run-log` — query the per-AI-call run log.
//!
//! [`crate::agent::llm::run_log`] writes one JSONL record per
//! `provider.chat(...)` invocation (and per gate denial) to
//! `<log_dir>/ai.jsonl`. That's a finer-grained stream than the
//! turn-level audit log queried by [`super::audit_cli`] (one
//! `cos agent ask` invocation produces N turns, and each turn is
//! exactly one AI call). For diagnosing bad answers and reproducing
//! them on a specific engine version, the per-call stream is what
//! you want.
//!
//! Subcommands (all output JSON to stdout):
//!
//! - `tail [--lines N] [--session SID] [--provider P] [--model M] [--status ok|error]`
//!   — print the most recent N records (default 50).
//! - `summary [--session SID]` — total records, by-provider counts,
//!   by-model counts, by-status counts, distinct session count, and
//!   first/last timestamp.
//! - `errors [--session SID] [--limit N]` — list error records only,
//!   most recent first (default limit 20).
//! - `engines` — group records by `(engine_name, engine_version)`
//!   so the operator can see which local engine versions have
//!   actually run inference.
//! - `clear [--force]` — remove the run log. Refuses without
//!   `--force`.
//! - `path` — print the resolved run log path.
//!
//! All subcommands accept `--path <FILE>` to override the default
//! (mainly for tests).
//!
//! Read semantics mirror [`super::audit_cli`]: append-only JSONL,
//! malformed lines silently skipped, missing file returns empty
//! results (never an error).

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Top-level dispatcher for `cos agent run-log <subcmd>`.
pub fn run_log_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("summary");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "tail" => cmd_tail(rest),
        "summary" => cmd_summary(rest),
        "errors" => cmd_errors(rest),
        "engines" => cmd_engines(rest),
        "clear" => cmd_clear(rest),
        "path" => Ok(json!({
            "path": resolve_path(rest)?.display().to_string(),
        })),
        other => Err(format!(
            "unknown run-log subcommand: {other}. try: tail | summary | errors | engines | clear | path"
        )),
    }
}

// ---------------------------------------------------------------------------
// Subcommands
// ---------------------------------------------------------------------------

fn cmd_tail(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let lines = parse_usize_opt(args, "--lines")?.unwrap_or(50);
    let session = parse_string_opt(args, "--session");
    let provider = parse_string_opt(args, "--provider");
    let model = parse_string_opt(args, "--model");
    let status = parse_string_opt(args, "--status");
    let events = read_events(&path)?;
    let filtered: Vec<&Value> = events
        .iter()
        .filter(|e| match_session(e, session.as_deref()))
        .filter(|e| match_field(e, "provider", provider.as_deref()))
        .filter(|e| match_field(e, "model", model.as_deref()))
        .filter(|e| match_field(e, "status", status.as_deref()))
        .collect();
    let total = filtered.len();
    let returned: Vec<Value> = filtered
        .iter()
        .rev()
        .take(lines)
        .copied()
        .cloned()
        .collect();
    // The .rev().take(N) above gives newest-first. Reverse back so the
    // output is chronological (oldest first) which is friendlier for
    // human reading and `jq` pipelines that don't care about order.
    let mut returned = returned;
    returned.reverse();
    Ok(json!({
        "path": path.display().to_string(),
        "matched": total,
        "returned": returned.len(),
        "events": returned,
    }))
}

fn cmd_summary(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let session = parse_string_opt(args, "--session");
    let events = read_events(&path)?;
    let mut by_provider: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_model: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_status: BTreeMap<String, u64> = BTreeMap::new();
    let mut sessions: BTreeMap<String, ()> = BTreeMap::new();
    let mut count: u64 = 0;
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;
    for e in events
        .iter()
        .filter(|e| match_session(e, session.as_deref()))
    {
        count += 1;
        if let Some(p) = e.get("provider").and_then(|v| v.as_str()) {
            *by_provider.entry(p.to_string()).or_insert(0) += 1;
        }
        if let Some(m) = e.get("model").and_then(|v| v.as_str()) {
            *by_model.entry(m.to_string()).or_insert(0) += 1;
        }
        if let Some(s) = e.get("status").and_then(|v| v.as_str()) {
            *by_status.entry(s.to_string()).or_insert(0) += 1;
        }
        if let Some(s) = e.get("session_id").and_then(|v| v.as_str()) {
            sessions.insert(s.to_string(), ());
        }
        if let Some(ts) = e.get("timestamp").and_then(|v| v.as_str()) {
            if first_ts.is_none() {
                first_ts = Some(ts.to_string());
            }
            last_ts = Some(ts.to_string());
        }
    }
    Ok(json!({
        "path": path.display().to_string(),
        "session_filter": session,
        "calls": count,
        "sessions": sessions.len(),
        "first_timestamp": first_ts,
        "last_timestamp": last_ts,
        "by_provider": by_provider,
        "by_model": by_model,
        "by_status": by_status,
    }))
}

fn cmd_errors(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let session = parse_string_opt(args, "--session");
    let limit = parse_usize_opt(args, "--limit")?.unwrap_or(20);
    let events = read_events(&path)?;
    let errs: Vec<&Value> = events
        .iter()
        .filter(|e| match_session(e, session.as_deref()))
        .filter(|e| e.get("status").and_then(|v| v.as_str()) == Some("error"))
        .collect();
    let total = errs.len();
    let returned: Vec<Value> = errs.iter().rev().take(limit).copied().cloned().collect();
    let mut returned = returned;
    returned.reverse();
    Ok(json!({
        "path": path.display().to_string(),
        "session_filter": session,
        "total_errors": total,
        "returned": returned.len(),
        "events": returned,
    }))
}

fn cmd_engines(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let events = read_events(&path)?;
    // Group by "<engine_name>@<engine_version>" so each (name,version)
    // pair gets its own bucket. Records with no engine_name (cloud
    // providers) bucket under "<cloud>".
    let mut by_engine: BTreeMap<String, EngineAgg> = BTreeMap::new();
    #[derive(Default)]
    struct EngineAgg {
        calls: u64,
        ok: u64,
        errors: u64,
        models: BTreeMap<String, u64>,
    }
    for e in events.iter() {
        let name = e.get("engine_name").and_then(|v| v.as_str());
        let version = e.get("engine_version").and_then(|v| v.as_str());
        let key = match (name, version) {
            (Some(n), Some(v)) => format!("{n}@{v}"),
            (Some(n), None) => n.to_string(),
            (None, _) => "<cloud>".to_string(),
        };
        let agg = by_engine.entry(key).or_default();
        agg.calls += 1;
        match e.get("status").and_then(|v| v.as_str()) {
            Some("ok") => agg.ok += 1,
            Some("error") => agg.errors += 1,
            _ => {}
        }
        if let Some(m) = e.get("model").and_then(|v| v.as_str()) {
            *agg.models.entry(m.to_string()).or_insert(0) += 1;
        }
    }
    let by_engine_json: Value = by_engine
        .into_iter()
        .map(|(k, a)| {
            (
                k,
                json!({
                    "calls": a.calls,
                    "ok": a.ok,
                    "errors": a.errors,
                    "models": a.models,
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    Ok(json!({
        "path": path.display().to_string(),
        "by_engine": by_engine_json,
    }))
}

fn cmd_clear(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let force = args.iter().any(|a| a == "--force");
    if !force {
        return Err("refusing to clear without --force. pass --force to confirm.".to_string());
    }
    if !path.exists() {
        return Ok(json!({
            "path": path.display().to_string(),
            "cleared": false,
            "reason": "file does not exist",
        }));
    }
    fs::remove_file(&path).map_err(|e| format!("failed to remove {}: {e}", path.display()))?;
    Ok(json!({
        "path": path.display().to_string(),
        "cleared": true,
    }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_path(args: &[String]) -> Result<PathBuf, String> {
    if let Some(p) = parse_string_opt(args, "--path") {
        Ok(PathBuf::from(p))
    } else {
        Ok(crate::paths::ai_run_log_path())
    }
}

fn parse_string_opt(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a == flag {
            return iter.next().cloned();
        }
    }
    None
}

fn parse_usize_opt(args: &[String], flag: &str) -> Result<Option<usize>, String> {
    if let Some(s) = parse_string_opt(args, flag) {
        let n: usize = s
            .parse()
            .map_err(|_| format!("invalid {flag}: {s} (expected non-negative integer)"))?;
        Ok(Some(n))
    } else {
        Ok(None)
    }
}

fn match_session(e: &Value, session: Option<&str>) -> bool {
    let Some(want) = session else {
        return true;
    };
    e.get("session_id").and_then(|v| v.as_str()) == Some(want)
}

fn match_field(e: &Value, field: &str, want: Option<&str>) -> bool {
    let Some(want) = want else {
        return true;
    };
    e.get(field).and_then(|v| v.as_str()) == Some(want)
}

fn read_events(path: &Path) -> Result<Vec<Value>, String> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let f = fs::File::open(path).map_err(|e| format!("failed to open {}: {e}", path.display()))?;
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Silently drop malformed lines: a half-flushed final line from
        // a crashed worker shouldn't poison the whole query.
        if let Ok(Value::Object(_)) = serde_json::from_str::<Value>(trimmed) {
            // Re-parse as Value (cheaper than cloning the Map).
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                out.push(v);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/run_log_cli.rs"
    ));
}
