use super::*;

fn args(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|s| s.to_string()).collect()
}

#[test]
fn doctor_returns_top_level_shape() {
    let v = doctor_cmd(&args(&[])).unwrap();
    assert!(v.get("status").is_some());
    assert!(v.get("summary").is_some());
    assert!(v.get("flags").is_some());
    let checks = v.get("checks").unwrap().as_object().unwrap();
    for k in [
        "provider", "engines", "memory", "audit", "run_log",
        "usage", "insights", "skills", "hooks",
    ] {
        assert!(checks.contains_key(k), "missing check: {k}");
        assert!(
            checks[k].get("status").is_some(),
            "check {k} missing status"
        );
    }
}

#[test]
fn doctor_summary_matches_subcheck_counts() {
    let v = doctor_cmd(&args(&[])).unwrap();
    let summary = v.get("summary").unwrap().as_object().unwrap();
    let checks = v.get("checks").unwrap().as_object().unwrap();
    let mut ok = 0u32;
    let mut warn = 0u32;
    let mut fail = 0u32;
    for (_k, c) in checks {
        match c.get("status").and_then(|s| s.as_str()).unwrap_or("ok") {
            "ok" => ok += 1,
            "warn" => warn += 1,
            "fail" => fail += 1,
            _ => {}
        }
    }
    assert_eq!(summary["ok"], json!(ok));
    assert_eq!(summary["warn"], json!(warn));
    assert_eq!(summary["fail"], json!(fail));
}

#[test]
fn quick_mode_skips_log_file_scans() {
    let v = doctor_cmd(&args(&["--quick"])).unwrap();
    let checks = v.get("checks").unwrap();
    assert_eq!(checks["audit"]["status"], json!("skipped"));
    assert_eq!(checks["run_log"]["status"], json!("skipped"));
    assert_eq!(checks["usage"]["status"], json!("skipped"));
    assert_eq!(checks["insights"]["status"], json!("skipped"));
    let flags = v.get("flags").unwrap();
    assert_eq!(flags["quick"], json!(true));
    // --quick suppresses --probe-network even if asked.
    assert_eq!(flags["probe_network"], json!(false));
}

#[test]
fn quick_mode_forces_probe_network_off() {
    let v = doctor_cmd(&args(&["--quick", "--probe-network"])).unwrap();
    let flags = v.get("flags").unwrap();
    assert_eq!(flags["quick"], json!(true));
    assert_eq!(flags["probe_network"], json!(false));
    assert_eq!(flags["probe_network_requested"], json!(true));
    // No network_probe block should be attached when --quick wins.
    let provider = v.get("checks").unwrap().get("provider").unwrap();
    assert!(provider.get("network_probe").is_none());
}

#[test]
fn doctor_rejects_unknown_flag() {
    let err = doctor_cmd(&args(&["--bogus"])).unwrap_err();
    assert!(err.to_lowercase().contains("unknown doctor flag"));
}

#[test]
fn doctor_rejects_zero_probe_timeout() {
    let err = doctor_cmd(&args(&["--probe-timeout", "0"])).unwrap_err();
    assert!(err.contains("> 0"));
}

#[test]
fn check_provider_reports_matrix_and_active() {
    let v = check_provider(false, 30);
    assert!(v.get("active").is_some());
    assert!(v.get("registered").is_some());
    assert!(v.get("available").is_some());
    assert!(v.get("configured").is_some());
    assert!(v.get("configuration_error").is_some());
    let matrix = v.get("matrix").and_then(|m| m.as_array()).expect("matrix array");
    // Every registered provider should show up in the matrix
    // (no --names filter applied), and each entry must carry
    // the env_present / credential_present / configured shape
    // that the dev `providers` command exposes.
    assert!(!matrix.is_empty(), "matrix should list available providers");
    for entry in matrix {
        assert!(entry.get("name").is_some());
        assert!(entry.get("env_present").is_some());
        assert!(entry.get("credential_present").is_some());
        assert!(entry.get("configured").is_some());
        assert!(entry.get("configuration_error").is_some());
    }
    // No network probe was requested → no network_probe block.
    assert!(v.get("network_probe").is_none());
    let status = v.get("status").and_then(|s| s.as_str()).unwrap();
    assert!(matches!(status, "ok" | "fail"));
}

#[test]
fn check_engines_returns_list_and_status() {
    let v = check_engines();
    let linked = v.get("linked").unwrap().as_array().unwrap();
    let status = v.get("status").and_then(|s| s.as_str()).unwrap();
    if linked.is_empty() {
        assert_eq!(status, "warn");
    } else {
        assert_eq!(status, "ok");
    }
}

#[test]
fn check_memory_attaches_stats_block_when_db_open() {
    // The default DB lives at agent_memory_db_path() and may or
    // may not exist; check_memory creates it on demand. Either
    // way the stats block should be present (object, possibly
    // with all-zero counts on a fresh install).
    let v = check_memory();
    let memory_db = v.get("memory_db").expect("memory_db field");
    // Only assert the stats sub-shape when the memory_db itself
    // opened successfully — fail-path doesn't carry stats.
    if memory_db.get("status").and_then(|s| s.as_str()) == Some("ok") {
        let stats = memory_db.get("stats").expect("stats field");
        assert!(stats.is_object(), "stats must be an object");
        assert!(stats.get("messages_last_7d").is_some());
        assert!(stats.get("total_sessions").is_some());
    }
}

#[test]
fn check_log_file_reports_ok_with_note_when_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("does-not-exist.jsonl");
    let v = check_log_file(&p, "test");
    // Missing log files are normal on fresh installs; doctor
    // should not warn (which would be alarmist) but should
    // explain why lines/bytes are zero.
    assert_eq!(v["status"], json!("ok"));
    assert_eq!(v["label"], json!("test"));
    assert_eq!(v["lines"], json!(0));
    assert_eq!(v["bytes"], json!(0));
    assert!(
        v["note"].as_str().unwrap_or("").contains("not yet created"),
        "expected note about not-yet-created log, got {:?}",
        v["note"]
    );
}

#[test]
fn check_log_file_reports_lines_when_present() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("present.jsonl");
    std::fs::write(&p, "{}\n{}\n{}\n").unwrap();
    let v = check_log_file(&p, "test");
    assert_eq!(v["status"], json!("ok"));
    assert_eq!(v["lines"], json!(3));
}

#[test]
fn run_log_and_insights_derive_from_the_owner_scoped_usage_snapshot() {
    let usage = json!({
        "status": "ok",
        "scope": "overall",
        "since": "2026-08-20T00:00:00Z",
        "log": "/var/lib/cos/users/1000/logs/ai.jsonl",
        "total": {
            "calls": 3,
            "input_tokens": 120,
            "output_tokens": 30,
            "finish_reasons": {"stop": 2, "tool_use": 1},
            "errors": 1
        },
        "by_provider": {"anthropic": {"calls": 3}},
        "by_model": {"claude-sonnet": {"calls": 3}},
        "log_lines": 4,
        "log_bytes": 1024,
        "parse_errors": 0,
    });

    let run_log = check_run_log_from_usage(&usage);
    let insights = check_insights_from_usage(&usage);

    assert_eq!(run_log["path"], usage["log"]);
    assert_eq!(run_log["lines"], 4);
    assert_eq!(run_log["bytes"], 1024);
    assert_eq!(run_log["records"], 3);
    assert_eq!(insights["log"], usage["log"]);
    assert_eq!(insights["providers_seen"], 1);
    assert_eq!(insights["overall"], usage["total"]);
    assert_eq!(insights["overall"]["finish_reasons"]["tool_use"], 1);
    assert_eq!(insights["overall"]["errors"], 1);
}

#[test]
fn check_skills_warns_on_load_errors_only() {
    let v = check_skills();
    let status = v.get("status").and_then(|s| s.as_str()).unwrap();
    let errors = v.get("errors").and_then(|n| n.as_u64()).unwrap_or(0);
    if errors > 0 {
        assert_eq!(status, "warn");
    } else {
        assert_eq!(status, "ok");
    }
}

#[test]
fn check_hooks_returns_registered_and_persisted() {
    let v = check_hooks();
    assert!(v.get("registered").is_some());
    assert!(v.get("persisted").is_some());
    assert!(v.get("config_path").is_some());
}
