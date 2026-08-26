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
fn clear_with_force_archives_and_restarts_chain() {
    let (_d, p) = fixture(&[ev("pre_turn", "s1", 0, json!({}))]);
    let v = audit_cmd(&argv_with_path(&p, &["clear", "--force"])).unwrap();
    assert_eq!(v["cleared"], json!(true));
    assert_eq!(v["archived"], json!(true));
    assert!(p.exists(), "a new archive-anchor chain should exist");
    assert!(Path::new(v["archive_path"].as_str().unwrap()).exists());
    let verified = audit_cmd(&argv_with_path(&p, &["verify"])).unwrap();
    assert_eq!(verified["valid"], json!(true));
}

#[test]
fn clear_missing_file_with_force_is_noop() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("does-not-exist.jsonl");
    let v = audit_cmd(&argv_with_path(&p, &["clear", "--force"])).unwrap();
    assert_eq!(v["cleared"], json!(false));
}

// ---- verify ----

#[test]
fn verify_accepts_hash_chained_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.jsonl");
    crate::audit::log_chained_event(&path, ev("pre_turn", "s1", 1, json!({})));
    crate::audit::log_chained_event(&path, ev("post_turn", "s1", 1, json!({})));
    let value = audit_cmd(&argv_with_path(&path, &["verify"])).unwrap();
    assert_eq!(value["valid"], json!(true));
    assert_eq!(value["events"], json!(2));
    assert!(value["last_hash"]
        .as_str()
        .is_some_and(|hash| hash.len() == 64));
}

#[test]
fn verify_detects_tampered_event() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.jsonl");
    crate::audit::log_chained_event(&path, ev("pre_turn", "s1", 1, json!({})));
    crate::audit::log_chained_event(&path, ev("post_turn", "s1", 1, json!({})));
    let body = fs::read_to_string(&path).unwrap();
    fs::write(&path, body.replacen("\"pre_turn\"", "\"pre_tool\"", 1)).unwrap();
    let value = audit_cmd(&argv_with_path(&path, &["verify"])).unwrap();
    assert_eq!(value["valid"], json!(false));
    assert!(!value["errors"].as_array().unwrap().is_empty());
}

#[test]
fn verify_follows_multiple_archive_generations() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.jsonl");
    crate::audit::log_chained_event(&path, ev("pre_turn", "s1", 1, json!({})));
    audit_cmd(&argv_with_path(&path, &["clear", "--force"])).unwrap();
    crate::audit::log_chained_event(&path, ev("post_turn", "s1", 1, json!({})));
    audit_cmd(&argv_with_path(&path, &["clear", "--force"])).unwrap();
    let value = audit_cmd(&argv_with_path(&path, &["verify"])).unwrap();
    assert_eq!(value["valid"], json!(true));
    assert_eq!(value["archives"].as_array().unwrap().len(), 1);
    assert!(value["archives"][0]["chain"]["archives"]
        .as_array()
        .is_some_and(|archives| !archives.is_empty()));
}

#[test]
fn verify_rejects_empty_log_with_stale_head() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.jsonl");
    fs::write(&path, "").unwrap();
    fs::write(
        path.with_file_name("agent.jsonl.head"),
        r#"{"chain_version":1,"chain_id":"00000000000000000000000000000000","sequence":1,"this_hash":"0000000000000000000000000000000000000000000000000000000000000000"}"#,
    )
    .unwrap();
    let value = audit_cmd(&argv_with_path(&path, &["verify"])).unwrap();
    assert_eq!(value["valid"], json!(false));
}

#[test]
fn verify_reports_legacy_log_without_tamper_failure() {
    let (_dir, path) = fixture(&[ev("pre_turn", "s1", 1, json!({}))]);
    let value = audit_cmd(&argv_with_path(&path, &["verify"])).unwrap();
    assert_eq!(value["legacy"], json!(true));
    assert_eq!(value["status"], json!("legacy"));
}

#[test]
fn append_recovers_hash_anchored_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.jsonl");
    crate::audit::log_chained_event(&path, ev("pre_turn", "s1", 1, json!({})));
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "{{").unwrap();
    crate::audit::log_chained_event(&path, ev("post_turn", "s1", 1, json!({})));
    let value = audit_cmd(&argv_with_path(&path, &["verify"])).unwrap();
    assert_eq!(value["valid"], json!(true));
    assert_eq!(value["archives"][0]["kind"], json!("torn-tail"));
}

#[test]
fn quarantine_acknowledges_invalid_history_without_deleting_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.jsonl");
    crate::audit::log_chained_event(&path, ev("pre_turn", "s1", 1, json!({})));
    let body = fs::read_to_string(&path).unwrap();
    fs::write(&path, body.replace("\"pre_turn\"", "\"pre_tool\"")).unwrap();
    let quarantined = audit_cmd(&argv_with_path(&path, &["quarantine", "--force"])).unwrap();
    assert_eq!(quarantined["quarantined"], json!(true));
    let value = audit_cmd(&argv_with_path(&path, &["verify"])).unwrap();
    assert_eq!(value["valid"], json!(true));
    assert_eq!(value["archives"][0]["kind"], json!("quarantined-invalid"));
    assert!(!value["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn quarantine_refuses_valid_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.jsonl");
    crate::audit::log_chained_event(&path, ev("pre_turn", "s1", 1, json!({})));
    let error = audit_cmd(&argv_with_path(&path, &["quarantine", "--force"])).unwrap_err();
    assert!(error.contains("only for an invalid"));
}
