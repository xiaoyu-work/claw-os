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
//! - `clear [--force]` — remove the audit log. Refuses without
//!   `--force` to avoid accidents.
//! - `path` — print the resolved audit log path. Useful for
//!   shell scripting without depending on platform-specific
//!   defaults.
//!
//! All subcommands accept `--path <FILE>` to point at an alternative
//! audit log (used by tests; rarely useful otherwise).
//!
//! ## Read semantics
//!
//! The log is append-only JSONL. Lines that fail to parse as JSON
//! objects are silently skipped (so a half-flushed last line during
//! a live tail can't break the query). All filters operate on the
//! parsed object; an event missing a queried field simply doesn't
//! match.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Top-level dispatcher for `cos agent audit <subcmd>`.
pub fn audit_cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("summary");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match sub {
        "tail" => cmd_tail(rest),
        "summary" => cmd_summary(rest),
        "cache-stats" | "cache_stats" => cmd_cache_stats(rest),
        "checkpoints" => cmd_checkpoints(rest),
        "clear" => cmd_clear(rest),
        "path" => Ok(json!({
            "path": resolve_path(rest)?.display().to_string(),
        })),
        other => Err(format!(
            "unknown audit subcommand: {other}. try: tail | summary | cache-stats | checkpoints | clear | path"
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
    let events = read_events(&path)?;
    let mut total = 0u64;
    let mut by_kind: std::collections::BTreeMap<String, u64> = Default::default();
    let mut sessions: std::collections::BTreeSet<String> = Default::default();
    let mut first_ts: Option<String> = None;
    let mut last_ts: Option<String> = None;

    for e in events
        .iter()
        .filter(|e| match_session(e, session.as_deref()))
    {
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
    }
    Ok(json!({
        "path": path.display().to_string(),
        "session_filter": session,
        "events": total,
        "by_kind": by_kind,
        "sessions": sessions.len(),
        "first_timestamp": first_ts,
        "last_timestamp": last_ts,
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

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Test helper: write `events` as JSONL to a fresh tempfile, return
    /// the path along with the tempdir (kept alive by the caller).
    fn fixture(events: &[Value]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        for e in events {
            writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
        }
        (dir, path)
    }

    fn ev(kind: &str, session: &str, turn: u64, extra: Value) -> Value {
        let mut base = json!({
            "kind": kind,
            "session_id": session,
            "turn": turn,
            "timestamp": format!("2026-01-01T00:00:{:02}Z", turn),
        });
        if let Value::Object(extra_map) = extra {
            for (k, v) in extra_map {
                base.as_object_mut().unwrap().insert(k, v);
            }
        }
        base
    }

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    fn argv_with_path(path: &Path, extra: &[&str]) -> Vec<String> {
        // Subcommand must be first so audit_cmd dispatches correctly.
        // Caller convention: extra[0] is the subcommand, optionally followed
        // by its own flags. We then append --path <p> for the cmd_* parser.
        let mut v: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        v.push("--path".to_string());
        v.push(path.display().to_string());
        v
    }

    // ---- dispatcher ----

    #[test]
    fn unknown_subcommand_errors() {
        let err = audit_cmd(&args(&["frobnicate"])).unwrap_err();
        assert!(err.contains("unknown audit subcommand"), "got {err}");
    }

    #[test]
    fn empty_args_default_to_summary() {
        let v = audit_cmd(&[]).unwrap();
        // The default summary returns the canonical agent.jsonl path —
        // we don't assert on the actual path because it depends on env.
        assert!(v.get("path").is_some());
        assert!(v.get("by_kind").is_some());
    }

    #[test]
    fn path_subcommand_returns_resolved_path() {
        let (_d, p) = fixture(&[]);
        let v = audit_cmd(&argv_with_path(&p, &["path"])).unwrap();
        assert_eq!(v["path"], json!(p.display().to_string()));
    }

    // ---- tail ----

    #[test]
    fn tail_returns_all_when_lines_exceed_total() {
        let events = vec![
            ev("pre_turn", "s1", 0, json!({})),
            ev("post_turn", "s1", 0, json!({})),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["tail", "--lines", "100"])).unwrap();
        assert_eq!(v["matched"], json!(2));
        assert_eq!(v["returned"], json!(2));
    }

    #[test]
    fn tail_limits_to_requested_count() {
        let events: Vec<Value> = (0..10)
            .map(|i| ev("pre_turn", "s1", i, json!({})))
            .collect();
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["tail", "--lines", "3"])).unwrap();
        assert_eq!(v["matched"], json!(10));
        assert_eq!(v["returned"], json!(3));
        let tail = v["events"].as_array().unwrap();
        // Most recent 3 = turns 7, 8, 9.
        assert_eq!(tail[0]["turn"], json!(7));
        assert_eq!(tail[2]["turn"], json!(9));
    }

    #[test]
    fn tail_filters_by_session() {
        let events = vec![
            ev("pre_turn", "s1", 0, json!({})),
            ev("pre_turn", "s2", 0, json!({})),
            ev("pre_turn", "s1", 1, json!({})),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["tail", "--session", "s1"])).unwrap();
        assert_eq!(v["matched"], json!(2));
        for e in v["events"].as_array().unwrap() {
            assert_eq!(e["session_id"], json!("s1"));
        }
    }

    #[test]
    fn tail_filters_by_kind() {
        let events = vec![
            ev("pre_turn", "s1", 0, json!({})),
            ev("post_turn", "s1", 0, json!({})),
            ev("pre_tool", "s1", 0, json!({})),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["tail", "--kind", "post_turn"])).unwrap();
        assert_eq!(v["matched"], json!(1));
        assert_eq!(v["events"][0]["kind"], json!("post_turn"));
    }

    #[test]
    fn tail_invalid_lines_errors() {
        let (_d, p) = fixture(&[]);
        let err = audit_cmd(&argv_with_path(&p, &["tail", "--lines", "not-a-number"])).unwrap_err();
        assert!(err.contains("invalid --lines"), "got {err}");
    }

    #[test]
    fn tail_missing_file_returns_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.jsonl");
        let v = audit_cmd(&argv_with_path(&p, &["tail"])).unwrap();
        assert_eq!(v["matched"], json!(0));
    }

    // ---- summary ----

    #[test]
    fn summary_counts_by_kind_and_session() {
        let events = vec![
            ev("pre_turn", "s1", 0, json!({})),
            ev("post_turn", "s1", 0, json!({})),
            ev("pre_turn", "s2", 0, json!({})),
            ev("post_turn", "s2", 0, json!({})),
            ev("pre_tool_checkpoint", "s1", 1, json!({"status": "ok"})),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["summary"])).unwrap();
        assert_eq!(v["events"], json!(5));
        assert_eq!(v["sessions"], json!(2));
        assert_eq!(v["by_kind"]["pre_turn"], json!(2));
        assert_eq!(v["by_kind"]["post_turn"], json!(2));
        assert_eq!(v["by_kind"]["pre_tool_checkpoint"], json!(1));
    }

    #[test]
    fn summary_session_filter_narrows_counts() {
        let events = vec![
            ev("pre_turn", "s1", 0, json!({})),
            ev("post_turn", "s1", 0, json!({})),
            ev("pre_turn", "s2", 0, json!({})),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["summary", "--session", "s1"])).unwrap();
        assert_eq!(v["events"], json!(2));
        assert_eq!(v["sessions"], json!(1));
        assert_eq!(v["session_filter"], json!("s1"));
    }

    #[test]
    fn summary_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("agent.jsonl");
        let mut f = fs::File::create(&p).unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&ev("pre_turn", "s1", 0, json!({}))).unwrap()
        )
        .unwrap();
        writeln!(f, "this is not json").unwrap();
        writeln!(
            f,
            "{}",
            serde_json::to_string(&ev("post_turn", "s1", 0, json!({}))).unwrap()
        )
        .unwrap();
        let v = audit_cmd(&argv_with_path(&p, &["summary"])).unwrap();
        assert_eq!(v["events"], json!(2), "garbage line should be skipped");
    }

    // ---- cache-stats ----

    #[test]
    fn cache_stats_aggregates_post_turn_only() {
        let events = vec![
            // pre_turn / pre_tool / post_tool should NOT be counted.
            ev("pre_turn", "s1", 0, json!({"input_tokens": 999})),
            ev("pre_tool", "s1", 0, json!({"input_tokens": 999})),
            ev(
                "post_turn",
                "s1",
                0,
                json!({
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_tokens": 30,
                    "cache_write_tokens": 5,
                }),
            ),
            ev(
                "post_turn",
                "s1",
                1,
                json!({
                    "input_tokens": 200,
                    "output_tokens": 100,
                    "cache_read_tokens": 70,
                    "cache_write_tokens": 0,
                }),
            ),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["cache-stats"])).unwrap();
        assert_eq!(v["turns_observed"], json!(2));
        assert_eq!(v["input_tokens_total"], json!(300));
        assert_eq!(v["output_tokens_total"], json!(150));
        assert_eq!(v["cache_read_tokens_total"], json!(100));
        assert_eq!(v["cache_write_tokens_total"], json!(5));
        // billable input = 300 - 100 = 200; hit_rate = 100 / (100 + 200) ≈ 0.333…
        assert_eq!(v["billable_input_tokens"], json!(200));
        let hr = v["cache_hit_rate"].as_f64().unwrap();
        assert!((hr - 0.333_333).abs() < 0.001, "got {hr}");
    }

    #[test]
    fn cache_stats_zero_when_no_post_turn_events() {
        let events = vec![ev("pre_turn", "s1", 0, json!({}))];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["cache-stats"])).unwrap();
        assert_eq!(v["turns_observed"], json!(0));
        assert_eq!(v["cache_hit_rate"], json!(0.0));
    }

    #[test]
    fn cache_stats_session_filter_isolates_sums() {
        let events = vec![
            ev(
                "post_turn",
                "s1",
                0,
                json!({"input_tokens": 100, "cache_read_tokens": 50}),
            ),
            ev(
                "post_turn",
                "s2",
                0,
                json!({"input_tokens": 100, "cache_read_tokens": 0}),
            ),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["cache-stats", "--session", "s1"])).unwrap();
        assert_eq!(v["turns_observed"], json!(1));
        assert_eq!(v["cache_read_tokens_total"], json!(50));
    }

    #[test]
    fn cache_stats_per_model_breakdown_aggregates_across_turns() {
        // Two turns on claude-haiku-4-5, one on an unknown model.
        let events = vec![
            ev(
                "post_turn",
                "s1",
                0,
                json!({
                    "model": "claude-haiku-4-5",
                    "input_tokens": 1_000_000_u64,
                    "output_tokens": 0,
                }),
            ),
            ev(
                "post_turn",
                "s1",
                1,
                json!({
                    "model": "claude-haiku-4-5",
                    "input_tokens": 1_000_000_u64,
                    "output_tokens": 0,
                }),
            ),
            ev(
                "post_turn",
                "s2",
                0,
                json!({
                    "model": "made-up-model-9001",
                    "input_tokens": 500_000_u64,
                }),
            ),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["cache-stats"])).unwrap();
        let by_model = v["by_model"].as_object().unwrap();
        assert_eq!(by_model["claude-haiku-4-5"]["turns"], json!(2));
        assert_eq!(
            by_model["claude-haiku-4-5"]["input_tokens"],
            json!(2_000_000_u64)
        );
        assert_eq!(by_model["made-up-model-9001"]["turns"], json!(1));
        assert_eq!(
            by_model["made-up-model-9001"]["input_tokens"],
            json!(500_000_u64)
        );
    }

    // ---- checkpoints ----

    #[test]
    fn checkpoints_lists_pre_tool_checkpoint_events_only() {
        let events = vec![
            ev("pre_turn", "s1", 0, json!({})),
            ev(
                "pre_tool_checkpoint",
                "s1",
                1,
                json!({
                    "status": "ok",
                    "tool_name": "cos_sandbox",
                    "tool_call_id": "c1",
                    "checkpoint_id": "cp-1",
                }),
            ),
            ev(
                "pre_tool_checkpoint",
                "s1",
                2,
                json!({
                    "status": "error",
                    "tool_name": "cos_proc",
                    "tool_call_id": "c2",
                    "error": "overlayfs unavailable",
                }),
            ),
            ev("post_turn", "s1", 2, json!({})),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["checkpoints"])).unwrap();
        assert_eq!(v["total"], json!(2));
        assert_eq!(v["ok"], json!(1));
        assert_eq!(v["errors"], json!(1));
        let rows = v["events"].as_array().unwrap();
        assert_eq!(rows[0]["tool_name"], json!("cos_sandbox"));
        assert_eq!(rows[0]["checkpoint_id"], json!("cp-1"));
        assert_eq!(rows[1]["error"], json!("overlayfs unavailable"));
    }

    #[test]
    fn checkpoints_session_filter_narrows() {
        let events = vec![
            ev(
                "pre_tool_checkpoint",
                "s1",
                0,
                json!({"status": "ok", "tool_name": "cos_sandbox"}),
            ),
            ev(
                "pre_tool_checkpoint",
                "s2",
                0,
                json!({"status": "ok", "tool_name": "cos_proc"}),
            ),
        ];
        let (_d, p) = fixture(&events);
        let v = audit_cmd(&argv_with_path(&p, &["checkpoints", "--session", "s1"])).unwrap();
        assert_eq!(v["total"], json!(1));
        assert_eq!(v["events"][0]["tool_name"], json!("cos_sandbox"));
    }

    // ---- clear ----

    #[test]
    fn clear_refuses_without_force() {
        let (_d, p) = fixture(&[ev("pre_turn", "s1", 0, json!({}))]);
        let err = audit_cmd(&argv_with_path(&p, &["clear"])).unwrap_err();
        assert!(err.contains("--force"), "got {err}");
        assert!(p.exists(), "must NOT have removed the file");
    }

    #[test]
    fn clear_with_force_removes_file() {
        let (_d, p) = fixture(&[ev("pre_turn", "s1", 0, json!({}))]);
        let v = audit_cmd(&argv_with_path(&p, &["clear", "--force"])).unwrap();
        assert_eq!(v["cleared"], json!(true));
        assert!(!p.exists());
    }

    #[test]
    fn clear_missing_file_with_force_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.jsonl");
        let v = audit_cmd(&argv_with_path(&p, &["clear", "--force"])).unwrap();
        assert_eq!(v["cleared"], json!(false));
    }
}
