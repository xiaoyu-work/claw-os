//! `cos agent run-log` — query the per-LLM-call run log.
//!
//! [`crate::agent::llm::run_log`] writes one JSONL record per
//! `provider.chat(...)` invocation to `<log_dir>/llm.jsonl`. That's a
//! finer-grained stream than the turn-level audit log queried by
//! [`super::audit_cli`] (one `cos agent ask` invocation produces N
//! turns, and each turn is exactly one LLM call). For diagnosing bad
//! answers and reproducing them on a specific engine version, the
//! per-call stream is what you want.
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
//! - `cost [--session SID]` — total USD cost across known-pricing
//!   models, plus per-model breakdown. Same math as
//!   `audit cache-stats` but at call granularity (more accurate when
//!   a single turn does multiple parallel completions).
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
        "cost" => cmd_cost(rest),
        "clear" => cmd_clear(rest),
        "path" => Ok(json!({
            "path": resolve_path(rest)?.display().to_string(),
        })),
        other => Err(format!(
            "unknown run-log subcommand: {other}. try: tail | summary | errors | engines | cost | clear | path"
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

fn cmd_cost(args: &[String]) -> Result<Value, String> {
    let path = resolve_path(args)?;
    let session = parse_string_opt(args, "--session");
    let events = read_events(&path)?;
    let mut by_model: BTreeMap<String, ModelCostAgg> = BTreeMap::new();
    #[derive(Default)]
    struct ModelCostAgg {
        calls: u64,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
        cost_usd: f64,
        pricing_known: bool,
    }
    let mut total_cost: f64 = 0.0;
    let mut total_calls: u64 = 0;
    let mut any_pricing_known = false;
    for e in events
        .iter()
        .filter(|e| match_session(e, session.as_deref()))
        .filter(|e| e.get("status").and_then(|v| v.as_str()) == Some("ok"))
    {
        total_calls += 1;
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
        let model_name = e
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("<unknown>")
            .to_string();
        let agg = by_model.entry(model_name.clone()).or_default();
        agg.calls += 1;
        agg.input += input;
        agg.output += output;
        agg.cache_read += cache_read;
        agg.cache_write += cache_write;
        if let Some(c) = crate::agent::llm::metadata::estimate_cost_usd(
            &model_name,
            input,
            output,
            cache_read,
            cache_write,
        ) {
            agg.cost_usd += c;
            agg.pricing_known = true;
            total_cost += c;
            any_pricing_known = true;
        }
    }
    let by_model_json: Value = by_model
        .into_iter()
        .map(|(k, a)| {
            (
                k,
                json!({
                    "calls": a.calls,
                    "input_tokens": a.input,
                    "output_tokens": a.output,
                    "cache_read_tokens": a.cache_read,
                    "cache_write_tokens": a.cache_write,
                    "cost_usd": if a.pricing_known { json!(a.cost_usd) } else { Value::Null },
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    let total_value = if any_pricing_known {
        json!(total_cost)
    } else {
        Value::Null
    };
    Ok(json!({
        "path": path.display().to_string(),
        "session_filter": session,
        "calls_observed": total_calls,
        "cost_total_usd": total_value,
        "by_model": by_model_json,
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
        Ok(crate::paths::llm_run_log_path())
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
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    // ---- fixtures ----

    fn fixture(records: &[Value]) -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llm.jsonl");
        let mut f = fs::File::create(&path).unwrap();
        for r in records {
            writeln!(f, "{}", serde_json::to_string(r).unwrap()).unwrap();
        }
        (dir, path)
    }

    /// Build a synthetic record. Pass extras as a JSON object to merge
    /// in additional fields (e.g. `engine_name`, `error`).
    fn rec(
        provider: &str,
        model: &str,
        session: &str,
        status: &str,
        ts_seq: u64,
        extra: Value,
    ) -> Value {
        let mut base = json!({
            "timestamp": format!("2026-01-01T00:00:{:02}Z", ts_seq),
            "provider": provider,
            "model": model,
            "session_id": session,
            "status": status,
            "duration_ms": 100,
            "input_tokens": 0,
            "output_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "finish_reason": if status == "error" { "error" } else { "stop" },
        });
        if let Value::Object(extras) = extra {
            for (k, v) in extras {
                base.as_object_mut().unwrap().insert(k, v);
            }
        }
        base
    }

    fn args(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    fn argv_with_path(path: &Path, extra: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = extra.iter().map(|s| s.to_string()).collect();
        v.push("--path".to_string());
        v.push(path.display().to_string());
        v
    }

    // ---- dispatcher ----

    #[test]
    fn unknown_subcommand_errors() {
        let err = run_log_cmd(&args(&["frobnicate"])).unwrap_err();
        assert!(err.contains("unknown run-log subcommand"), "got {err}");
    }

    #[test]
    fn empty_args_default_to_summary() {
        let v = run_log_cmd(&[]).unwrap();
        // The default summary returns the canonical llm.jsonl path
        // — we don't assert on the path because it depends on env.
        assert!(v.get("path").is_some());
        assert!(v.get("calls").is_some());
        assert!(v.get("by_provider").is_some());
    }

    #[test]
    fn path_subcommand_returns_resolved_path() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("anywhere.jsonl");
        let v = run_log_cmd(&argv_with_path(&p, &["path"])).unwrap();
        assert_eq!(v["path"], json!(p.display().to_string()));
    }

    // ---- tail ----

    #[test]
    fn tail_returns_chronological_oldest_first() {
        let records = vec![
            rec("anthropic", "claude-haiku-4-5", "s1", "ok", 0, json!({})),
            rec("anthropic", "claude-haiku-4-5", "s1", "ok", 1, json!({})),
            rec("anthropic", "claude-haiku-4-5", "s1", "ok", 2, json!({})),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["tail", "--lines", "2"])).unwrap();
        assert_eq!(v["matched"], json!(3));
        assert_eq!(v["returned"], json!(2));
        let evs = v["events"].as_array().unwrap();
        // Most recent two, in chronological order: ts_seq 1 then 2.
        assert_eq!(evs[0]["timestamp"], json!("2026-01-01T00:00:01Z"));
        assert_eq!(evs[1]["timestamp"], json!("2026-01-01T00:00:02Z"));
    }

    #[test]
    fn tail_filters_by_provider_and_model() {
        let records = vec![
            rec("anthropic", "claude-haiku-4-5", "s1", "ok", 0, json!({})),
            rec("openai", "gpt-4o-mini", "s1", "ok", 1, json!({})),
            rec("anthropic", "claude-sonnet-4-5", "s1", "ok", 2, json!({})),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["tail", "--provider", "anthropic"])).unwrap();
        assert_eq!(v["matched"], json!(2));
        let v = run_log_cmd(&argv_with_path(
            &p,
            &["tail", "--model", "claude-haiku-4-5"],
        ))
        .unwrap();
        assert_eq!(v["matched"], json!(1));
    }

    #[test]
    fn tail_filters_by_status() {
        let records = vec![
            rec("anthropic", "haiku", "s1", "ok", 0, json!({})),
            rec(
                "anthropic",
                "haiku",
                "s1",
                "error",
                1,
                json!({"error": "rate limited"}),
            ),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["tail", "--status", "error"])).unwrap();
        assert_eq!(v["matched"], json!(1));
        let evs = v["events"].as_array().unwrap();
        assert_eq!(evs[0]["error"], json!("rate limited"));
    }

    #[test]
    fn tail_invalid_lines_errors() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("none.jsonl");
        let err = run_log_cmd(&argv_with_path(&p, &["tail", "--lines", "abc"])).unwrap_err();
        assert!(err.contains("invalid --lines"), "got {err}");
    }

    #[test]
    fn tail_missing_file_returns_empty_not_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("does-not-exist.jsonl");
        let v = run_log_cmd(&argv_with_path(&p, &["tail"])).unwrap();
        assert_eq!(v["matched"], json!(0));
        assert_eq!(v["returned"], json!(0));
    }

    // ---- summary ----

    #[test]
    fn summary_counts_provider_model_status() {
        let records = vec![
            rec("anthropic", "haiku", "s1", "ok", 0, json!({})),
            rec("anthropic", "haiku", "s1", "ok", 1, json!({})),
            rec("anthropic", "sonnet", "s2", "error", 2, json!({})),
            rec("openai", "gpt-4o-mini", "s2", "ok", 3, json!({})),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["summary"])).unwrap();
        assert_eq!(v["calls"], json!(4));
        assert_eq!(v["sessions"], json!(2));
        assert_eq!(v["by_provider"]["anthropic"], json!(3));
        assert_eq!(v["by_provider"]["openai"], json!(1));
        assert_eq!(v["by_model"]["haiku"], json!(2));
        assert_eq!(v["by_status"]["ok"], json!(3));
        assert_eq!(v["by_status"]["error"], json!(1));
        assert_eq!(v["first_timestamp"], json!("2026-01-01T00:00:00Z"));
        assert_eq!(v["last_timestamp"], json!("2026-01-01T00:00:03Z"));
    }

    #[test]
    fn summary_session_filter_limits_counts() {
        let records = vec![
            rec("anthropic", "haiku", "s1", "ok", 0, json!({})),
            rec("anthropic", "haiku", "s2", "ok", 1, json!({})),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["summary", "--session", "s1"])).unwrap();
        assert_eq!(v["calls"], json!(1));
        assert_eq!(v["sessions"], json!(1));
    }

    #[test]
    fn summary_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("llm.jsonl");
        let mut f = fs::File::create(&p).unwrap();
        writeln!(
            f,
            r#"{{"timestamp":"t","provider":"p","model":"m","status":"ok"}}"#
        )
        .unwrap();
        writeln!(f, "{{ this is not valid json").unwrap();
        writeln!(f, r#""hello""#).unwrap(); // valid JSON but not an object
        writeln!(
            f,
            r#"{{"timestamp":"t","provider":"p","model":"m","status":"ok"}}"#
        )
        .unwrap();
        f.flush().unwrap();
        drop(f);
        let v = run_log_cmd(&argv_with_path(&p, &["summary"])).unwrap();
        assert_eq!(v["calls"], json!(2), "garbage lines should be skipped");
    }

    // ---- errors ----

    #[test]
    fn errors_lists_only_status_error() {
        let records = vec![
            rec("anthropic", "haiku", "s1", "ok", 0, json!({})),
            rec(
                "anthropic",
                "haiku",
                "s1",
                "error",
                1,
                json!({"error": "boom"}),
            ),
            rec(
                "anthropic",
                "haiku",
                "s2",
                "error",
                2,
                json!({"error": "splat"}),
            ),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["errors"])).unwrap();
        assert_eq!(v["total_errors"], json!(2));
        let evs = v["events"].as_array().unwrap();
        assert_eq!(evs.len(), 2);
        // Chronological order preserved.
        assert_eq!(evs[0]["error"], json!("boom"));
        assert_eq!(evs[1]["error"], json!("splat"));
    }

    #[test]
    fn errors_respects_session_and_limit() {
        let records = vec![
            rec("p", "m", "s1", "error", 0, json!({"error": "a"})),
            rec("p", "m", "s2", "error", 1, json!({"error": "b"})),
            rec("p", "m", "s2", "error", 2, json!({"error": "c"})),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(
            &p,
            &["errors", "--session", "s2", "--limit", "1"],
        ))
        .unwrap();
        assert_eq!(v["total_errors"], json!(2));
        assert_eq!(v["returned"], json!(1));
        // Limit takes most-recent.
        let evs = v["events"].as_array().unwrap();
        assert_eq!(evs[0]["error"], json!("c"));
    }

    // ---- engines ----

    #[test]
    fn engines_groups_by_name_at_version() {
        let records = vec![
            rec(
                "llama_local",
                "qwen3-7b",
                "s1",
                "ok",
                0,
                json!({"engine_name": "llama-cpp", "engine_version": "b3950"}),
            ),
            rec(
                "llama_local",
                "qwen3-7b",
                "s1",
                "ok",
                1,
                json!({"engine_name": "llama-cpp", "engine_version": "b3950"}),
            ),
            rec(
                "llama_local",
                "llama3-8b",
                "s1",
                "error",
                2,
                json!({"engine_name": "llama-cpp", "engine_version": "b4001"}),
            ),
            // Cloud record: no engine fields -> bucket as <cloud>.
            rec("anthropic", "haiku", "s1", "ok", 3, json!({})),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["engines"])).unwrap();
        let by = v["by_engine"].as_object().unwrap();
        assert_eq!(by["llama-cpp@b3950"]["calls"], json!(2));
        assert_eq!(by["llama-cpp@b3950"]["ok"], json!(2));
        assert_eq!(by["llama-cpp@b4001"]["errors"], json!(1));
        assert_eq!(by["<cloud>"]["calls"], json!(1));
        // Per-engine model breakdown.
        assert_eq!(by["llama-cpp@b3950"]["models"]["qwen3-7b"], json!(2));
    }

    // ---- cost ----

    #[test]
    fn cost_sums_known_model_pricing() {
        // claude-haiku-4-5: input $1/M, output $5/M.
        // 1M input + 1M output * 2 successful calls = $12 total.
        let records = vec![
            rec(
                "anthropic",
                "claude-haiku-4-5",
                "s1",
                "ok",
                0,
                json!({"input_tokens": 1_000_000_u64, "output_tokens": 1_000_000_u64}),
            ),
            rec(
                "anthropic",
                "claude-haiku-4-5",
                "s1",
                "ok",
                1,
                json!({"input_tokens": 1_000_000_u64, "output_tokens": 1_000_000_u64}),
            ),
            // Error records are excluded from cost.
            rec(
                "anthropic",
                "claude-haiku-4-5",
                "s1",
                "error",
                2,
                json!({"input_tokens": 1_000_000_u64, "output_tokens": 1_000_000_u64}),
            ),
        ];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["cost"])).unwrap();
        assert_eq!(v["calls_observed"], json!(2));
        let total = v["cost_total_usd"].as_f64().unwrap();
        assert!((total - 12.0).abs() < 1e-9, "got {total}");
        let m_cost = v["by_model"]["claude-haiku-4-5"]["cost_usd"]
            .as_f64()
            .unwrap();
        assert!((m_cost - 12.0).abs() < 1e-9);
    }

    #[test]
    fn cost_total_null_when_no_known_models() {
        let records = vec![rec(
            "anthropic",
            "made-up-9001",
            "s1",
            "ok",
            0,
            json!({"input_tokens": 1_000_000_u64}),
        )];
        let (_d, p) = fixture(&records);
        let v = run_log_cmd(&argv_with_path(&p, &["cost"])).unwrap();
        assert_eq!(v["cost_total_usd"], Value::Null);
        assert_eq!(v["by_model"]["made-up-9001"]["cost_usd"], Value::Null);
        assert_eq!(v["by_model"]["made-up-9001"]["calls"], json!(1));
    }

    // ---- clear ----

    #[test]
    fn clear_refuses_without_force() {
        let (_d, p) = fixture(&[rec("p", "m", "s1", "ok", 0, json!({}))]);
        let err = run_log_cmd(&argv_with_path(&p, &["clear"])).unwrap_err();
        assert!(err.contains("--force"), "got {err}");
        // File still exists.
        assert!(p.exists());
    }

    #[test]
    fn clear_with_force_removes_file() {
        let (_d, p) = fixture(&[rec("p", "m", "s1", "ok", 0, json!({}))]);
        let v = run_log_cmd(&argv_with_path(&p, &["clear", "--force"])).unwrap();
        assert_eq!(v["cleared"], json!(true));
        assert!(!p.exists());
    }

    #[test]
    fn clear_missing_file_with_force_reports_not_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.jsonl");
        let v = run_log_cmd(&argv_with_path(&p, &["clear", "--force"])).unwrap();
        assert_eq!(v["cleared"], json!(false));
        assert!(v["reason"]
            .as_str()
            .unwrap_or("")
            .contains("does not exist"));
    }
}
