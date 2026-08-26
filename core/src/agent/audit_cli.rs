//! `cos agent audit` — query the agent audit log.
//!
//! [`AuditHook`](super::runtime::hooks::AuditHook) writes JSONL to
//! `<log_dir>/agent.jsonl` for every turn and tool dispatch when
//! enabled (`cos agent hooks enable audit`). [`CheckpointHook`](
//! super::runtime::hooks::CheckpointHook) writes
//! `pre_tool_checkpoint` events to the same file. This module makes
//! that JSONL queryable from the CLI without a `cat | jq` round-trip.
//!
//! Subcommands (all output JSON to stdout):
//!
//! - `tail [--lines N] [--session SID] [--kind KIND]` — print the
//!   most recent N entries (default 50), optionally filtering by
//!   session and/or event kind.
//! - `summary [--session SID]` — total events, by-kind counts,
//!   distinct session count, and first/last timestamp.
//! - `cache-stats [--session SID]` — aggregate prompt-cache token
//!   counts across `post_turn` events and report a hit-rate
//!   approximation, plus a per-model token breakdown.
//! - `checkpoints [--session SID]` — list every
//!   `pre_tool_checkpoint` event so an operator can see when /
//!   which tool / what checkpoint id / success or error.
//! - `verify` — verify the hash chain and recursively validate linked
//!   archives.
//! - `clear [--force]` — archive the current log and start a new chain.
//!   Refuses without `--force` to avoid accidents.
//! - `quarantine [--force]` — explicitly acknowledge an invalid chain,
//!   preserve it in a hash-anchored archive, and establish a new chain root.
//! - `path` — print the resolved audit log path. Useful for
//!   shell scripting without depending on platform-specific
//!   defaults.
//!
//! All subcommands accept `--path <FILE>` to point at an alternative
//! audit log (used by tests; rarely useful otherwise).
//!
//! ## Read semantics
//!
//! Query commands skip malformed lines so a live tail remains usable.
//! `verify` is strict: malformed JSON, sequence gaps, hash mismatches,
//! archive changes, and broken archive links all fail integrity.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const SUMMARY_VERIFY_LIMIT_BYTES: u64 = 32 * 1024 * 1024;

/// Top-level dispatcher for `cos agent audit <subcmd>`.
pub fn audit_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("summary");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "tail" => cmd_tail(rest),
        "summary" => cmd_summary(rest),
        "cache-stats" | "cache_stats" => cmd_cache_stats(rest),
        "checkpoints" => cmd_checkpoints(rest),
        "verify" => cmd_verify(rest),
        "clear" => cmd_clear(rest),
        "quarantine" => cmd_quarantine(rest),
        "path" => Ok(json!({
            "path": resolve_path(rest)?.display().to_string(),
        })),
        other => Err(format!(
            "unknown audit subcommand: {other}. try: tail | summary | cache-stats | checkpoints | verify | clear | quarantine | path"
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
    let kind = parse_string_opt(args, "--kind");

    let events = read_events(&path)?;
    let mut filtered: Vec<&Value> = events
        .iter()
        .filter(|e| match_session(e, session.as_deref()))
        .filter(|e| match_kind(e, kind.as_deref()))
        .collect();
    let total = filtered.len();
    let start = total.saturating_sub(lines);
    let tail: Vec<Value> = filtered.drain(start..).cloned().collect();
    Ok(json!({
        "path": path.display().to_string(),
        "matched": total,
        "returned": tail.len(),
        "events": tail,
    }))
}

fn cmd_summary(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let session = parse_string_opt(args, "--session");
    let mut total = 0u64;
    let mut by_kind: std::collections::BTreeMap<String, u64> = Default::default();
    let mut sessions: std::collections::BTreeSet<String> = Default::default();
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;

    scan_events(&path, |e| {
        if !match_session(e, session.as_deref()) {
            return;
        }
        total += 1;
        let kind = e
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string();
        *by_kind.entry(kind).or_insert(0) += 1;
        if let Some(s) = e.get("session_id").and_then(|v| v.as_str()) {
            sessions.insert(s.to_string());
        }
        if let Some(t) = e.get("timestamp").and_then(|v| v.as_str()) {
            if first_ts.is_none() {
                first_ts = Some(t.to_string());
            }
            last_ts = Some(t.to_string());
        }
    })?;
    let chain_bytes = crate::audit::hash_chain_storage_bytes(&path)?;
    let integrity = if chain_bytes > SUMMARY_VERIFY_LIMIT_BYTES {
        json!({
            "status": "skipped",
            "reason": "log plus linked archives exceed automatic verification limit",
            "bytes": chain_bytes,
            "limit_bytes": SUMMARY_VERIFY_LIMIT_BYTES,
        })
    } else {
        crate::audit::verify_hash_chain(&path)?
    };
    Ok(json!({
        "path": path.display().to_string(),
        "session_filter": session,
        "events": total,
        "by_kind": by_kind,
        "sessions": sessions.len(),
        "first_timestamp": first_ts,
        "last_timestamp": last_ts,
        "integrity": integrity,
    }))
}

fn cmd_cache_stats(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let session = parse_string_opt(args, "--session");
    let events = read_events(&path)?;
    let mut input_total: u64 = 0;
    let mut output_total: u64 = 0;
    let mut cache_read_total: u64 = 0;
    let mut cache_write_total: u64 = 0;
    let mut turns: u64 = 0;

    // Per-model breakdown so the operator can see which model is
    // burning the budget. Uses a BTreeMap for stable JSON key order.
    use std::collections::BTreeMap;
    #[derive(Default, Clone)]
    struct ModelAgg {
        turns: u64,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
    }
    let mut by_model: BTreeMap<String, ModelAgg> = BTreeMap::new();

    for e in events
        .iter()
        .filter(|e| match_session(e, session.as_deref()))
    {
        if e.get("kind").and_then(|v| v.as_str()) != Some("post_turn") {
            continue;
        }
        turns += 1;
        let input = e.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let output = e.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_read = e
            .get("cache_read_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_write = e
            .get("cache_write_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        input_total += input;
        output_total += output;
        cache_read_total += cache_read;
        cache_write_total += cache_write;

        // Per-model aggregation. An event missing the model field is
        // bucketed under "<unknown>" so it still shows up.
        let model_name = e
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>")
            .to_string();
        let entry = by_model.entry(model_name.clone()).or_default();
        entry.turns += 1;
        entry.input += input;
        entry.output += output;
        entry.cache_read += cache_read;
        entry.cache_write += cache_write;
    }
    // Cache hit rate ≈ cache_read / (cache_read + non-cached input). For
    // providers that don't expose cache tokens, all cache_* will be 0
    // and the rate degrades to 0.0 — which is the right value: zero
    // information about caching is observable.
    let billable_input = input_total.saturating_sub(cache_read_total);
    let denom = cache_read_total + billable_input;
    let hit_rate = if denom > 0 {
        (cache_read_total as f64) / (denom as f64)
    } else {
        0.0
    };

    let by_model_json: Value = by_model
        .into_iter()
        .map(|(name, a)| {
            (
                name,
                json!({
                    "turns": a.turns,
                    "input_tokens": a.input,
                    "output_tokens": a.output,
                    "cache_read_tokens": a.cache_read,
                    "cache_write_tokens": a.cache_write,
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();

    Ok(json!({
        "path": path.display().to_string(),
        "session_filter": session,
        "turns_observed": turns,
        "input_tokens_total": input_total,
        "output_tokens_total": output_total,
        "cache_read_tokens_total": cache_read_total,
        "cache_write_tokens_total": cache_write_total,
        "billable_input_tokens": billable_input,
        "cache_hit_rate": hit_rate,
        "by_model": by_model_json,
    }))
}

fn cmd_checkpoints(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let session = parse_string_opt(args, "--session");
    let events = read_events(&path)?;
    let mut rows: Vec<Value> = Vec::new();
    let mut ok_count: u64 = 0;
    let mut err_count: u64 = 0;
    for e in events
        .iter()
        .filter(|e| e.get("kind").and_then(|v| v.as_str()) == Some("pre_tool_checkpoint"))
        .filter(|e| match_session(e, session.as_deref()))
    {
        match e.get("status").and_then(|v| v.as_str()) {
            Some("ok") => ok_count += 1,
            Some("error") => err_count += 1,
            _ => {}
        }
        rows.push(json!({
            "timestamp": e.get("timestamp"),
            "session_id": e.get("session_id"),
            "turn": e.get("turn"),
            "tool_name": e.get("tool_name"),
            "tool_call_id": e.get("tool_call_id"),
            "status": e.get("status"),
            "checkpoint_id": e.get("checkpoint_id"),
            "error": e.get("error"),
            "description": e.get("description"),
        }));
    }
    Ok(json!({
        "path": path.display().to_string(),
        "session_filter": session,
        "total": rows.len(),
        "ok": ok_count,
        "errors": err_count,
        "events": rows,
    }))
}

fn cmd_clear(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let force = args.iter().any(|a| a == "--force" || a == "-f");
    if !force {
        return Err(format!(
            "refusing to clear {} without --force",
            path.display()
        ));
    }
    crate::audit::archive_hash_chain(&path)
}

fn cmd_verify(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    crate::audit::verify_hash_chain(&path)
}

fn cmd_quarantine(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let force = args
        .iter()
        .any(|argument| argument == "--force" || argument == "-f");
    if !force {
        return Err(format!(
            "refusing to quarantine {} without --force",
            path.display()
        ));
    }
    crate::audit::quarantine_hash_chain(&path)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn resolve_path(args: &[String]) -> Result<PathBuf, String> {
    if let Some(p) = parse_string_opt(args, "--path") {
        Ok(PathBuf::from(p))
    } else {
        Ok(crate::paths::agent_audit_log_path())
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
    match parse_string_opt(args, flag) {
        Some(v) => v
            .parse::<usize>()
            .map(Some)
            .map_err(|e| format!("invalid {flag} value: {e}")),
        None => Ok(None),
    }
}

fn match_session(e: &Value, session: Option<&str>) -> bool {
    match session {
        None => true,
        Some(want) => e.get("session_id").and_then(|v| v.as_str()) == Some(want),
    }
}

fn match_kind(e: &Value, kind: Option<&str>) -> bool {
    match kind {
        None => true,
        Some(want) => e.get("kind").and_then(|v| v.as_str()) == Some(want),
    }
}

/// Read the full audit log into memory as a `Vec<Value>`.
///
/// Lines that fail to parse as a JSON object are silently dropped
/// (so a half-flushed final line during a live tail doesn't break
/// the query). Returns `Ok(empty)` when the file does not exist —
/// "no audit log yet" is a valid state, not an error.
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
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            if v.is_object() {
                out.push(v);
            }
        }
    }
    Ok(out)
}

fn scan_events<F>(path: &Path, mut visitor: F) -> Result<(), String>
where
    F: FnMut(&Value),
{
    if !path.exists() {
        return Ok(());
    }
    let file = fs::File::open(path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.map_err(|error| format!("read {}: {error}", path.display()))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
            if value.is_object() {
                visitor(&value);
            }
        }
    }
    Ok(())
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/audit_cli.rs"
    ));
}
