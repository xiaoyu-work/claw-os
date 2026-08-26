use super::*;
use std::io::Write;
use tempfile::TempDir;

// ---- fixtures ----

fn fixture(records: &[Value]) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ai.jsonl");
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
    // The default summary returns the canonical ai.jsonl path
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
    let p = dir.path().join("ai.jsonl");
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
