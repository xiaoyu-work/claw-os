use super::*;

#[test]
fn llm_providers_returns_known_providers_with_counts() {
    let v = llm_cmd(&["providers".into()]).expect("llm providers ok");
    let count = v.get("count").and_then(|c| c.as_u64()).expect("count");
    assert!(count >= 1, "expected at least one provider, got {count}");
    let providers = v
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers array");
    for p in providers {
        assert!(p.get("name").and_then(|x| x.as_str()).is_some());
        assert!(p.get("models").and_then(|x| x.as_u64()).is_some());
    }
    let total = v
        .get("total_entries")
        .and_then(|c| c.as_u64())
        .expect("total_entries");
    assert!(total >= count, "total entries should be >= provider count");
}

#[test]
fn llm_providers_default_when_no_args() {
    // The bare `cos agent llm` invocation with no subcommand
    // defaults to providers (mirrors `usage` defaulting to overall).
    let v = llm_cmd(&[]).expect("llm default ok");
    assert!(v.get("providers").is_some());
}

#[test]
fn llm_models_filters_by_provider() {
    let v = llm_cmd(&["models".into(), "--provider".into(), "anthropic".into()])
        .expect("llm models filter ok");
    let models = v
        .get("models")
        .and_then(|m| m.as_array())
        .expect("models array");
    assert!(
        !models.is_empty(),
        "anthropic should have at least one model"
    );
    for m in models {
        assert_eq!(
            m.get("provider").and_then(|p| p.as_str()),
            Some("anthropic"),
            "filter leaked: {m:?}"
        );
    }
}

#[test]
fn llm_models_unfiltered_returns_all() {
    let v = llm_cmd(&["models".into()]).expect("llm models all ok");
    let n = v.get("count").and_then(|c| c.as_u64()).expect("count");
    assert!(n >= 1);
}

#[test]
fn llm_model_unknown_errors() {
    let err = llm_cmd(&["model".into(), "definitely-not-a-real-model".into()]).unwrap_err();
    assert!(err.contains("unknown model"));
}

#[test]
fn llm_model_returns_pricing_and_capability_fields() {
    // Pick the first model the registry reports for the first
    // known provider so this test is robust to table changes.
    let providers = llm_cmd(&["providers".into()]).expect("providers ok");
    let first = providers
        .get("providers")
        .and_then(|p| p.as_array())
        .and_then(|arr| arr.first())
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .expect("at least one known provider")
        .to_string();
    let models = llm_cmd(&["models".into(), "--provider".into(), first]).expect("models ok");
    let first_model = models
        .get("models")
        .and_then(|m| m.as_array())
        .and_then(|arr| arr.first())
        .and_then(|m| m.get("name"))
        .and_then(|n| n.as_str())
        .expect("at least one model for first provider")
        .to_string();
    let v = llm_cmd(&["model".into(), first_model]).expect("model ok");
    assert!(v.get("name").and_then(|x| x.as_str()).is_some());
    assert!(v.get("provider").and_then(|x| x.as_str()).is_some());
    assert!(v.get("context_window").is_some());
    assert!(v.get("supports_tools").is_some());
}

// ---- compress_cmd ----

#[test]
fn compress_cmd_show_config_returns_defaults() {
    let v = compress_cmd(&["show-config".into()]).expect("show-config ok");
    assert!(v.get("target_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0);
    assert!(
        v.get("trigger_tokens")
            .and_then(|n| n.as_u64())
            .unwrap_or(0)
            > 0
    );
    assert!(v.get("keep_tail_tokens").and_then(|n| n.as_u64()).is_some());
    assert!(v
        .get("summary_max_tokens")
        .and_then(|n| n.as_u64())
        .is_some());
}

#[test]
fn compress_cmd_default_subcommand_is_show_config() {
    let v = compress_cmd(&[]).expect("default ok");
    assert!(v.get("target_tokens").is_some());
}

#[test]
fn compress_cmd_check_requires_file() {
    let err = compress_cmd(&["check".into()]).unwrap_err();
    assert!(err.contains("--file"));
}

#[test]
fn compress_cmd_check_reports_zero_for_empty_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    std::fs::write(&path, "").expect("write");
    let v = compress_cmd(&["check".into(), "--file".into(), path.display().to_string()])
        .expect("check ok");
    assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(v.get("total_tokens").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(
        v.get("would_trigger").and_then(|b| b.as_bool()),
        Some(false)
    );
}

#[test]
fn compress_cmd_check_skips_blank_lines() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    let body = format!(
        "{}\n\n{}\n",
        serde_json::to_string(&crate::agent::llm::types::Message::user_text("hello")).unwrap(),
        serde_json::to_string(&crate::agent::llm::types::Message::assistant_text(
            "hi back"
        ))
        .unwrap(),
    );
    std::fs::write(&path, body).expect("write");
    let v = compress_cmd(&["check".into(), "--file".into(), path.display().to_string()])
        .expect("check ok");
    assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(2));
}

#[test]
fn compress_cmd_check_counts_by_role() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    let body = format!(
        "{}\n{}\n{}\n",
        serde_json::to_string(&crate::agent::llm::types::Message::user_text("u1")).unwrap(),
        serde_json::to_string(&crate::agent::llm::types::Message::assistant_text("a1")).unwrap(),
        serde_json::to_string(&crate::agent::llm::types::Message::user_text("u2")).unwrap(),
    );
    std::fs::write(&path, body).expect("write");
    let v = compress_cmd(&["check".into(), "--file".into(), path.display().to_string()])
        .expect("check ok");
    let by_role = v.get("by_role").expect("by_role");
    let counts = by_role.get("counts").expect("counts");
    assert_eq!(counts.get("user").and_then(|n| n.as_u64()), Some(2));
    assert_eq!(counts.get("assistant").and_then(|n| n.as_u64()), Some(1));
}

#[test]
fn compress_cmd_check_includes_system_tokens_when_provided() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    std::fs::write(&path, "").expect("write");
    let v = compress_cmd(&[
        "check".into(),
        "--file".into(),
        path.display().to_string(),
        "--system".into(),
        "you are a helpful assistant".into(),
    ])
    .expect("check ok");
    assert!(v.get("system_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0);
}

#[test]
fn compress_cmd_check_system_file_loads_text() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    std::fs::write(&path, "").expect("write");
    let sys_path = dir.path().join("sys.txt");
    std::fs::write(&sys_path, "system prompt body").expect("write");
    let v = compress_cmd(&[
        "check".into(),
        "--file".into(),
        path.display().to_string(),
        "--system-file".into(),
        sys_path.display().to_string(),
    ])
    .expect("check ok");
    assert!(v.get("system_tokens").and_then(|n| n.as_u64()).unwrap_or(0) > 0);
}

#[test]
fn compress_cmd_check_system_and_system_file_conflict() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    std::fs::write(&path, "").expect("write");
    let sys_path = dir.path().join("sys.txt");
    std::fs::write(&sys_path, "x").expect("write");
    let err = compress_cmd(&[
        "check".into(),
        "--file".into(),
        path.display().to_string(),
        "--system".into(),
        "y".into(),
        "--system-file".into(),
        sys_path.display().to_string(),
    ])
    .unwrap_err();
    assert!(err.contains("mutually exclusive"));
}

#[test]
fn compress_cmd_check_would_trigger_when_total_meets_trigger() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    let big = "x".repeat(2000);
    let body = format!(
        "{}\n",
        serde_json::to_string(&crate::agent::llm::types::Message::user_text(&big)).unwrap(),
    );
    std::fs::write(&path, body).expect("write");
    let v = compress_cmd(&[
        "check".into(),
        "--file".into(),
        path.display().to_string(),
        "--trigger".into(),
        "10".into(),
    ])
    .expect("check ok");
    assert_eq!(v.get("would_trigger").and_then(|b| b.as_bool()), Some(true));
}

#[test]
fn compress_cmd_check_overrides_config() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    std::fs::write(&path, "").expect("write");
    let v = compress_cmd(&[
        "check".into(),
        "--file".into(),
        path.display().to_string(),
        "--trigger".into(),
        "12345".into(),
        "--target".into(),
        "8000".into(),
        "--keep-tail".into(),
        "1234".into(),
        "--summary-max".into(),
        "777".into(),
    ])
    .expect("check ok");
    let cfg = v.get("config").expect("config");
    assert_eq!(
        cfg.get("trigger_tokens").and_then(|n| n.as_u64()),
        Some(12345)
    );
    assert_eq!(
        cfg.get("target_tokens").and_then(|n| n.as_u64()),
        Some(8000)
    );
    assert_eq!(
        cfg.get("keep_tail_tokens").and_then(|n| n.as_u64()),
        Some(1234)
    );
    assert_eq!(
        cfg.get("summary_max_tokens").and_then(|n| n.as_u64()),
        Some(777)
    );
}

#[test]
fn compress_cmd_check_rejects_corrupt_jsonl() {
    let dir = tempfile::tempdir().expect("tmp");
    let path = dir.path().join("conv.jsonl");
    std::fs::write(&path, "{not json}\n").expect("write");
    let err =
        compress_cmd(&["check".into(), "--file".into(), path.display().to_string()]).unwrap_err();
    assert!(err.contains("parse line 1"));
}

// ---- aux_cmd ----

#[test]
fn aux_cmd_show_default_unconfigured() {
    let v = aux_cmd(&["show".into()]).expect("show ok");
    // Default config has no auxiliary_provider set. Configured
    // SHOULD be false (no aux). build_error null.
    assert!(v.get("configured").is_some());
    assert!(v.get("provider").is_some()); // null is fine
    assert!(v.get("model").is_some());
    assert!(v.get("max_tokens").is_some());
    assert!(v.get("note").and_then(|n| n.as_str()).is_some());
}

#[test]
fn aux_cmd_default_subcommand_is_show() {
    let v = aux_cmd(&[]).expect("default ok");
    assert!(v.get("max_tokens").is_some());
}

#[test]
fn aux_cmd_ask_requires_prompt() {
    let err = aux_cmd(&["ask".into()]).unwrap_err();
    assert!(err.contains("--prompt"));
}

#[test]
fn aux_cmd_ask_when_unconfigured_errs() {
    // Default config has no aux, so ask MUST refuse before doing
    // any network IO.
    let err = aux_cmd(&["ask".into(), "--prompt".into(), "hello".into()]).unwrap_err();
    assert!(err.contains("auxiliary"));
}

#[test]
fn aux_cmd_max_tokens_invalid_errs() {
    let err = aux_cmd(&[
        "ask".into(),
        "--prompt".into(),
        "hi".into(),
        "--max-tokens".into(),
        "lots".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--max-tokens"));
}

// ---- retry_cmd ----

#[test]
fn retry_cmd_show_default_disabled() {
    // Default config has retry_enabled = false.
    let v = retry_cmd(&["show".into()]).expect("show ok");
    assert_eq!(v.get("enabled").and_then(|b| b.as_bool()), Some(false));
    assert!(v.get("config_retry_enabled").is_some());
    assert!(v.get("note").and_then(|s| s.as_str()).is_some());
}

#[test]
fn retry_cmd_default_subcommand_is_show() {
    let v = retry_cmd(&[]).expect("default ok");
    assert!(v.get("enabled").is_some());
}

#[test]
fn retry_cmd_schedule_falls_back_to_standard_when_disabled() {
    // retry_cmd schedule should still produce a preview using
    // RetryPolicy::standard() even when config has retries off.
    let v = retry_cmd(&["schedule".into()]).expect("schedule ok");
    let waits = v
        .get("inter_attempt_waits")
        .and_then(|w| w.as_array())
        .expect("array");
    // standard() = 4 attempts → 3 inter-attempt waits.
    assert_eq!(waits.len(), 3);
    assert_eq!(v.get("max_attempts").and_then(|n| n.as_u64()), Some(4));
}

#[test]
fn retry_cmd_schedule_attempts_override() {
    let v = retry_cmd(&["schedule".into(), "--attempts".into(), "6".into()]).expect("schedule ok");
    let waits = v
        .get("inter_attempt_waits")
        .and_then(|w| w.as_array())
        .expect("array");
    assert_eq!(waits.len(), 5);
    assert_eq!(v.get("max_attempts").and_then(|n| n.as_u64()), Some(6));
}

#[test]
fn retry_cmd_schedule_one_attempt_has_no_waits() {
    let v = retry_cmd(&["schedule".into(), "--attempts".into(), "1".into()]).expect("schedule ok");
    let waits = v
        .get("inter_attempt_waits")
        .and_then(|w| w.as_array())
        .expect("array");
    assert!(waits.is_empty());
    assert_eq!(v.get("total_observed_ms").and_then(|n| n.as_u64()), Some(0));
}

#[test]
fn retry_cmd_schedule_caps_delay_at_max_ms() {
    // standard() base=500, max=8000. delay_for(4) would naively
    // be 500 << 3 = 4000 (≤ max), delay_for(5) = 500 << 4 = 8000
    // (= max), delay_for(10) > max → capped.
    let v = retry_cmd(&["schedule".into(), "--attempts".into(), "11".into()]).expect("schedule ok");
    let waits = v
        .get("inter_attempt_waits")
        .and_then(|w| w.as_array())
        .expect("array");
    // Find attempt 10 → cap_ms must be exactly max_ms (8000).
    let last = &waits[waits.len() - 1];
    assert_eq!(last.get("cap_ms").and_then(|n| n.as_u64()), Some(8000));
}

#[test]
fn retry_cmd_schedule_invalid_attempts_errs() {
    let err = retry_cmd(&["schedule".into(), "--attempts".into(), "lots".into()]).unwrap_err();
    assert!(err.contains("--attempts"));
}
