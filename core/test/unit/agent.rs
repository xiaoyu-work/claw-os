// Reads the session's history from the memory DB, infers tool
// usage from the stored `[tool_use:NAME] ...` markers (no schema
// migration required), and runs the deterministic
// [`crate::agent::curator::Curator`] pure-function pipeline.
//
// Output is a JSON object with either a `draft` (id/title/desc/
// allowed_tools/confidence) or a `not_enough` reason.

use super::*;

#[test]
fn interactive_chat_hides_evidence_warnings() {
    use crate::agent::runtime::evidence::EvidenceStatus;

    assert!(!should_render_evidence_warning(
        true,
        &EvidenceStatus::Missing
    ));
    assert!(should_render_evidence_warning(
        false,
        &EvidenceStatus::Missing
    ));
    assert!(!should_render_evidence_warning(
        false,
        &EvidenceStatus::Verified
    ));
}

#[test]
fn terminal_tool_line_has_no_surrounding_blank_lines() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_line(&mut out, "[tool: cos_sysinfo]");
    state.finish_line(&mut out);
    state.write_text(&mut out, "Ubuntu 26.04");
    state.finish_line(&mut out);

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "[tool: cos_sysinfo]\nUbuntu 26.04\n"
    );
}

#[test]
fn terminal_tool_line_separates_unfinished_text_once() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_text(&mut out, "Let me check");
    state.write_line(&mut out, "[tool: cos_sysinfo]");

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "Let me check\n[tool: cos_sysinfo]\n"
    );
}

#[test]
fn terminal_heartbeat_line_finishes_before_tool_failure() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_line(&mut out, "[tool: cos_sysinfo]");
    state.write_text(&mut out, "...");
    state.finish_line(&mut out);
    state.write_line(&mut out, "[tool failed: cos_sysinfo]");

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "[tool: cos_sysinfo]\n...\n[tool failed: cos_sysinfo]\n"
    );
}

#[test]
fn terminal_heartbeat_line_finishes_before_next_prompt() {
    let mut state = TerminalOutputState::default();
    let mut out = Vec::new();

    state.write_line(&mut out, "[tool: cos_sysinfo]");
    state.write_text(&mut out, "..");
    state.finish_line(&mut out);
    out.extend_from_slice(b"you> ");

    assert_eq!(
        String::from_utf8(out).unwrap(),
        "[tool: cos_sysinfo]\n..\nyou> "
    );
}

#[test]
fn override_cmd_help_shape() {
    let err = override_cmd(&[]).unwrap_err();
    assert!(err.contains("show"));
    assert!(err.contains("path"));
    assert!(err.contains("effective"));
}

#[test]
fn override_cmd_path_returns_user_config_path() {
    let v = override_cmd(&["path".to_string(), "demo-app".to_string()]).expect("path ok");
    let p = v.get("path").and_then(|x| x.as_str()).expect("path field");
    assert!(p.contains("apps"));
    assert!(p.ends_with("demo-app.json"));
}

#[test]
fn override_cmd_show_missing_file_reports_absent() {
    // Mutates process-wide env; serialize with the crate-wide
    // env lock so we don't race with other env-touching tests.
    let _env_lock = crate::test_env::lock_env();
    // Point user-config at an empty tmp dir so the file definitely doesn't exist.
    let tmp = std::env::temp_dir().join(format!("cos-override-cmd-missing-{}", std::process::id()));
    let prev = std::env::var_os("COS_USER_CONFIG_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &tmp);
    let v = override_cmd(&["show".to_string(), "never-installed".to_string()]).expect("show ok");
    match prev {
        Some(p) => std::env::set_var("COS_USER_CONFIG_DIR", p),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    assert_eq!(v.get("present").and_then(|x| x.as_bool()), Some(false));
    assert!(v.get("override").is_some_and(|x| x.is_null()));
}

#[test]
fn budget_user_path_returns_ai_budget_path() {
    let v = budget_cmd(&["user".to_string(), "path".to_string()]).expect("path ok");
    let p = v.get("path").and_then(|x| x.as_str()).expect("path field");
    assert!(p.contains("ai"));
    assert!(p.ends_with("budget.json"));
    assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("user"));
}

#[test]
fn budget_user_show_missing_file_reports_unlimited() {
    // Mutates process-wide env; serialize with the crate-wide
    // env lock so we don't race with other env-touching tests.
    let _env_lock = crate::test_env::lock_env();
    // Empty tmp dirs ⇒ no budget.json (unlimited) and a writable
    // data dir for the SQLite store (the default /var/lib/cos is
    // not writable on dev hosts).
    let tmp = std::env::temp_dir().join(format!("cos-budget-user-show-{}", std::process::id()));
    let cfg_dir = tmp.join("config");
    let data_dir = tmp.join("data");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&data_dir).unwrap();
    let prev_cfg = std::env::var_os("COS_USER_CONFIG_DIR");
    let prev_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_USER_CONFIG_DIR", &cfg_dir);
    std::env::set_var("COS_DATA_DIR", &data_dir);
    let v = budget_cmd(&["user".to_string(), "show".to_string()]).expect("show ok");
    match prev_cfg {
        Some(p) => std::env::set_var("COS_USER_CONFIG_DIR", p),
        None => std::env::remove_var("COS_USER_CONFIG_DIR"),
    }
    match prev_data {
        Some(p) => std::env::set_var("COS_DATA_DIR", p),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("user"));
    assert_eq!(v.get("unlimited").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(v.get("units_cap").and_then(|x| x.as_u64()), Some(0));
    assert!(v.get("units_available").is_some_and(|x| x.is_null()));
}

#[test]
fn insights_overall_returns_empty_when_no_log() {
    // The default log path may or may not exist at test time;
    // either way the call must not panic and must shape a JSON
    // object with the expected fields.
    let v = insights_cmd(&[]).expect("insights ok");
    assert!(v.get("overall").is_some());
    assert!(v.get("per_provider").is_some());
    assert!(v.get("per_model").is_some());
    assert!(v.get("log").is_some());
}

#[test]
fn insights_recent_parses_n_arg() {
    let v = insights_cmd(&["recent".into(), "5".into()]).expect("recent ok");
    assert!(v.get("records").is_some());
    // n is the actual returned count, not the requested limit; on a
    // fresh test env it should be zero records but the field must
    // still exist.
    let n = v.get("n").and_then(|x| x.as_u64()).expect("n field");
    assert!(n <= 5);
}

#[test]
fn insights_sessions_returns_map() {
    let v = insights_cmd(&["sessions".into()]).expect("sessions ok");
    assert!(v.get("sessions").is_some());
}

#[test]
fn recall_empty_query_errors() {
    let err = recall_cmd(&[]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn notes_list_returns_dir_and_names() {
    let v = notes_cmd(&[]).expect("notes list ok");
    assert!(v.get("dir").is_some());
    assert!(v.get("notes").and_then(|x| x.as_array()).is_some());
}

#[test]
fn skills_root_returns_path() {
    let v = skills_cmd(&["root".into()]).expect("skills root ok");
    assert!(v.get("root").and_then(|x| x.as_str()).is_some());
    assert!(v.get("user_root").and_then(|x| x.as_str()).is_some());
    assert!(v.get("system_root").and_then(|x| x.as_str()).is_some());
}

#[test]
fn skills_list_shape_correct() {
    let v = skills_cmd(&[]).expect("skills list ok");
    assert!(v.get("loaded").is_some());
    assert!(v.get("disabled").is_some());
    assert!(v.get("errors").is_some());
    assert!(v.get("names").and_then(|x| x.as_array()).is_some());
    assert!(v.get("user_root").and_then(|x| x.as_str()).is_some());
    assert!(v.get("system_root").and_then(|x| x.as_str()).is_some());
}

#[test]
fn skills_info_unknown_id_errors() {
    let err = skills_cmd(&["info".into(), "definitely-not-a-real-skill".into()]).unwrap_err();
    assert!(err.contains("definitely-not-a-real-skill"));
}

#[test]
fn parse_owner_repo_accepts_valid_form() {
    let (o, r) = parse_owner_repo("clawos/skills-hub").unwrap();
    assert_eq!(o, "clawos");
    assert_eq!(r, "skills-hub");
}

#[test]
fn parse_owner_repo_trims_whitespace() {
    let (o, r) = parse_owner_repo(" foo / bar ").unwrap();
    assert_eq!(o, "foo");
    assert_eq!(r, "bar");
}

#[test]
fn parse_owner_repo_rejects_missing_slash() {
    let err = parse_owner_repo("noslashhere").unwrap_err();
    assert!(err.contains("owner"));
}

#[test]
fn parse_owner_repo_rejects_empty_segments() {
    assert!(parse_owner_repo("/repo").is_err());
    assert!(parse_owner_repo("owner/").is_err());
    assert!(parse_owner_repo("/").is_err());
    assert!(parse_owner_repo("").is_err());
}

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

#[test]
fn redact_replaces_known_secrets() {
    let v = redact_cmd(&["my key is sk-abcdef0123456789ABCDEF0123456789abcd ok".into()])
        .expect("redact ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("[REDACTED:"),
        "expected placeholder, got {out}"
    );
    assert!(!out.contains("sk-abcdef0123456789ABCDEF0123456789abcd"));
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn redact_unchanged_for_clean_input() {
    let v = redact_cmd(&["hello world, this is just text".into()]).expect("redact ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert_eq!(out, "hello world, this is just text");
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(false));
}

#[test]
fn redact_check_returns_detection_only() {
    let v = redact_cmd(&["--check".into(), "leaks AKIAIOSFODNN7EXAMPLE here".into()])
        .expect("check ok");
    assert_eq!(
        v.get("contains_secrets").and_then(|x| x.as_bool()),
        Some(true)
    );
    assert!(
        v.get("redacted").is_none(),
        "check mode should not include redacted"
    );
}

#[test]
fn redact_check_negative() {
    let v = redact_cmd(&["--check".into(), "innocent text".into()]).expect("check ok");
    assert_eq!(
        v.get("contains_secrets").and_then(|x| x.as_bool()),
        Some(false)
    );
}

#[test]
fn redact_strict_flag_propagates() {
    let v = redact_cmd(&["--strict".into(), "contact me at user@example.com".into()])
        .expect("strict redact");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("[REDACTED:email]"),
        "strict should redact emails: {out}"
    );
    assert_eq!(v.get("strict").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn redact_default_does_not_redact_email() {
    let v = redact_cmd(&["contact me at user@example.com".into()]).expect("default redact");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("user@example.com"),
        "default should keep email: {out}"
    );
}

#[test]
fn redact_from_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("sample.txt");
    std::fs::write(&p, "token=ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789").expect("write");
    let v =
        redact_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("file redact ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(
        out.contains("[REDACTED:github_token]"),
        "expected github_token placeholder, got {out}"
    );
}

#[test]
fn redact_joins_multiple_positional_args() {
    // Without --, the dispatcher will tokenize on spaces; we
    // re-stitch them so `cos agent redact hello world` doesn't
    // error out.
    let v = redact_cmd(&[
        "hello".into(),
        "this".into(),
        "has".into(),
        "Bearer".into(),
        "abcdefABCDEF1234567890123456789012345678".into(),
    ])
    .expect("multi-arg ok");
    let out = v.get("redacted").and_then(|x| x.as_str()).unwrap();
    assert!(out.contains("[REDACTED:"), "got {out}");
}

#[test]
fn skills_usage_stats_empty_returns_zero_count() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let v = skills_usage_cmd_at(&["stats".into()], &p).expect("stats ok");
    assert_eq!(v.get("skill_count").and_then(|x| x.as_u64()), Some(0));
    assert_eq!(
        v.get("skills").and_then(|x| x.as_array()).map(|a| a.len()),
        Some(0)
    );
}

#[test]
fn skills_usage_record_then_stats_aggregates() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "100".into(),
            "--ok".into(),
        ],
        &p,
    )
    .expect("record 1");
    skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "200".into(),
            "--error".into(),
        ],
        &p,
    )
    .expect("record 2");
    let v = skills_usage_cmd_at(&["stats".into()], &p).expect("stats ok");
    let skills = v.get("skills").and_then(|x| x.as_array()).unwrap();
    assert_eq!(skills.len(), 1);
    let s = &skills[0];
    assert_eq!(s.get("id").and_then(|x| x.as_str()), Some("demo"));
    assert_eq!(s.get("total").and_then(|x| x.as_u64()), Some(2));
    assert_eq!(s.get("success").and_then(|x| x.as_u64()), Some(1));
    assert_eq!(s.get("failure").and_then(|x| x.as_u64()), Some(1));
    assert_eq!(
        s.get("total_duration_ms").and_then(|x| x.as_u64()),
        Some(300)
    );
    assert_eq!(
        s.get("average_duration_ms").and_then(|x| x.as_u64()),
        Some(150)
    );
}

#[test]
fn skills_usage_stats_filter_by_id() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    for id in ["a", "b", "c"] {
        skills_usage_cmd_at(
            &[
                "record".into(),
                id.into(),
                "--duration-ms".into(),
                "10".into(),
            ],
            &p,
        )
        .expect("rec");
    }
    let v = skills_usage_cmd_at(&["stats".into(), "b".into()], &p).expect("stats ok");
    let skills = v.get("skills").and_then(|x| x.as_array()).unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(
        skills[0].get("id").and_then(|x| x.as_str()),
        Some("b"),
        "filter should keep only `b`"
    );
    assert_eq!(v.get("filter_id").and_then(|x| x.as_str()), Some("b"));
}

#[test]
fn skills_usage_record_requires_duration() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let err = skills_usage_cmd_at(&["record".into(), "demo".into()], &p).unwrap_err();
    assert!(err.contains("--duration-ms"));
}

#[test]
fn skills_usage_record_with_invoked_by_persists() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "5".into(),
            "--by".into(),
            "delegate".into(),
        ],
        &p,
    )
    .expect("record ok");
    let body = std::fs::read_to_string(&p).expect("read");
    assert!(body.contains("\"invoked_by\":\"delegate\""), "body: {body}");
}

#[test]
fn skills_usage_clear_refuses_without_yes() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    std::fs::write(&p, "junk").expect("write");
    let err = skills_usage_cmd_at(&["clear".into()], &p).unwrap_err();
    assert!(err.contains("--yes"));
    assert!(p.exists(), "file must remain after refused clear");
}

#[test]
fn skills_usage_clear_with_yes_removes_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    std::fs::write(&p, "junk").expect("write");
    let v = skills_usage_cmd_at(&["clear".into(), "--yes".into()], &p).expect("clear ok");
    assert_eq!(v.get("cleared").and_then(|x| x.as_bool()), Some(true));
    assert!(!p.exists(), "file should be removed");
}

#[test]
fn skills_usage_clear_missing_file_is_ok() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("does-not-exist.jsonl");
    let v = skills_usage_cmd_at(&["clear".into(), "--yes".into()], &p).expect("clear ok");
    assert_eq!(v.get("cleared").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn skills_usage_path_returns_path() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let v = skills_usage_cmd_at(&["path".into()], &p).expect("path ok");
    let returned = v.get("path").and_then(|x| x.as_str()).unwrap();
    assert!(returned.ends_with("usage.jsonl"), "got {returned}");
}

#[test]
fn skills_usage_record_unknown_flag_errors() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("usage.jsonl");
    let err = skills_usage_cmd_at(
        &[
            "record".into(),
            "demo".into(),
            "--duration-ms".into(),
            "1".into(),
            "--bogus".into(),
        ],
        &p,
    )
    .unwrap_err();
    assert!(err.contains("--bogus"));
}

#[test]
fn prompt_show_returns_prompt_string() {
    let v = prompt_cmd(&[]).expect("prompt show ok");
    let p = v
        .get("prompt")
        .and_then(|x| x.as_str())
        .expect("prompt str");
    assert!(!p.is_empty());
    let chars = v.get("chars").and_then(|x| x.as_u64()).expect("chars");
    assert!(chars > 0);
}

#[test]
fn prompt_show_default_includes_size_breakdown() {
    let v = prompt_cmd(&["show".into()]).expect("show ok");
    assert!(v.get("scaffold_chars").is_some());
    assert!(v.get("approx_tokens").is_some());
    assert!(v.get("extra_path").is_some()); // null when not provided
    assert_eq!(v["scope"], "new-session-candidate");
    assert_eq!(
        v["prompt_version"],
        crate::agent::prompt::CANONICAL_PROMPT_VERSION
    );
    assert!(v.get("turn_context_sources").is_some());
}

#[test]
fn prompt_raw_omits_size_breakdown() {
    let v = prompt_cmd(&["show".into(), "--raw".into()]).expect("raw ok");
    assert!(v.get("prompt").is_some());
    assert!(v.get("scaffold_chars").is_none());
    assert!(v.get("extra_path").is_none());
    assert!(v.get("turn_context").is_some());
}

#[test]
fn prompt_extra_appends_file_content() {
    let dir = tempfile::tempdir().expect("tmp");
    let extra = dir.path().join("preface.md");
    std::fs::write(&extra, "ZZZUNIQUEMARKERZZZ_extra_preface_text").expect("write");
    let baseline = prompt_cmd(&["show".into()]).expect("baseline");
    let with_extra = prompt_cmd(&[
        "show".into(),
        "--extra".into(),
        extra.to_string_lossy().to_string(),
    ])
    .expect("with extra");
    let baseline_chars = baseline.get("chars").and_then(|x| x.as_u64()).unwrap();
    let extra_chars = with_extra.get("chars").and_then(|x| x.as_u64()).unwrap();
    assert!(extra_chars > baseline_chars, "extra should grow prompt");
    let p = with_extra.get("prompt").and_then(|x| x.as_str()).unwrap();
    assert!(
        p.contains("ZZZUNIQUEMARKERZZZ_extra_preface_text"),
        "extra content must be in prompt"
    );
    assert_eq!(
        with_extra.get("extra_path").and_then(|x| x.as_str()),
        Some(extra.to_string_lossy().as_ref())
    );
}

#[test]
fn prompt_build_alias_works() {
    let v = prompt_cmd(&["build".into()]).expect("build ok");
    assert!(v.get("prompt").is_some());
}

#[test]
fn prompt_extra_nonexistent_file_does_not_panic() {
    // build_system_prompt silently swallows file IO errors and
    // falls back to scaffold-only — preserve that here.
    let v = prompt_cmd(&[
        "show".into(),
        "--extra".into(),
        "Z:\\definitely\\not\\a\\real\\path".into(),
    ])
    .expect("ok");
    assert!(v.get("prompt").and_then(|x| x.as_str()).is_some());
}

#[test]
fn think_scrub_strips_think_block() {
    let v = think_scrub_cmd(&["before <think>secret reasoning</think> after".into()]).expect("ok");
    let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
    assert!(!out.contains("secret reasoning"), "got {out}");
    assert!(out.contains("before"));
    assert!(out.contains("after"));
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(true));
}

#[test]
fn think_scrub_unchanged_for_clean_input() {
    let v = think_scrub_cmd(&["just plain text".into()]).expect("ok");
    assert_eq!(v.get("changed").and_then(|x| x.as_bool()), Some(false));
}

#[test]
fn think_scrub_check_returns_detection_only() {
    let v = think_scrub_cmd(&[
        "--check".into(),
        "<thinking>internal</thinking> answer".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("has_thinking").and_then(|x| x.as_bool()), Some(true));
    assert!(v.get("scrubbed").is_none());
}

#[test]
fn think_scrub_check_negative() {
    let v = think_scrub_cmd(&["--check".into(), "no tags here".into()]).expect("ok");
    assert_eq!(v.get("has_thinking").and_then(|x| x.as_bool()), Some(false));
}

#[test]
fn think_scrub_handles_multiline_block() {
    let v = think_scrub_cmd(&["<thinking>\nline one\nline two\n</thinking>\nfinal".into()])
        .expect("ok");
    let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
    assert!(!out.contains("line one"), "got {out}");
    assert!(out.contains("final"));
}

#[test]
fn think_scrub_from_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("trace.txt");
    std::fs::write(&p, "<reasoning>internal</reasoning>\nthe answer is 42").expect("write");
    let v = think_scrub_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("ok");
    let out = v.get("scrubbed").and_then(|x| x.as_str()).unwrap();
    assert!(!out.contains("internal"), "got {out}");
    assert!(out.contains("the answer is 42"));
}

#[test]
fn tokens_basic_input() {
    // chars / 4 with a min of 1 — see estimate_text_tokens.
    let v = tokens_cmd(&["hello world this is some text".into()]).expect("ok");
    let chars = v.get("chars").and_then(|x| x.as_u64()).unwrap();
    let tokens = v.get("approx_tokens").and_then(|x| x.as_u64()).unwrap();
    assert_eq!(chars, "hello world this is some text".len() as u64);
    assert!(tokens >= 1);
    assert!(tokens <= chars, "tokens should be <= chars");
}

#[test]
fn tokens_from_file() {
    let dir = tempfile::tempdir().expect("tmp");
    let p = dir.path().join("body.txt");
    let content = "x".repeat(400);
    std::fs::write(&p, &content).expect("write");
    let v = tokens_cmd(&["--file".into(), p.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("chars").and_then(|x| x.as_u64()), Some(400));
    // chars / 4 = 100
    assert_eq!(v.get("approx_tokens").and_then(|x| x.as_u64()), Some(100));
}

#[test]
fn tokens_includes_method_label() {
    let v = tokens_cmd(&["abc".into()]).expect("ok");
    let m = v.get("method").and_then(|x| x.as_str()).unwrap();
    assert!(m.contains("chars"), "got {m}");
}

#[test]
fn read_text_input_joins_positional_with_spaces() {
    let (s, _) = read_text_input(&["a".into(), "b".into(), "c".into()], "tokens").expect("ok");
    assert_eq!(s, "a b c");
}

#[test]
fn nudge_path_returns_string() {
    let v = nudge_cmd(&["path".into()]).expect("nudge path ok");
    assert!(v.get("path").and_then(|x| x.as_str()).is_some());
}

#[test]
fn nudge_list_shape_correct() {
    let v = nudge_cmd(&[]).expect("nudge list ok");
    assert!(v.get("path").is_some());
    assert!(v.get("n").is_some());
    assert!(v.get("nudges").and_then(|x| x.as_array()).is_some());
}

#[test]
fn nudge_add_rejects_non_integer_due() {
    let err = nudge_cmd(&["add".into(), "not-a-number".into(), "msg".into()]).unwrap_err();
    assert!(err.contains("integer"));
}

#[test]
fn mcp_status_returns_catalogue() {
    let v = mcp_cmd(&["status".into()]).expect("mcp status ok");
    assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ready"));
    assert_eq!(v.get("transport").and_then(|x| x.as_str()), Some("stdio"));
    assert!(v.get("tools_registered").is_some());
    assert!(v.get("tools_permitted").is_some());
    assert!(v.get("tools").and_then(|x| x.as_array()).is_some());
}

#[test]
fn mcp_default_returns_status() {
    let v = mcp_cmd(&[]).expect("mcp default = status");
    assert_eq!(v.get("status").and_then(|x| x.as_str()), Some("ready"));
}

#[test]
fn mcp_status_includes_external_servers_section() {
    let v = mcp_cmd(&["status".into()]).expect("mcp status ok");
    // Always present even when no external servers are configured.
    assert!(v.get("external_servers_configured").is_some());
    assert!(v.get("external_servers_enabled").is_some());
    assert!(
        v.get("external_servers")
            .and_then(|x| x.as_array())
            .is_some(),
        "external_servers must be a JSON array (possibly empty)"
    );
}

#[test]
fn mcp_servers_without_probe_does_not_spawn_anything() {
    // Default test config has no mcp_servers, so this is a pure
    // shape assertion. It's still useful because a regression
    // that turned off the !probe early-return would either spawn
    // nothing (passes) or panic on attach (we'd see the failure).
    let v = mcp_cmd(&["servers".into()]).expect("mcp servers ok");
    assert_eq!(v.get("ok").and_then(|x| x.as_bool()), Some(true));
    assert_eq!(v.get("probed").and_then(|x| x.as_bool()), Some(false));
    assert!(v.get("servers").and_then(|x| x.as_array()).is_some());
}

#[test]
fn usage_overall_returns_summary_shape() {
    let v = usage_cmd(&[]).expect("usage default = overall");
    assert!(v.get("log").is_some());
    assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("overall"));
    assert!(v.get("total").is_some());
    assert!(v.get("by_provider").is_some());
    assert!(v.get("by_model").is_some());
    assert!(v.get("by_session").is_some());
    assert!(v.get("by_app").is_some());
    assert!(v.get("by_verb").is_some());
}

#[test]
fn usage_since_rejects_non_iso_timestamp() {
    let err = usage_cmd(&["overall".into(), "--since".into(), "not-iso".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("since"));
}

#[test]
fn usage_provider_filter_records_in_response() {
    let v = usage_cmd(&["provider".into(), "anthropic".into()]).expect("usage provider ok");
    assert_eq!(
        v.get("filter")
            .and_then(|f| f.get("provider"))
            .and_then(|x| x.as_str()),
        Some("anthropic")
    );
}

#[test]
fn usage_app_scope_records_app_filter() {
    let v = usage_cmd(&["app".into(), "summarize".into()]).expect("usage app ok");
    assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("app"));
    assert_eq!(
        v.get("filter")
            .and_then(|f| f.get("app_id"))
            .and_then(|x| x.as_str()),
        Some("summarize")
    );
}

#[test]
fn usage_verb_scope_records_verb_filter() {
    let v = usage_cmd(&["verb".into(), "ai.image.generate".into()]).expect("usage verb ok");
    assert_eq!(v.get("scope").and_then(|x| x.as_str()), Some("verb"));
    assert_eq!(
        v.get("filter")
            .and_then(|f| f.get("verb"))
            .and_then(|x| x.as_str()),
        Some("ai.image.generate")
    );
}

#[test]
fn usage_app_flag_combines_with_provider_scope() {
    let v = usage_cmd(&[
        "provider".into(),
        "anthropic".into(),
        "--app".into(),
        "summarize".into(),
    ])
    .expect("usage provider --app ok");
    let filter = v.get("filter").unwrap();
    assert_eq!(
        filter.get("provider").and_then(|x| x.as_str()),
        Some("anthropic")
    );
    assert_eq!(
        filter.get("app_id").and_then(|x| x.as_str()),
        Some("summarize")
    );
}

#[test]
fn merge_mcp_overrides_preserves_base_and_denies_attended_tools() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_allow = Some(vec!["echo".into()]);
    base.tool_deny = vec!["cos_sandbox".into()];
    let merged = merge_mcp_overrides(&base, None, Vec::new());
    assert_eq!(merged.tool_allow, base.tool_allow);
    assert_eq!(
        merged.tool_deny,
        vec![
            "cos_sandbox".to_string(),
            "cos_oauth_login".to_string()
        ]
    );
}

#[test]
fn merge_mcp_overrides_allow_replaces_base_allow() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_allow = Some(vec!["echo".into()]);
    let merged = merge_mcp_overrides(&base, Some(vec!["now".into()]), Vec::new());
    assert_eq!(merged.tool_allow, Some(vec!["now".into()]));
}

#[test]
fn merge_mcp_overrides_deny_appends_to_base() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_deny = vec!["cos_sandbox".into()];
    let merged = merge_mcp_overrides(&base, None, vec!["cos_proc".into()]);
    assert_eq!(
        merged.tool_deny,
        vec![
            "cos_sandbox".to_string(),
            "cos_proc".to_string(),
            "cos_oauth_login".to_string()
        ]
    );
}

#[test]
fn merge_mcp_overrides_cannot_allow_attended_oauth_tool() {
    let base = crate::config::AgentConfig::default();
    let merged = merge_mcp_overrides(
        &base,
        Some(vec!["cos_oauth_login".into()]),
        Vec::new(),
    );

    assert_eq!(
        merged.tool_allow,
        Some(vec!["cos_oauth_login".to_string()])
    );
    assert!(merged
        .tool_deny
        .iter()
        .any(|name| name == "cos_oauth_login"));
}

#[test]
fn merge_mcp_overrides_does_not_mutate_base() {
    let mut base = crate::config::AgentConfig::default();
    base.tool_allow = Some(vec!["a".into()]);
    let _ = merge_mcp_overrides(&base, Some(vec!["b".into()]), vec!["c".into()]);
    // Base unchanged.
    assert_eq!(base.tool_allow, Some(vec!["a".into()]));
    assert!(base.tool_deny.is_empty());
}

#[test]
fn curator_propose_requires_session_id() {
    let err = curator_cmd(&["propose".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_propose_rejects_flag_as_session_id() {
    // `propose --accept` without a session id must error rather
    // than silently treating "--accept" as the session id.
    let err = curator_cmd(&["propose".into(), "--accept".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_author_requires_draft_id() {
    let err = curator_cmd(&["author".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_author_rejects_flag_as_id() {
    let err = curator_cmd(&["author".into(), "--write".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"));
}

#[test]
fn curator_author_missing_draft_returns_helpful_error() {
    // The default DraftStore should open successfully (or fail
    // with an IO error); either way, asking for an unknown id
    // must return a string mentioning the missing id.
    let result = curator_cmd(&["author".into(), "definitely-not-real".into()]);
    let err = result.unwrap_err();
    assert!(
        err.contains("definitely-not-real") || err.contains("draft store"),
        "want missing-id or draft-store error, got: {err}"
    );
}

#[test]
fn curator_scan_returns_envelope_when_db_available() {
    // The scan command may succeed (returning an envelope with
    // zero scanned sessions) or fail with a "memory db
    // unavailable" error depending on test environment. Both
    // are acceptable; what matters is no panic and a recognised
    // outcome shape.
    match curator_cmd(&["scan".into(), "--limit".into(), "1".into()]) {
        Ok(v) => {
            assert!(v.get("scanned").is_some(), "envelope missing 'scanned'");
            assert!(v.get("results").is_some(), "envelope missing 'results'");
            assert!(v.get("drafted").is_some(), "envelope missing 'drafted'");
        }
        Err(e) => {
            assert!(
                e.contains("memory db") || e.contains("draft store"),
                "unexpected scan error: {e}"
            );
        }
    }
}

#[test]
fn curator_drafts_auto_title_rejects_invalid_seed() {
    let err = curator_drafts_cmd(&[
        "auto-title".into(),
        "some-id".into(),
        "--seed".into(),
        "bogus".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--seed"));
}
#[test]
fn providers_cmd_lists_every_registered_provider() {
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v
        .get("providers")
        .and_then(|p| p.as_array())
        .expect("providers array");
    let names: std::collections::HashSet<_> = arr
        .iter()
        .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
        .collect();
    for &expected in llm::available_providers().iter() {
        assert!(
            names.contains(expected),
            "providers_cmd missing {expected}: got {names:?}"
        );
    }
    assert_eq!(
        v.get("count").and_then(|c| c.as_u64()).unwrap_or(0),
        llm::available_providers().len() as u64
    );
}

#[test]
fn providers_cmd_marks_active_provider() {
    let active = crate::config::get().agent.provider.clone();
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    let active_entries: Vec<_> = arr
        .iter()
        .filter(|e| e.get("active") == Some(&serde_json::Value::Bool(true)))
        .collect();
    if active.is_empty() {
        // Fresh-install default: no provider configured, so no entry
        // is marked active. The CLI output is the source of truth
        // here — it reports `active: ""` and zero active entries.
        assert_eq!(
            active_entries.len(),
            0,
            "no entry should be active when provider is unconfigured"
        );
        assert_eq!(
            v.get("active").and_then(|a| a.as_str()),
            Some(""),
            "active field should be the empty string"
        );
    } else {
        assert_eq!(active_entries.len(), 1, "exactly one active provider");
        assert_eq!(
            active_entries[0].get("name").and_then(|n| n.as_str()),
            Some(active.as_str())
        );
    }
}

#[test]
fn providers_cmd_filters_by_names_flag() {
    let v = providers_cmd(&["--names".into(), "openai,anthropic".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 2);
    let names: Vec<_> = arr
        .iter()
        .filter_map(|p| p.get("name").and_then(|n| n.as_str()))
        .collect();
    assert!(names.contains(&"openai"));
    assert!(names.contains(&"anthropic"));
}

#[test]
fn providers_cmd_filter_drops_unknown_names_silently() {
    let v =
        providers_cmd(&["--names".into(), "openai,does-not-exist".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    // "does-not-exist" is not in REGISTERED, so it gets dropped.
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0].get("name").and_then(|n| n.as_str()), Some("openai"));
}

#[test]
fn providers_cmd_local_providers_have_no_canonical_env_or_credential() {
    let v =
        providers_cmd(&["--names".into(), "ollama,mock,llama_local".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 3);
    for entry in arr {
        assert_eq!(
            entry.get("env"),
            Some(&serde_json::Value::Null),
            "{:?} should have no canonical env",
            entry.get("name")
        );
        assert_eq!(
            entry.get("credential"),
            Some(&serde_json::Value::Null),
            "{:?} should have no canonical credential",
            entry.get("name")
        );
        assert_eq!(
            entry.get("key_required"),
            Some(&serde_json::Value::Bool(false)),
            "{:?} should not require a key",
            entry.get("name")
        );
    }
}

#[test]
fn providers_cmd_cloud_providers_advertise_canonical_env_and_credential() {
    let v = providers_cmd(&[
        "--names".into(),
        "openai,anthropic,gemini,xai,deepseek,openrouter".into(),
    ])
    .expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 6);
    let by_name: std::collections::HashMap<_, _> = arr
        .iter()
        .map(|e| {
            (
                e.get("name").and_then(|n| n.as_str()).unwrap().to_string(),
                e.clone(),
            )
        })
        .collect();
    assert_eq!(
        by_name["openai"].get("env").and_then(|e| e.as_str()),
        Some("OPENAI_API_KEY")
    );
    assert_eq!(
        by_name["anthropic"].get("env").and_then(|e| e.as_str()),
        Some("ANTHROPIC_API_KEY")
    );
    assert_eq!(
        by_name["gemini"].get("env").and_then(|e| e.as_str()),
        Some("GEMINI_API_KEY")
    );
    assert_eq!(
        by_name["openrouter"]
            .get("credential")
            .and_then(|e| e.as_str()),
        Some("openrouter")
    );
    for n in [
        "openai",
        "anthropic",
        "gemini",
        "xai",
        "deepseek",
        "openrouter",
    ] {
        assert_eq!(
            by_name[n].get("key_required"),
            Some(&serde_json::Value::Bool(true)),
            "{n} should require a key"
        );
    }
}

#[test]
fn providers_cmd_default_base_url_per_alias() {
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    let by_name: std::collections::HashMap<_, _> = arr
        .iter()
        .map(|e| {
            (
                e.get("name").and_then(|n| n.as_str()).unwrap().to_string(),
                e.clone(),
            )
        })
        .collect();
    assert_eq!(
        by_name["openai"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://api.openai.com/v1")
    );
    assert_eq!(
        by_name["xai"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://api.x.ai/v1")
    );
    assert_eq!(
        by_name["ollama"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("http://localhost:11434/v1")
    );
    assert_eq!(
        by_name["anthropic"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://api.anthropic.com/v1")
    );
    assert_eq!(
        by_name["gemini"]
            .get("default_base_url")
            .and_then(|u| u.as_str()),
        Some("https://generativelanguage.googleapis.com/v1beta")
    );
}

#[test]
fn providers_cmd_env_present_reflects_environment() {
    // Pick an env name extremely unlikely to be set in CI to assert
    // the negative path. We can't safely set/unset OPENAI_API_KEY in
    // a process-shared test, so we just check the contract.
    let v = providers_cmd(&[]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    for entry in arr {
        let env = entry.get("env").and_then(|e| e.as_str());
        let env_present = entry
            .get("env_present")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if env.is_none() {
            assert!(
                !env_present,
                "providers without canonical env must report env_present=false"
            );
        }
    }
}

#[test]
fn providers_cmd_probe_credentials_default_off() {
    let v = providers_cmd(&[]).expect("providers ok");
    assert_eq!(
        v.get("probe_credentials"),
        Some(&serde_json::Value::Bool(false))
    );
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    for entry in arr {
        assert_eq!(
            entry.get("credential_present"),
            Some(&serde_json::Value::Bool(false)),
            "credential_present must be false when --probe-credentials is off (no false positives)"
        );
    }
}

#[test]
fn providers_cmd_probe_credentials_flag_flips_marker() {
    let v = providers_cmd(&["--probe-credentials".into()]).expect("providers ok");
    assert_eq!(
        v.get("probe_credentials"),
        Some(&serde_json::Value::Bool(true))
    );
    // We don't assert credential_present truthiness because the
    // test environment is unpredictable; just that the probe ran.
}

#[test]
fn providers_cmd_count_matches_providers_array_len() {
    let v = providers_cmd(&[]).expect("providers ok");
    let arr_len = v
        .get("providers")
        .and_then(|p| p.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
    assert_eq!(count as usize, arr_len);
}

#[test]
fn providers_cmd_filter_count_matches_filtered_array() {
    let v = providers_cmd(&["--names".into(), "openai".into()]).expect("providers ok");
    let count = v.get("count").and_then(|c| c.as_u64()).unwrap_or(0);
    assert_eq!(count, 1);
}

#[test]
fn providers_cmd_surfaces_bedrock_with_aws_access_key_env() {
    // Bedrock uses three env vars (access key, secret, optional
    // session token); we surface AWS_ACCESS_KEY_ID as the canonical
    // env, matching AWS SDK convention. credential is None
    // because Bedrock's three-name credential model doesn't fit
    // the single-name `*_credential` field.
    let v = providers_cmd(&["--names".into(), "bedrock".into()]).expect("providers ok");
    let arr = v.get("providers").and_then(|p| p.as_array()).unwrap();
    assert_eq!(arr.len(), 1);
    let entry = &arr[0];
    assert_eq!(entry.get("name").and_then(|n| n.as_str()), Some("bedrock"));
    assert_eq!(
        entry.get("env").and_then(|e| e.as_str()),
        Some("AWS_ACCESS_KEY_ID")
    );
    assert_eq!(entry.get("credential"), Some(&serde_json::Value::Null));
    assert_eq!(
        entry.get("key_required"),
        Some(&serde_json::Value::Bool(true))
    );
    let url = entry
        .get("default_base_url")
        .and_then(|u| u.as_str())
        .unwrap_or("");
    assert!(
        url.contains("bedrock-runtime") && url.contains("{region}"),
        "expected region-templated default_base_url, got {url}"
    );
}

#[test]
fn provider_build_status_surfaces_unresolved_pool_without_secret_values() {
    const LEGACY_ENV: &str = "__COS_TEST_PROVIDER_STATUS_LEGACY__";
    const MISSING_POOL_ENV: &str = "__COS_TEST_PROVIDER_STATUS_POOL_MISSING__";
    const LEGACY_VALUE: &str = "legacy-status-secret-must-not-leak";
    std::env::set_var(LEGACY_ENV, LEGACY_VALUE);
    std::env::remove_var(MISSING_POOL_ENV);
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "openai".into();
    cfg.model = "gpt-test".into();
    cfg.api_key_env = Some(LEGACY_ENV.into());
    cfg.api_key_envs = vec![MISSING_POOL_ENV.into()];

    let (configured, error) = provider_build_status("openai", "gpt-test", &cfg);
    std::env::remove_var(LEGACY_ENV);
    assert!(!configured);
    assert_eq!(error["kind"], "credential_pool");
    assert_eq!(error["provider"], "openai");
    assert_eq!(error["environment_variables"], json!([MISSING_POOL_ENV]));
    assert!(error["details"]
        .as_str()
        .is_some_and(|details| details.contains(MISSING_POOL_ENV)));
    assert!(!error.to_string().contains(LEGACY_VALUE));
}

// ---- provider-doctor ----

#[test]
fn provider_doctor_static_only_includes_doctor_section() {
    // Default invocation: no --probe-network.
    let v = provider_doctor_cmd(&[]).expect("doctor ok");
    // Inherits the providers_cmd shape.
    assert!(v.get("providers").and_then(|p| p.as_array()).is_some());
    // Doctor section present.
    let doctor = v.get("doctor").expect("doctor section");
    assert_eq!(
        doctor.get("probe_network"),
        Some(&serde_json::Value::Bool(false))
    );
    let probe = doctor.get("active_probe").expect("active_probe");
    assert_eq!(
        probe.get("attempted"),
        Some(&serde_json::Value::Bool(false))
    );
    assert!(probe.get("reason").and_then(|r| r.as_str()).is_some());
}

#[test]
fn provider_doctor_default_timeout_is_30s() {
    let v = provider_doctor_cmd(&[]).expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    assert_eq!(
        doctor.get("probe_timeout_secs").and_then(|t| t.as_u64()),
        Some(30)
    );
}

#[test]
fn provider_doctor_custom_timeout_parses() {
    let v = provider_doctor_cmd(&["--timeout".into(), "5".into()]).expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    assert_eq!(
        doctor.get("probe_timeout_secs").and_then(|t| t.as_u64()),
        Some(5)
    );
}

#[test]
fn provider_doctor_zero_timeout_rejected() {
    let err = provider_doctor_cmd(&["--timeout".into(), "0".into()]).unwrap_err();
    assert!(err.contains("--timeout"));
}

#[test]
fn provider_doctor_non_numeric_timeout_rejected() {
    let err = provider_doctor_cmd(&["--timeout".into(), "soon".into()]).unwrap_err();
    assert!(err.contains("--timeout"));
}

#[test]
fn provider_doctor_skips_probe_for_unconfigured_provider() {
    // The default test config now has provider="" (not configured).
    // Verify --probe-network is gracefully skipped without spinning a
    // tokio runtime or hitting the network.
    let v = provider_doctor_cmd(&["--probe-network".into()]).expect("doctor ok");
    let probe = v
        .get("doctor")
        .and_then(|d| d.get("active_probe"))
        .expect("probe");
    assert_eq!(
        v.get("doctor")
            .and_then(|d| d.get("active"))
            .and_then(|a| a.as_str()),
        Some("")
    );
    assert_eq!(
        probe.get("attempted"),
        Some(&serde_json::Value::Bool(false))
    );
    let reason = probe.get("reason").and_then(|r| r.as_str()).unwrap_or("");
    assert!(
        reason.contains("no LLM provider configured"),
        "expected unconfigured-skip reason, got {reason:?}"
    );
}

#[test]
fn provider_doctor_filter_excluding_active_marks_out_of_scope() {
    // Active provider is "" in test config (fresh-install default).
    // Filtering to "openai" leaves the active filter out-of-scope.
    // The probe-skip reason can be either "filter excluded the active
    // provider" or "no LLM configured" depending on which check fires
    // first; both are honest UX.
    let v = provider_doctor_cmd(&["--probe-network".into(), "--names".into(), "openai".into()])
        .expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    assert_eq!(
        doctor.get("active_in_scope"),
        Some(&serde_json::Value::Bool(false))
    );
    let probe = doctor.get("active_probe").unwrap();
    assert_eq!(
        probe.get("attempted"),
        Some(&serde_json::Value::Bool(false))
    );
    let reason = probe.get("reason").and_then(|r| r.as_str()).unwrap_or("");
    assert!(
        reason.contains("filtered out")
            || reason.contains("--names")
            || reason.contains("no LLM provider configured"),
        "expected filter or unconfigured reason, got {reason:?}"
    );
}

#[test]
fn provider_doctor_surfaces_effective_timeout_min_of_two() {
    // probe_timeout 9999 + provider request_timeout (default = some
    // smaller value from CosConfig) → effective is the smaller one.
    let v = provider_doctor_cmd(&["--timeout".into(), "9999".into()]).expect("doctor ok");
    let doctor = v.get("doctor").unwrap();
    let probe_t = doctor
        .get("probe_timeout_secs")
        .and_then(|t| t.as_u64())
        .unwrap();
    let provider_t = doctor
        .get("provider_request_timeout_secs")
        .and_then(|t| t.as_u64())
        .unwrap();
    let effective = doctor
        .get("effective_timeout_secs")
        .and_then(|t| t.as_u64())
        .unwrap();
    assert_eq!(probe_t, 9999);
    assert_eq!(effective, std::cmp::min(probe_t, provider_t));
}

#[test]
fn llm_error_kind_classification_is_complete() {
    // Pin the tag for every LlmError variant — adding a new variant
    // without updating the doctor classifier should fail this test.
    assert_eq!(
        llm_error_kind(&llm::LlmError::NotConfigured("x".into())),
        "not_configured"
    );
    assert_eq!(
        llm_error_kind(&llm::LlmError::InvalidRequest("x".into())),
        "invalid_request"
    );
    assert_eq!(
        llm_error_kind(&llm::LlmError::Provider {
            status: 500,
            message: "x".into(),
        }),
        "provider"
    );
    assert_eq!(
        llm_error_kind(&llm::LlmError::RateLimited { retry_after_ms: 0 }),
        "rate_limited"
    );
    assert_eq!(llm_error_kind(&llm::LlmError::Auth), "auth");
    assert_eq!(llm_error_kind(&llm::LlmError::Parse("x".into())), "parse");
    assert_eq!(llm_error_kind(&llm::LlmError::Stream("x".into())), "stream");
    assert_eq!(
        llm_error_kind(&llm::LlmError::Internal("x".into())),
        "internal"
    );
}

#[test]
fn title_cmd_returns_first_line_clamped() {
    let v = title_cmd(&["hello world".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello world"));
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
}

#[test]
fn title_cmd_strips_slash_command_verb() {
    let v = title_cmd(&["/ask hello there".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello there"));
}

#[test]
fn title_cmd_takes_first_line_only() {
    let v = title_cmd(&["one\ntwo\nthree".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("one"));
}

#[test]
fn title_cmd_empty_input_falls_back_to_untitled() {
    let v = title_cmd(&["   ".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("untitled"));
}

#[test]
fn title_cmd_requires_some_input() {
    let err = title_cmd(&[]).unwrap_err();
    assert!(err.contains("title"));
}

#[test]
fn title_cmd_llm_without_aux_errs() {
    // No auxiliary config in test env → CLI should err clearly.
    let err = title_cmd(&["hello".into(), "--llm".into()]).unwrap_err();
    assert!(err.contains("auxiliary"));
}

#[test]
fn title_cmd_llm_flag_is_consumed_not_treated_as_input() {
    // Without --llm we still get heuristic from "hello"; confirms
    // flag isn't joined into the input.
    let v = title_cmd(&["hello".into()]).expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("hello"));
}

#[test]
fn title_cmd_with_aux_none_falls_back_to_heuristic() {
    let v = title_cmd_with_aux("/help me", None).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("me"));
}

#[test]
fn title_cmd_with_aux_uses_mock_response() {
    use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::config::AgentConfig;
    let cfg = AgentConfig::default();
    let provider = MockProvider::new("title-mock", &cfg);
    provider.push_response(MockResponse::Text("Quick rust setup".into()));
    let aux = AuxiliaryClient::new(
        std::sync::Arc::new(provider),
        AuxiliaryConfig::new("mock", "title-mock"),
    );
    let v = title_cmd_with_aux("How do I install rust?", Some(&aux)).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("llm"));
    assert_eq!(
        v.get("title").and_then(|s| s.as_str()),
        Some("Quick rust setup")
    );
    assert_eq!(v.get("provider").and_then(|s| s.as_str()), Some("mock"));
    assert_eq!(v.get("model").and_then(|s| s.as_str()), Some("title-mock"));
}

#[test]
fn summarise_cmd_returns_first_sentence() {
    let v = summarise_cmd(&["First sentence. Second one.".into()]).expect("summarise ok");
    assert_eq!(
        v.get("summary").and_then(|s| s.as_str()),
        Some("First sentence.")
    );
    assert_eq!(v.get("clamped").and_then(|b| b.as_bool()), Some(false));
}

#[test]
fn summarise_cmd_clamps_to_max_with_ellipsis() {
    let v = summarise_cmd(&[
        "abcdefghij no terminator".into(),
        "--max".into(),
        "5".into(),
    ])
    .expect("summarise ok");
    let s = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
    assert_eq!(s.chars().count(), 5);
    assert!(s.ends_with('…'), "should end with ellipsis: {s:?}");
    assert_eq!(v.get("clamped").and_then(|b| b.as_bool()), Some(true));
}

#[test]
fn summarise_cmd_default_max_is_200() {
    let v = summarise_cmd(&["short input".into()]).expect("summarise ok");
    assert_eq!(v.get("max_chars").and_then(|n| n.as_u64()), Some(200));
}

#[test]
fn summarise_cmd_max_must_parse() {
    let err = summarise_cmd(&["--max".into(), "not-a-number".into(), "x".into()]).unwrap_err();
    assert!(err.contains("--max"));
}

#[test]
fn summarize_alias_dispatches_to_summarise() {
    // Confirm the US-spelling alias hits the same handler (now under `dev`).
    let v = run("dev", &["summarize".into(), "hello.".into()]).expect("summarize ok");
    assert_eq!(v.get("summary").and_then(|s| s.as_str()), Some("hello."));
}

#[test]
fn summarise_cmd_llm_without_aux_errs() {
    let err = summarise_cmd(&["hello there".into(), "--llm".into()]).unwrap_err();
    assert!(err.contains("auxiliary"));
}

#[test]
fn summarise_cmd_with_aux_none_falls_back_to_heuristic() {
    let v = summarise_cmd_with_aux("First sentence. Second one.", 200, None).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("heuristic"));
}

#[test]
fn summarise_cmd_with_aux_uses_mock_response_when_input_exceeds_max() {
    use crate::agent::llm::auxiliary::{AuxiliaryClient, AuxiliaryConfig};
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::config::AgentConfig;
    let cfg = AgentConfig::default();
    let provider = MockProvider::new("sum-mock", &cfg);
    provider.push_response(MockResponse::Text("Compact summary".into()));
    let aux = AuxiliaryClient::new(
        std::sync::Arc::new(provider),
        AuxiliaryConfig::new("mock", "sum-mock"),
    );
    // Input must exceed max_chars to trigger the aux path (see summarise()).
    let big = "long ".repeat(60);
    let v = summarise_cmd_with_aux(&big, 50, Some(&aux)).expect("ok");
    assert_eq!(v.get("method").and_then(|s| s.as_str()), Some("llm"));
    assert_eq!(
        v.get("summary").and_then(|s| s.as_str()),
        Some("Compact summary")
    );
    assert_eq!(v.get("provider").and_then(|s| s.as_str()), Some("mock"));
}

#[test]
fn classify_cmd_matches_label_case_insensitively() {
    let v = classify_cmd(&[
        "POSITIVE".into(),
        "--labels".into(),
        "positive,negative,neutral".into(),
    ])
    .expect("classify ok");
    assert_eq!(v.get("matched").and_then(|m| m.as_str()), Some("positive"));
}

#[test]
fn classify_cmd_returns_null_on_no_match() {
    let v = classify_cmd(&[
        "definitely not a label".into(),
        "--labels".into(),
        "yes,no".into(),
    ])
    .expect("classify ok");
    assert_eq!(v.get("matched"), Some(&serde_json::Value::Null));
}

#[test]
fn classify_cmd_tolerates_trailing_punctuation() {
    let v =
        classify_cmd(&["yes.".into(), "--labels".into(), "yes,no".into()]).expect("classify ok");
    assert_eq!(v.get("matched").and_then(|m| m.as_str()), Some("yes"));
}

#[test]
fn classify_cmd_requires_labels_flag() {
    let err = classify_cmd(&["yes".into()]).unwrap_err();
    assert!(err.contains("--labels"));
}

#[test]
fn classify_cmd_empty_label_list_rejected() {
    let err = classify_cmd(&["yes".into(), "--labels".into(), ",, ,".into()]).unwrap_err();
    assert!(err.contains("--labels"));
}

#[test]
fn classify_cmd_returns_label_set_in_response() {
    let v = classify_cmd(&["yes".into(), "--labels".into(), "yes,no,maybe".into()])
        .expect("classify ok");
    let labels = v
        .get("labels")
        .and_then(|l| l.as_array())
        .expect("labels array");
    assert_eq!(labels.len(), 3);
}

// ---- tools_cmd ----

#[test]
fn tools_cmd_default_lists_permitted_tools() {
    let v = tools_cmd(&[]).expect("tools list ok");
    let arr = v
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("tools array");
    assert!(
        !arr.is_empty(),
        "default registry should have at least echo + now"
    );
    // Every entry should be permitted under the default permissive guardrails.
    for entry in arr {
        assert_eq!(entry.get("permitted"), Some(&serde_json::Value::Bool(true)));
    }
    let permitted_count = v
        .get("permitted_count")
        .and_then(|c| c.as_u64())
        .unwrap_or(0);
    assert_eq!(permitted_count as usize, arr.len());
}

#[test]
fn tools_cmd_show_returns_full_schema() {
    let v = tools_cmd(&["show".into(), "echo".into()]).expect("tools show ok");
    assert_eq!(v.get("name").and_then(|n| n.as_str()), Some("echo"));
    assert!(v.get("description").is_some());
    assert!(v.get("input_schema").is_some());
}

#[test]
fn tools_cmd_show_unknown_tool_errs() {
    let err = tools_cmd(&["show".into(), "does-not-exist".into()]).unwrap_err();
    assert!(err.contains("does-not-exist"));
}

#[test]
fn tools_cmd_llm_list_returns_serialised_tool_blob() {
    let v = tools_cmd(&["llm-list".into()]).expect("tools llm-list ok");
    let arr = v
        .get("tools")
        .and_then(|t| t.as_array())
        .expect("tools array");
    assert!(!arr.is_empty());
    for entry in arr {
        assert!(entry.get("name").and_then(|n| n.as_str()).is_some());
        assert!(entry.get("input_schema").is_some());
    }
}

#[test]
fn tools_cmd_unfiltered_includes_at_least_as_many_as_filtered() {
    let plain = tools_cmd(&["list".into()]).expect("plain list ok");
    let unfiltered = tools_cmd(&["list".into(), "--unfiltered".into()]).expect("unfiltered ok");
    let plain_count = plain
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    let unfiltered_count = unfiltered
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    assert!(unfiltered_count >= plain_count);
}

// ---- guardrails_cmd ----

#[test]
fn guardrails_cmd_default_show_reports_permissive_mode() {
    let v = guardrails_cmd(&[]).expect("guardrails show ok");
    // Default config has no tool_allow / empty tool_deny → permissive.
    let mode = v.get("mode").and_then(|m| m.as_str()).unwrap_or("");
    assert!(
        mode == "permissive" || mode == "allowlist",
        "mode {mode:?} should be permissive or allowlist"
    );
    assert!(v.get("deny_count").and_then(|c| c.as_u64()).is_some());
}

#[test]
fn guardrails_cmd_check_returns_decision_for_known_tool() {
    let v = guardrails_cmd(&["check".into(), "echo".into()]).expect("guardrails check ok");
    let decision = v.get("decision").and_then(|d| d.as_str()).unwrap_or("");
    assert!(decision == "allow" || decision == "deny");
    assert_eq!(v.get("tool").and_then(|t| t.as_str()), Some("echo"));
}

#[test]
fn guardrails_cmd_check_requires_tool_name() {
    let err = guardrails_cmd(&["check".into()]).unwrap_err();
    assert!(err.contains("check"));
}

// ---- approval_cmd ----

#[test]
fn approval_cmd_default_show_returns_three_sets() {
    let v = approval_cmd(&[]).expect("approval show ok");
    assert!(v.get("auto_approve").and_then(|a| a.as_array()).is_some());
    assert!(v.get("auto_deny").and_then(|a| a.as_array()).is_some());
    assert!(v.get("dangerous").and_then(|a| a.as_array()).is_some());
}

#[test]
fn approval_cmd_check_safe_tool_returns_approved() {
    // Default config has no dangerous_tools → every tool short-circuits to approved.
    let v = approval_cmd(&["check".into(), "echo".into()]).expect("approval check ok");
    assert_eq!(v.get("decision").and_then(|d| d.as_str()), Some("approved"));
    assert_eq!(
        v.get("would_short_circuit").and_then(|b| b.as_bool()),
        Some(true)
    );
}

#[test]
fn approval_cmd_keeps_proc_on_legacy_authority_until_mapping_is_complete() {
    let v = approval_cmd(&["check".into(), "cos_proc".into()]).expect("approval check ok");
    assert_eq!(
        v.get("authority").and_then(|v| v.as_str()),
        Some("legacy_tool_name")
    );
}

#[test]
fn approval_cmd_check_requires_tool_name() {
    let err = approval_cmd(&["check".into()]).unwrap_err();
    assert!(err.contains("check"));
}

#[test]
fn approval_cmd_check_input_must_parse_as_json() {
    let err = approval_cmd(&[
        "check".into(),
        "echo".into(),
        "--input".into(),
        "not json".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--input"));
}

// ---- todo_cmd ----

fn temp_todo_store() -> (tempfile::TempDir, crate::agent::tools::todo::TodoStore) {
    let dir = tempfile::tempdir().expect("tmp");
    let store = crate::agent::tools::todo::TodoStore::new(dir.path().to_path_buf());
    (dir, store)
}

#[test]
fn todo_cmd_path_returns_dir() {
    let v = todo_cmd(&["path".into()]).expect("path ok");
    assert!(v.get("path").and_then(|p| p.as_str()).is_some());
}

#[test]
fn todo_cmd_list_empty_session_returns_empty() {
    let (_dir, store) = temp_todo_store();
    let v = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(0));
    let items = v
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items array");
    assert!(items.is_empty());
}

#[test]
fn todo_cmd_list_requires_session() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["list".into()], &store).unwrap_err();
    assert!(err.contains("list"));
}

#[test]
fn todo_cmd_add_appends_and_persists() {
    let (_dir, store) = temp_todo_store();
    let v = todo_cmd_at(
        &[
            "add".into(),
            "session-1".into(),
            "t1".into(),
            "first".into(),
            "todo".into(),
            "item".into(),
        ],
        &store,
    )
    .expect("add ok");
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(1));

    // Re-read confirms persistence + multi-word title joined.
    let listed = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
    let items = listed
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0].get("title").and_then(|t| t.as_str()),
        Some("first todo item")
    );
    assert_eq!(
        items[0].get("status").and_then(|s| s.as_str()),
        Some("pending")
    );
}

#[test]
fn todo_cmd_add_with_note_flag() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &[
            "add".into(),
            "session-1".into(),
            "t1".into(),
            "title".into(),
            "--note".into(),
            "explanatory note".into(),
        ],
        &store,
    )
    .expect("add ok");
    let listed = todo_cmd_at(&["list".into(), "session-1".into()], &store).expect("list ok");
    let items = listed
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert_eq!(
        items[0].get("note").and_then(|n| n.as_str()),
        Some("explanatory note")
    );
}

#[test]
fn todo_cmd_add_rejects_duplicate_id() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("first add ok");
    let err = todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "second".into()],
        &store,
    )
    .unwrap_err();
    assert!(err.contains("t1"));
}

#[test]
fn todo_cmd_add_requires_title() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["add".into(), "s1".into(), "t1".into()], &store).unwrap_err();
    assert!(err.contains("title"));
}

#[test]
fn todo_cmd_add_note_flag_requires_value() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(
        &[
            "add".into(),
            "s1".into(),
            "t1".into(),
            "title".into(),
            "--note".into(),
        ],
        &store,
    )
    .unwrap_err();
    assert!(err.contains("--note"));
}

#[test]
fn todo_cmd_set_status_updates_one_item() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("add ok");
    let v = todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t1".into(),
            "in_progress".into(),
        ],
        &store,
    )
    .expect("set-status ok");
    assert_eq!(
        v.get("status").and_then(|s| s.as_str()),
        Some("in_progress")
    );
}

#[test]
fn todo_cmd_set_status_accepts_dash_alias() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("add ok");
    // Both `in_progress` and `in-progress` should work.
    todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t1".into(),
            "in-progress".into(),
        ],
        &store,
    )
    .expect("dash alias accepted");
}

#[test]
fn todo_cmd_set_status_rejects_unknown_status() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "first".into()],
        &store,
    )
    .expect("add ok");
    let err = todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t1".into(),
            "bogus".into(),
        ],
        &store,
    )
    .unwrap_err();
    assert!(err.contains("bogus"));
}

#[test]
fn todo_cmd_remove_drops_item() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "a".into()],
        &store,
    )
    .expect("add ok");
    todo_cmd_at(
        &["add".into(), "s1".into(), "t2".into(), "b".into()],
        &store,
    )
    .expect("add ok");
    let v = todo_cmd_at(&["remove".into(), "s1".into(), "t1".into()], &store).expect("remove ok");
    assert_eq!(v.get("count").and_then(|c| c.as_u64()), Some(1));
    let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
    let items = listed
        .get("items")
        .and_then(|i| i.as_array())
        .expect("items");
    assert_eq!(items[0].get("id").and_then(|i| i.as_str()), Some("t2"));
}

#[test]
fn todo_cmd_remove_unknown_id_errs() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["remove".into(), "s1".into(), "ghost".into()], &store).unwrap_err();
    assert!(err.contains("ghost"));
}

#[test]
fn todo_cmd_clear_requires_yes_flag() {
    let (_dir, store) = temp_todo_store();
    let err = todo_cmd_at(&["clear".into(), "s1".into()], &store).unwrap_err();
    assert!(err.contains("--yes"));
}

#[test]
fn todo_cmd_clear_with_yes_wipes_session() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "a".into()],
        &store,
    )
    .expect("add ok");
    let v = todo_cmd_at(&["clear".into(), "s1".into(), "--yes".into()], &store).expect("clear ok");
    assert_eq!(v.get("cleared").and_then(|c| c.as_bool()), Some(true));
    let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
    assert_eq!(listed.get("count").and_then(|c| c.as_u64()), Some(0));
}

#[test]
fn todo_cmd_list_includes_status_breakdown() {
    let (_dir, store) = temp_todo_store();
    todo_cmd_at(
        &["add".into(), "s1".into(), "t1".into(), "a".into()],
        &store,
    )
    .expect("add ok");
    todo_cmd_at(
        &["add".into(), "s1".into(), "t2".into(), "b".into()],
        &store,
    )
    .expect("add ok");
    todo_cmd_at(
        &[
            "set-status".into(),
            "s1".into(),
            "t2".into(),
            "completed".into(),
        ],
        &store,
    )
    .expect("status ok");
    let listed = todo_cmd_at(&["list".into(), "s1".into()], &store).expect("list ok");
    let counts = listed.get("by_status").expect("by_status");
    assert_eq!(counts.get("pending").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(counts.get("completed").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(counts.get("in_progress").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(counts.get("cancelled").and_then(|n| n.as_u64()), Some(0));
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

// ---- mcp_cmd probe/call argument parsing ----

#[test]
fn mcp_probe_requires_cmd() {
    let err = mcp_probe(&[]).unwrap_err();
    assert!(err.contains("--cmd"));
}

#[test]
fn mcp_call_requires_cmd() {
    let err = mcp_call(&[]).unwrap_err();
    assert!(err.contains("--cmd"));
}

#[test]
fn mcp_call_requires_tool_positional() {
    let err = mcp_call(&["--cmd".into(), "nonexistent-binary-xyz-zyx".into()]).unwrap_err();
    assert!(err.contains("tool name"));
}

#[test]
fn parse_mcp_spawn_spec_collects_args_env_cwd_timeout() {
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "python".into(),
        "--arg".into(),
        "-u".into(),
        "--arg".into(),
        "server.py".into(),
        "--env".into(),
        "API_KEY=secret".into(),
        "--env".into(),
        "DEBUG=1".into(),
        "--cwd".into(),
        "/tmp".into(),
        "--timeout".into(),
        "60".into(),
        "leftover-positional".into(),
    ];
    let (spec, leftover) = parse_mcp_spawn_spec(&raw).expect("parse ok");
    assert_eq!(spec.cmd, "python");
    assert_eq!(spec.args, vec!["-u", "server.py"]);
    assert_eq!(
        spec.env,
        vec![
            ("API_KEY".to_string(), "secret".to_string()),
            ("DEBUG".to_string(), "1".to_string()),
        ]
    );
    assert_eq!(spec.cwd.as_deref(), Some("/tmp"));
    assert_eq!(spec.timeout_secs, 60);
    assert_eq!(leftover, vec!["leftover-positional".to_string()]);
}

#[test]
fn parse_mcp_spawn_spec_rejects_malformed_env() {
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "x".into(),
        "--env".into(),
        "noequalshere".into(),
    ];
    let err = parse_mcp_spawn_spec(&raw).unwrap_err();
    assert!(err.contains("KEY=VALUE"));
}

#[test]
fn parse_mcp_spawn_spec_default_timeout_is_30() {
    let raw: Vec<String> = vec!["--cmd".into(), "x".into()];
    let (spec, leftover) = parse_mcp_spawn_spec(&raw).expect("parse ok");
    assert_eq!(spec.timeout_secs, 30);
    assert!(leftover.is_empty());
}

#[test]
fn parse_mcp_spawn_spec_timeout_invalid_errs() {
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "x".into(),
        "--timeout".into(),
        "fast".into(),
    ];
    let err = parse_mcp_spawn_spec(&raw).unwrap_err();
    assert!(err.contains("--timeout"));
}

#[test]
fn mcp_probe_propagates_spawn_failure() {
    // A binary that almost certainly doesn't exist on PATH.
    let raw: Vec<String> = vec![
        "--cmd".into(),
        "definitely-not-a-real-binary-zzz-9999".into(),
        "--timeout".into(),
        "2".into(),
    ];
    let err = mcp_probe(&raw).unwrap_err();
    // tokio::process::Command::spawn returns the underlying OS
    // error; both Windows ("program not found") and Unix ("No such
    // file") flavours are acceptable, so we only assert the binary
    // name is mentioned.
    assert!(err.contains("definitely-not-a-real-binary-zzz-9999"));
}

#[test]
fn mcp_probe_rejects_extra_positional() {
    let err = mcp_probe(&["--cmd".into(), "python".into(), "extra".into()]).unwrap_err();
    assert!(err.contains("positional"));
}

#[test]
fn mcp_call_rejects_invalid_input_json() {
    let err = mcp_call(&[
        "--cmd".into(),
        "python".into(),
        "echo".into(),
        "--input".into(),
        "not json{".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--input"));
}

#[test]
fn mcp_call_rejects_extra_positional() {
    let err = mcp_call(&[
        "--cmd".into(),
        "python".into(),
        "echo".into(),
        "another".into(),
    ])
    .unwrap_err();
    assert!(err.contains("positional"));
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

// ---- skills_guard_cmd ----

fn skills_guard_test_dir(label: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!(
        "cos-agent-skills-guard-{label}-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_test_skill(
    dir: &std::path::Path,
    id: &str,
    tools: &[&str],
) -> crate::agent::skills::loader::LoadedSkill {
    use crate::agent::skills::loader::LoadedSkill;
    use std::fs;
    let sd = dir.join(id);
    fs::create_dir_all(&sd).unwrap();
    let mp = sd.join("SKILL.md");
    let allowed = if tools.is_empty() {
        String::new()
    } else {
        format!(
            "allowed-tools:\n{}\n",
            tools
                .iter()
                .map(|t| format!("  - {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    fs::write(
        &mp,
        format!("---\nname: {id}\ndescription: test\n{allowed}---\n# body\n"),
    )
    .unwrap();
    let doc = crate::agent::skills::manifest::parse(&fs::read_to_string(&mp).unwrap()).unwrap();
    LoadedSkill {
        id: id.to_string(),
        dir: sd,
        manifest_path: mp,
        manifest: doc.manifest,
        body_bytes: doc.body.len(),
        body: doc.body,
        origin: crate::agent::skills::loader::SkillOrigin::Local,
    }
}

fn guard_skills_map(
    skill: crate::agent::skills::loader::LoadedSkill,
) -> std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> {
    let mut m = std::collections::BTreeMap::new();
    m.insert(skill.id.clone(), skill);
    m
}

#[test]
fn skills_guard_unknown_id_errs() {
    let map: std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> =
        std::collections::BTreeMap::new();
    let err = skills_guard_cmd_against(&["nope".into()], &map).unwrap_err();
    assert!(err.contains("not loaded"));
}

#[test]
fn skills_guard_missing_id_errs() {
    let map: std::collections::BTreeMap<String, crate::agent::skills::loader::LoadedSkill> =
        std::collections::BTreeMap::new();
    let err = skills_guard_cmd_against(&[], &map).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn skills_guard_default_provenance_hub_allows_clean_skill() {
    let dir = skills_guard_test_dir("default-hub");
    let skill = write_test_skill(&dir, "alpha", &["echo"]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(&["alpha".into()], &map).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
    assert_eq!(v.get("provenance").and_then(|s| s.as_str()), Some("hub"));
}

#[test]
fn skills_guard_vendor_provenance_is_trusted() {
    // Even with require_allowed_tools + zero declared tools,
    // vendor provenance + honour_provenance_trust = Allow.
    let dir = skills_guard_test_dir("vendor-trust");
    let skill = write_test_skill(&dir, "beta", &[]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(
        &[
            "beta".into(),
            "--provenance".into(),
            "vendor".into(),
            "--require-allowed-tools".into(),
        ],
        &map,
    )
    .expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
}

#[test]
fn skills_guard_require_allowed_tools_denies_empty_hub_skill() {
    let dir = skills_guard_test_dir("require-tools");
    let skill = write_test_skill(&dir, "gamma", &[]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(&["gamma".into(), "--require-allowed-tools".into()], &map)
        .expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert!(v.get("reason").and_then(|s| s.as_str()).is_some());
}

#[test]
fn skills_guard_ignore_trust_strips_vendor_pass() {
    // vendor + ignore-trust + require-allowed-tools (empty) → deny.
    let dir = skills_guard_test_dir("ignore-trust");
    let skill = write_test_skill(&dir, "delta", &[]);
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(
        &[
            "delta".into(),
            "--provenance".into(),
            "vendor".into(),
            "--ignore-trust".into(),
            "--require-allowed-tools".into(),
        ],
        &map,
    )
    .expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert_eq!(
        v.get("config")
            .and_then(|c| c.get("honour_provenance_trust"))
            .and_then(|b| b.as_bool()),
        Some(false)
    );
}

#[test]
fn skills_guard_max_file_bytes_triggers_confirmation() {
    // Write a sibling file larger than the cap and verify the
    // verdict flips to require_confirmation.
    let dir = skills_guard_test_dir("max-bytes");
    let skill = write_test_skill(&dir, "epsilon", &["echo"]);
    // 200 bytes payload, cap = 100 bytes.
    std::fs::write(skill.dir.join("data.bin"), vec![0u8; 200]).unwrap();
    let map = guard_skills_map(skill);
    let v = skills_guard_cmd_against(
        &["epsilon".into(), "--max-file-bytes".into(), "100".into()],
        &map,
    )
    .expect("ok");
    assert_eq!(
        v.get("verdict").and_then(|s| s.as_str()),
        Some("require_confirmation")
    );
    assert!(v
        .get("reason")
        .and_then(|s| s.as_str())
        .map(|r| r.contains("data.bin"))
        .unwrap_or(false));
}

#[test]
fn skills_guard_unknown_provenance_errs() {
    let dir = skills_guard_test_dir("bad-prov");
    let skill = write_test_skill(&dir, "zeta", &["echo"]);
    let map = guard_skills_map(skill);
    let err = skills_guard_cmd_against(
        &["zeta".into(), "--provenance".into(), "alien".into()],
        &map,
    )
    .unwrap_err();
    assert!(err.contains("alien"));
}

#[test]
fn skills_guard_invalid_max_file_bytes_errs() {
    let dir = skills_guard_test_dir("bad-bytes");
    let skill = write_test_skill(&dir, "theta", &["echo"]);
    let map = guard_skills_map(skill);
    let err = skills_guard_cmd_against(
        &["theta".into(), "--max-file-bytes".into(), "lots".into()],
        &map,
    )
    .unwrap_err();
    assert!(err.contains("--max-file-bytes"));
}

// ---- sessions_cmd / sessions_*_with ----

fn fresh_session_db() -> memory::sqlite_fts::MemoryDb {
    memory::sqlite_fts::MemoryDb::open_in_memory().expect("open in-memory db")
}

#[test]
fn sessions_list_with_empty_db_returns_no_sessions() {
    let db = fresh_session_db();
    let v = sessions_list_with(&db, 20).expect("list ok");
    assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(v.get("limit").and_then(|n| n.as_u64()), Some(20));
    assert!(v
        .get("sessions")
        .and_then(|s| s.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(false));
}

#[test]
fn sessions_list_with_returns_recorded_sessions_in_recency_order() {
    let db = fresh_session_db();
    db.record_message("s-old", "user", "hi old").unwrap();
    // Tick to ensure a different ms.
    std::thread::sleep(std::time::Duration::from_millis(5));
    db.record_message("s-new", "user", "hi new").unwrap();

    let v = sessions_list_with(&db, 10).expect("list ok");
    let arr = v.get("sessions").and_then(|s| s.as_array()).expect("array");
    assert_eq!(arr.len(), 2);
    // Most recent first.
    assert_eq!(
        arr[0].get("session_id").and_then(|s| s.as_str()),
        Some("s-new")
    );
    assert_eq!(
        arr[1].get("session_id").and_then(|s| s.as_str()),
        Some("s-old")
    );
}

#[test]
fn sessions_title_with_returns_null_when_unset() {
    let db = fresh_session_db();
    let v = sessions_title_with(&db, "sx").expect("title ok");
    assert_eq!(v.get("set").and_then(|b| b.as_bool()), Some(false));
    assert!(v.get("title").map(|t| t.is_null()).unwrap_or(false));
}

#[test]
fn sessions_set_title_with_then_title_with_round_trips() {
    let db = fresh_session_db();
    let v = sessions_set_title_with(&db, "sx", "My Session").expect("set ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("My Session"));
    let v2 = sessions_title_with(&db, "sx").expect("title ok");
    assert_eq!(v2.get("title").and_then(|s| s.as_str()), Some("My Session"));
    assert_eq!(v2.get("set").and_then(|b| b.as_bool()), Some(true));
}

#[test]
fn sessions_set_title_overwrites_existing_title() {
    let db = fresh_session_db();
    sessions_set_title_with(&db, "sx", "first").expect("set ok");
    sessions_set_title_with(&db, "sx", "second").expect("set ok");
    let v = sessions_title_with(&db, "sx").expect("title ok");
    assert_eq!(v.get("title").and_then(|s| s.as_str()), Some("second"));
}

#[test]
fn parse_set_title_args_accepts_multi_word_title() {
    let (id, title) = parse_set_title_args(&[
        "sid".into(),
        "Hello".into(),
        "World".into(),
        "Of".into(),
        "Tests".into(),
    ])
    .expect("parse ok");
    assert_eq!(id, "sid");
    assert_eq!(title, "Hello World Of Tests");
}

#[test]
fn parse_set_title_args_stops_at_first_flag() {
    let (id, title) = parse_set_title_args(&[
        "sid".into(),
        "Hello".into(),
        "World".into(),
        "--unknown".into(),
        "ignored".into(),
    ])
    .expect("parse ok");
    assert_eq!(id, "sid");
    assert_eq!(title, "Hello World");
}

#[test]
fn parse_set_title_args_requires_title() {
    let err = parse_set_title_args(&["sid".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn parse_set_title_args_rejects_id_starting_with_double_dash() {
    let err = parse_set_title_args(&["--id".into(), "title".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn sessions_count_with_total_includes_all_sessions() {
    let db = fresh_session_db();
    db.record_message("s1", "user", "a").unwrap();
    db.record_message("s1", "assistant", "b").unwrap();
    db.record_message("s2", "user", "c").unwrap();
    let v = sessions_count_with(&db, None).expect("count ok");
    assert_eq!(v.get("total_messages").and_then(|n| n.as_i64()), Some(3));
}

#[test]
fn sessions_count_with_filters_by_session_id() {
    let db = fresh_session_db();
    db.record_message("s1", "user", "a").unwrap();
    db.record_message("s1", "assistant", "b").unwrap();
    db.record_message("s2", "user", "c").unwrap();
    let v = sessions_count_with(&db, Some("s1")).expect("count ok");
    assert_eq!(v.get("messages").and_then(|n| n.as_i64()), Some(2));
    assert_eq!(v.get("session_id").and_then(|s| s.as_str()), Some("s1"));
}

#[test]
fn sessions_clear_with_drops_session_messages_only() {
    let db = fresh_session_db();
    db.record_message("s1", "user", "a").unwrap();
    db.record_message("s1", "assistant", "b").unwrap();
    db.record_message("s2", "user", "c").unwrap();
    let v = sessions_clear_with(&db, "s1").expect("clear ok");
    assert_eq!(v.get("messages_cleared").and_then(|n| n.as_u64()), Some(2));
    // s2 should be intact.
    let total = sessions_count_with(&db, None).expect("count ok");
    assert_eq!(
        total.get("total_messages").and_then(|n| n.as_i64()),
        Some(1)
    );
}

#[test]
fn sessions_clear_refuses_without_yes_flag() {
    let err = sessions_clear(&["sx".into()]).unwrap_err();
    assert!(err.contains("--yes"));
}

#[test]
fn sessions_clear_requires_session_id() {
    let err = sessions_clear(&[]).unwrap_err();
    assert!(err.contains("usage"));
    let err2 = sessions_clear(&["--yes".into()]).unwrap_err();
    assert!(err2.contains("usage"));
}

#[test]
fn sessions_cmd_numeric_first_arg_routes_to_list() {
    // Numeric first arg keeps backward-compat: cos agent sessions 5 → list 5.
    let v = sessions_cmd(&["5".into()]).expect("legacy list ok");
    assert_eq!(v.get("limit").and_then(|n| n.as_u64()), Some(5));
}

// ---- sessions_purge ----

#[test]
fn sessions_purge_requires_older_than() {
    let err = sessions_purge(&["--yes".into()]).unwrap_err();
    assert!(err.contains("--older-than"), "got {err}");
}

#[test]
fn sessions_purge_validates_days_is_positive_integer() {
    let err = sessions_purge(&["--older-than".into(), "0".into(), "--yes".into()]).unwrap_err();
    assert!(err.contains("> 0"), "got {err}");
    let err2 = sessions_purge(&["--older-than".into(), "abc".into(), "--yes".into()]).unwrap_err();
    assert!(err2.contains("positive integer"), "got {err2}");
}

#[test]
fn sessions_purge_refuses_apply_without_yes() {
    let err = sessions_purge(&["--older-than".into(), "1".into()]).unwrap_err();
    assert!(err.contains("--yes"), "got {err}");
    assert!(err.contains("--dry-run"), "got {err}");
}

#[test]
fn sessions_purge_with_dry_run_does_not_mutate() {
    let db = fresh_session_db();
    // Insert one ancient message with explicit ts so we can
    // exercise the cutoff cleanly.
    db.record_message_at("old", "user", "ancient", 100).unwrap();
    // And one fresh row via the normal path so its ts_ms is now.
    db.record_message("new", "user", "fresh").unwrap();
    // Cutoff = 1000ms; "old" (100) is below, "new" (~now) is above.
    let v = sessions_purge_with(&db, 1000, 7, true).expect("dry ok");
    assert_eq!(v["dry_run"], json!(true));
    assert_eq!(v["messages_deleted"], json!(1));
    assert_eq!(v["sessions_emptied"], json!(1));
    // Messages still on disk after dry-run.
    let total = sessions_count_with(&db, None).unwrap();
    assert_eq!(total["total_messages"].as_i64(), Some(2));
}

#[test]
fn sessions_purge_with_apply_drops_old_rows_and_titles() {
    let db = fresh_session_db();
    db.record_message_at("old", "user", "ancient", 100).unwrap();
    db.set_title("old", "Old Convo").unwrap();
    db.record_message("new", "user", "fresh").unwrap();
    // Apply with cutoff=1000.
    let v = sessions_purge_with(&db, 1000, 7, false).expect("apply ok");
    assert_eq!(v["dry_run"], json!(false));
    assert_eq!(v["messages_deleted"], json!(1));
    assert_eq!(v["sessions_emptied"], json!(1));
    assert_eq!(v["titles_deleted"], json!(1));
    // Only "new" remains.
    let total = sessions_count_with(&db, None).unwrap();
    assert_eq!(total["total_messages"].as_i64(), Some(1));
    // Title for "old" is gone.
    let title = db.title_for("old").unwrap();
    assert!(title.is_none());
}

#[test]
fn sessions_purge_empty_db_returns_zero_counts() {
    let db = fresh_session_db();
    let v = sessions_purge_with(&db, 1000, 7, false).expect("apply ok");
    assert_eq!(v["messages_deleted"], json!(0));
    assert_eq!(v["sessions_emptied"], json!(0));
    assert_eq!(v["titles_deleted"], json!(0));
}

#[test]
fn sessions_purge_dispatched_via_sessions_cmd() {
    // Smoke test that the `purge` verb is wired through
    // sessions_cmd. We pass --dry-run --older-than 999999 to
    // ensure no rows match (so the test doesn't depend on the
    // shared default db being empty).
    let v = sessions_cmd(&[
        "purge".into(),
        "--older-than".into(),
        "999999".into(),
        "--dry-run".into(),
    ])
    .expect("dispatch ok");
    assert_eq!(v["dry_run"], json!(true));
    assert_eq!(v["older_than_days"], json!(999999u64));
}

// ---- sessions_stats ----

#[test]
fn sessions_stats_rejects_extra_args() {
    let err = sessions_stats(&["bogus".into()]).unwrap_err();
    assert!(err.contains("unexpected argument"), "got {err}");
}

#[test]
fn sessions_stats_session_flag_rejects_empty_value() {
    let err = sessions_stats(&["--session".into(), "".into()]).unwrap_err();
    assert!(err.contains("must not be empty"), "got {err}");
}

#[test]
fn sessions_stats_session_with_unknown_id_returns_zeros() {
    let db = fresh_session_db();
    // Other sessions exist, but the requested one does not.
    db.record_message("other", "user", "x").unwrap();
    let v = sessions_stats_session_with(&db, "ghost", 1_000_000).expect("stats ok");
    assert_eq!(v["scope"], json!("session"));
    assert_eq!(v["session_id"], json!("ghost"));
    assert_eq!(v["title"], json!(null));
    assert_eq!(v["total_messages"], json!(0u64));
    assert_eq!(v["by_role"], json!([]));
    // No total_sessions / titled_sessions in per-session shape.
    assert!(v.get("total_sessions").is_none());
    assert!(v.get("titled_sessions").is_none());
}

#[test]
fn sessions_stats_session_with_isolates_one_session() {
    let db = fresh_session_db();
    let now: i64 = 100 * 86_400_000;
    for _ in 0..3 {
        db.record_message_at("alpha", "user", "a", now - 3_600_000)
            .unwrap();
    }
    for _ in 0..7 {
        db.record_message_at("beta", "user", "b", now).unwrap();
    }
    db.set_title("alpha", "Alpha").unwrap();
    let v = sessions_stats_session_with(&db, "alpha", now).expect("stats ok");
    assert_eq!(v["session_id"], json!("alpha"));
    assert_eq!(v["title"], json!("Alpha"));
    assert_eq!(v["total_messages"], json!(3u64));
    assert_eq!(v["messages_last_1d"], json!(3u64));
    assert_eq!(v["by_role"], json!([{"role": "user", "count": 3u64}]));
}

#[test]
fn sessions_stats_dispatched_with_session_flag() {
    let v = sessions_cmd(&["stats".into(), "--session".into(), "no-such-id".into()])
        .expect("dispatch ok");
    assert_eq!(v["scope"], json!("session"));
    assert_eq!(v["session_id"], json!("no-such-id"));
}

#[test]
fn sessions_stats_with_empty_db_is_all_zeros() {
    let db = fresh_session_db();
    let v = sessions_stats_with(&db, 1_000_000).expect("stats ok");
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["total_messages"], json!(0u64));
    assert_eq!(v["total_sessions"], json!(0u64));
    assert_eq!(v["titled_sessions"], json!(0u64));
    assert_eq!(v["messages_last_7d"], json!(0u64));
    assert_eq!(v["by_role"], json!([]));
    assert_eq!(v["oldest_ts_ms"], json!(null));
    assert_eq!(v["newest_ts_ms"], json!(null));
}

#[test]
fn sessions_stats_with_buckets_recency_and_role() {
    let db = fresh_session_db();
    let now: i64 = 100 * 86_400_000;
    db.record_message_at("s", "user", "fresh", now - 3_600_000)
        .unwrap();
    db.record_message_at("s", "assistant", "old", now - 10 * 86_400_000)
        .unwrap();
    db.record_message_at("t", "user", "ancient", now - 60 * 86_400_000)
        .unwrap();
    db.set_title("s", "Hello").unwrap();
    let v = sessions_stats_with(&db, now).expect("stats ok");
    assert_eq!(v["total_messages"], json!(3u64));
    assert_eq!(v["total_sessions"], json!(2u64));
    assert_eq!(v["titled_sessions"], json!(1u64));
    assert_eq!(v["messages_last_1d"], json!(1u64));
    assert_eq!(v["messages_last_7d"], json!(1u64));
    assert_eq!(v["messages_last_30d"], json!(2u64));
    // by_role: "user" leads with 2, "assistant" trails with 1.
    let roles = v["by_role"].as_array().expect("array");
    assert_eq!(roles.len(), 2);
    assert_eq!(roles[0]["role"], json!("user"));
    assert_eq!(roles[0]["count"], json!(2u64));
    assert_eq!(v["oldest_ts_ms"], json!(now - 60 * 86_400_000));
    assert_eq!(v["newest_ts_ms"], json!(now - 3_600_000));
}

#[test]
fn sessions_stats_dispatched_via_sessions_cmd() {
    let v = sessions_cmd(&["stats".into()]).expect("dispatch ok");
    assert!(v.get("total_messages").is_some());
    assert!(v.get("by_role").is_some());
}

#[test]
fn sessions_health_with_path_reports_focused_checks() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("memory.db");
    let v = sessions_health_with_path(&path).expect("health");
    assert_eq!(v["initialized"], json!(false));
    assert!(v.get("sqlite").is_some());
    assert!(v.get("fts").is_some());
    assert!(v.get("prompt_hashes").is_some());
    assert!(v.get("compactions").is_some());
}

#[test]
fn sessions_repair_requires_confirmation_for_mutation() {
    let error = sessions_repair(&[]).unwrap_err();
    assert!(error.contains("--yes"));
    let error = sessions_repair(&["--unknown".into()]).unwrap_err();
    assert!(error.contains("unknown repair arg"));
}

#[test]
fn sessions_repair_with_path_supports_non_mutating_preview() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("memory.db");
    let v = sessions_repair_with_path(
        &path,
        memory::recovery::RepairOptions {
            dry_run: true,
            ..memory::recovery::RepairOptions::default()
        },
    )
    .expect("preview");
    assert_eq!(v["status"], json!("planned"));
    assert_eq!(v["dry_run"], json!(true));
    assert!(!path.exists());
}

// ---- sessions_top ----

#[test]
fn sessions_top_with_empty_db_returns_empty_array() {
    let db = fresh_session_db();
    let v = sessions_top_with(&db, 10).expect("top ok");
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["limit"], json!(10u64));
    assert_eq!(v["n"], json!(0u64));
    assert_eq!(v["ordered_by"], json!("message_count_desc"));
    assert_eq!(v["sessions"], json!([]));
}

#[test]
fn sessions_top_with_orders_by_count_desc() {
    let db = fresh_session_db();
    // "fat" 3 msgs, "mid" 2, "thin" 1.
    for _ in 0..3 {
        db.record_message("fat", "user", "x").unwrap();
    }
    for _ in 0..2 {
        db.record_message("mid", "user", "x").unwrap();
    }
    db.record_message("thin", "user", "x").unwrap();
    let v = sessions_top_with(&db, 10).expect("top ok");
    assert_eq!(v["n"], json!(3u64));
    let arr = v["sessions"].as_array().unwrap();
    assert_eq!(arr[0]["session_id"], json!("fat"));
    assert_eq!(arr[0]["message_count"], json!(3));
    assert_eq!(arr[1]["session_id"], json!("mid"));
    assert_eq!(arr[2]["session_id"], json!("thin"));
}

#[test]
fn sessions_top_with_carries_titles() {
    let db = fresh_session_db();
    db.record_message("s", "user", "x").unwrap();
    db.set_title("s", "Greeting").unwrap();
    let v = sessions_top_with(&db, 10).expect("top ok");
    let arr = v["sessions"].as_array().unwrap();
    assert_eq!(arr[0]["title"], json!("Greeting"));
}

#[test]
fn sessions_top_default_limit_is_20() {
    // Just make sure no parse errors; with no rows the array is
    // empty but the limit echoes 20.
    let v = sessions_top(&[]).expect("dispatch ok");
    assert_eq!(v["limit"], json!(20u64));
}

#[test]
fn sessions_top_dispatched_via_sessions_cmd() {
    let v = sessions_cmd(&["top".into(), "5".into()]).expect("dispatch ok");
    assert_eq!(v["limit"], json!(5u64));
    assert_eq!(v["ordered_by"], json!("message_count_desc"));
}

// ---- semantic_cmd: clear-all guards + status drift ----

#[test]
fn semantic_clear_all_refuses_without_yes() {
    let err = semantic_cmd(&["clear-all".into()]).unwrap_err();
    assert!(
        err.contains("--yes"),
        "expected error to point at --yes, got: {err}"
    );
}

#[test]
fn semantic_no_subcommand_errs_with_usage() {
    let err = semantic_cmd(&[]).unwrap_err();
    assert!(err.contains("usage"));
    assert!(err.contains("clear-all"));
}

// ---- vision_cmd / vision_route_cmd ----

#[test]
fn vision_cmd_default_subcommand_errs_with_usage() {
    let err = vision_cmd(&[]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn vision_route_synthetic_native_when_provider_vision_and_widely_supported() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("native"));
    assert!(v.get("reason").map(|r| r.is_null()).unwrap_or(false));
}

#[test]
fn vision_route_skip_when_vision_disabled() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
        "--vision-disabled".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
    assert!(v
        .get("reason")
        .and_then(|s| s.as_str())
        .map(|r| r.contains("vision disabled"))
        .unwrap_or(false));
}

#[test]
fn vision_route_skip_when_zero_bytes() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "0".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
}

#[test]
fn vision_route_extract_text_intent_prefers_ocr_when_available() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
        "--ocr-available".into(),
        "--intent".into(),
        "extract-text".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("ocr"));
}

#[test]
fn vision_route_skip_when_oversized_and_no_ocr() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "10000000".into(),
        "--mime".into(),
        "image/png".into(),
        "--provider-vision".into(),
        "--max-native-bytes".into(),
        "1000000".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
    assert!(v
        .get("reason")
        .and_then(|s| s.as_str())
        .map(|r| r.contains("exceeds native cap"))
        .unwrap_or(false));
}

#[test]
fn vision_route_unsupported_mime_without_ocr_skips() {
    let v = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/heic".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("skip"));
}

#[test]
fn vision_route_requires_bytes_or_file() {
    let err = vision_route_cmd(&[]).unwrap_err();
    assert!(err.contains("--file") || err.contains("--bytes"));
}

#[test]
fn vision_route_bytes_without_mime_errs() {
    let err = vision_route_cmd(&["--bytes".into(), "1024".into()]).unwrap_err();
    assert!(err.contains("--mime"));
}

#[test]
fn vision_route_unknown_intent_errs() {
    let err = vision_route_cmd(&[
        "--bytes".into(),
        "1024".into(),
        "--mime".into(),
        "image/png".into(),
        "--intent".into(),
        "bogus".into(),
    ])
    .unwrap_err();
    assert!(err.contains("bogus"));
}

#[test]
fn vision_route_file_uses_on_disk_size_and_extension_mime() {
    let dir = std::env::temp_dir().join(format!(
        "cos-agent-vision-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.png");
    std::fs::write(&path, vec![0u8; 4096]).unwrap();
    let v = vision_route_cmd(&[
        "--file".into(),
        path.display().to_string(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    let desc = v.get("descriptor").expect("descriptor");
    assert_eq!(desc.get("bytes_len").and_then(|n| n.as_u64()), Some(4096));
    assert_eq!(desc.get("mime").and_then(|m| m.as_str()), Some("Png"));
    assert_eq!(v.get("decision").and_then(|s| s.as_str()), Some("native"));
    // Cleanup.
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_route_file_mime_override_wins() {
    let dir = std::env::temp_dir().join(format!(
        "cos-agent-vision-mime-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("test.dat");
    std::fs::write(&path, vec![0u8; 100]).unwrap();
    let v = vision_route_cmd(&[
        "--file".into(),
        path.display().to_string(),
        "--mime".into(),
        "image/jpeg".into(),
        "--provider-vision".into(),
    ])
    .expect("ok");
    let desc = v.get("descriptor").expect("descriptor");
    assert_eq!(desc.get("mime").and_then(|m| m.as_str()), Some("Jpeg"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_route_file_missing_path_errs() {
    let err = vision_route_cmd(&[
        "--file".into(),
        "Z:\\definitely\\does\\not\\exist.png".into(),
    ])
    .unwrap_err();
    // On unix the path also won't exist.
    assert!(err.contains("stat") || err.contains("not"));
}

// ---- vision_sniff_cmd ----

#[test]
fn vision_sniff_requires_file_or_url() {
    let err = vision_sniff_cmd(&[]).unwrap_err();
    assert!(err.contains("--file") && err.contains("--url"));
}

#[test]
fn vision_sniff_rejects_both_file_and_url() {
    let err = vision_sniff_cmd(&[
        "--file".into(),
        "x.png".into(),
        "--url".into(),
        "https://x.invalid/y".into(),
    ])
    .unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn vision_sniff_file_returns_mime_and_len() {
    // Write a tiny PNG-magic-byte stub (8-byte signature) to a temp
    // file and confirm sniff_mime classifies it.
    let dir = std::env::temp_dir().join(format!(
        "cos-vision-sniff-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.png");
    std::fs::write(
        &path,
        [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x01],
    )
    .unwrap();

    let v = vision_sniff_cmd(&["--file".into(), path.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("bytes_len").and_then(|n| n.as_u64()), Some(10));
    assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Png"));
    assert_eq!(
        v.get("mime_widely_supported").and_then(|b| b.as_bool()),
        Some(true)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_sniff_file_unknown_magic_classifies_other() {
    let dir = std::env::temp_dir().join(format!(
        "cos-vision-sniff-other-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.bin");
    std::fs::write(&path, b"this is not an image").unwrap();

    let v = vision_sniff_cmd(&["--file".into(), path.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Other"));
    assert_eq!(v.get("is_other").and_then(|b| b.as_bool()), Some(true));
    assert_eq!(
        v.get("mime_widely_supported").and_then(|b| b.as_bool()),
        Some(false)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vision_sniff_file_missing_path_errs() {
    let err = vision_sniff_cmd(&[
        "--file".into(),
        "Z:\\definitely\\does\\not\\exist.png".into(),
    ])
    .unwrap_err();
    assert!(err.contains("stat") || err.contains("not"));
}

#[test]
fn vision_sniff_head_bytes_caps_inspection_window() {
    let dir = std::env::temp_dir().join(format!(
        "cos-vision-sniff-head-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("x.png");
    // 1KB file but PNG magic in first 8 bytes.
    let mut data = vec![0u8; 1024];
    data[0..8].copy_from_slice(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
    std::fs::write(&path, &data).unwrap();

    let v = vision_sniff_cmd(&[
        "--file".into(),
        path.to_string_lossy().to_string(),
        "--head-bytes".into(),
        "8".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("bytes_len").and_then(|n| n.as_u64()), Some(1024));
    assert_eq!(
        v.get("head_bytes_inspected").and_then(|n| n.as_u64()),
        Some(8)
    );
    assert_eq!(v.get("mime").and_then(|s| s.as_str()), Some("Png"));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- vision_analyze_cmd ----

#[test]
fn vision_analyze_requires_prompt() {
    let err = vision_analyze_cmd(&["--file".into(), "x.png".into()]).unwrap_err();
    assert!(err.contains("--prompt"));
}

#[test]
fn vision_analyze_empty_prompt_errs() {
    let err = vision_analyze_cmd(&[
        "--file".into(),
        "x.png".into(),
        "--prompt".into(),
        "   ".into(),
    ])
    .unwrap_err();
    assert!(err.contains("non-empty"));
}

#[test]
fn vision_analyze_rejects_zero_image_sources() {
    let err = vision_analyze_cmd(&["--prompt".into(), "describe".into()]).unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn vision_analyze_rejects_two_image_sources() {
    let err = vision_analyze_cmd(&[
        "--file".into(),
        "x.png".into(),
        "--url".into(),
        "https://x.invalid".into(),
        "--prompt".into(),
        "describe".into(),
    ])
    .unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn vision_analyze_base64_requires_mime() {
    let err = vision_analyze_cmd(&[
        "--base64".into(),
        "AAAA".into(),
        "--prompt".into(),
        "describe".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--mime"));
}

#[test]
fn vision_analyze_file_missing_errs_clean() {
    let err = vision_analyze_cmd(&[
        "--file".into(),
        "Z:\\nope\\image.png".into(),
        "--prompt".into(),
        "describe".into(),
    ])
    .unwrap_err();
    assert!(err.contains("read"));
}

// ---- vision_cmd dispatch picks up new subcommands ----

#[test]
fn vision_cmd_routes_sniff_subcommand() {
    // Empty sniff still dispatches into vision_sniff_cmd; we just
    // assert that the error originates from that helper.
    let err = vision_cmd(&["sniff".into()]).unwrap_err();
    assert!(err.contains("--file") && err.contains("--url"));
}

#[test]
fn vision_cmd_routes_analyze_subcommand() {
    let err = vision_cmd(&["analyze".into()]).unwrap_err();
    assert!(err.contains("--prompt"));
}

// ---- display_cmd ----

#[test]
fn display_format_bytes_renders_human_readable() {
    let v = display_format_bytes_cmd(&["1536".into()]).expect("ok");
    assert_eq!(v.get("input").and_then(|n| n.as_u64()), Some(1536));
    assert_eq!(v.get("formatted").and_then(|s| s.as_str()), Some("1.5 KB"));
}

#[test]
fn display_format_bytes_rejects_non_numeric() {
    let err = display_format_bytes_cmd(&["abc".into()]).unwrap_err();
    assert!(err.contains("abc"));
}

#[test]
fn display_format_duration_renders_minutes_seconds() {
    let v = display_format_duration_cmd(&["83400".into()]).expect("ok");
    assert_eq!(v.get("input_ms").and_then(|n| n.as_u64()), Some(83_400));
    let s = v.get("formatted").and_then(|s| s.as_str()).unwrap();
    // 83.4s → "1m 23.4s"
    assert!(s.starts_with("1m"));
}

#[test]
fn display_transcript_requires_session() {
    let err = parse_display_transcript_args(&[]).expect("parse");
    assert!(err.session.is_none());
    // The cmd-level call surfaces the missing-session error:
    let err = display_transcript_cmd(&[]).unwrap_err();
    assert!(err.contains("--session"));
}

#[test]
fn display_transcript_renders_messages_oldest_first() {
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
    db.record_message("sess-x", "user", "hello world").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    db.record_message("sess-x", "assistant", "hi back").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(5));
    db.record_message("sess-x", "tool", "result foo: 42")
        .unwrap();
    let parsed = DisplayTranscriptArgs {
        session: Some("sess-x".into()),
        limit: Some(10),
        ..Default::default()
    };
    let v = display_transcript_with(&db, "sess-x", &parsed).expect("render");
    assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(3));
    let t = v.get("transcript").and_then(|s| s.as_str()).unwrap();
    let user_pos = t.find("hello world").expect("user line");
    let asst_pos = t.find("hi back").expect("assistant line");
    let tool_pos = t.find("result foo").expect("tool line");
    assert!(user_pos < asst_pos);
    assert!(asst_pos < tool_pos);
    assert!(t.contains("[user]"));
    assert!(t.contains("[assistant]"));
    assert!(t.contains("[tool]"));
}

#[test]
fn display_transcript_truncates_long_content_by_default() {
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
    let big = "X".repeat(10_000);
    db.record_message("sess-y", "user", &big).unwrap();
    let parsed = DisplayTranscriptArgs {
        session: Some("sess-y".into()),
        ..Default::default()
    };
    let v = display_transcript_with(&db, "sess-y", &parsed).expect("render");
    let t = v.get("transcript").and_then(|s| s.as_str()).unwrap();
    assert!(t.contains("chars omitted"));
}

#[test]
fn display_transcript_no_truncate_keeps_full_content() {
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
    let big = "Y".repeat(10_000);
    db.record_message("sess-z", "user", &big).unwrap();
    let parsed = DisplayTranscriptArgs {
        session: Some("sess-z".into()),
        no_truncate: true,
        // Disable wrap so we can count Y's reliably without inserted newlines.
        width: Some(0),
        ..Default::default()
    };
    let v = display_transcript_with(&db, "sess-z", &parsed).expect("render");
    let t = v.get("transcript").and_then(|s| s.as_str()).unwrap();
    assert!(!t.contains("chars omitted"));
    let y_count = t.chars().filter(|c| *c == 'Y').count();
    assert_eq!(y_count, 10_000);
}

#[test]
fn display_transcript_empty_session_renders_empty_transcript() {
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().expect("open mem db");
    let parsed = DisplayTranscriptArgs {
        session: Some("nope".into()),
        ..Default::default()
    };
    let v = display_transcript_with(&db, "nope", &parsed).expect("render");
    assert_eq!(v.get("message_count").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(v.get("transcript").and_then(|s| s.as_str()), Some(""));
}

#[test]
fn shell_hooks_path_returns_default_log_path() {
    let v = shell_hooks_cmd(&["path".into()]).expect("path ok");
    let p = v.get("path").and_then(|s| s.as_str()).expect("path field");
    assert!(p.ends_with("shell-hooks.jsonl"), "got path: {p}");
}

#[test]
fn shell_hooks_default_subcommand_is_path() {
    let v = shell_hooks_cmd(&[]).expect("default ok");
    assert!(v.get("path").is_some());
}

#[test]
fn shell_hooks_init_bash_returns_script_with_trap() {
    let v = shell_hooks_cmd(&["init".into(), "bash".into()]).expect("init bash ok");
    assert_eq!(v.get("shell").and_then(|s| s.as_str()), Some("bash"));
    let script = v.get("script").and_then(|s| s.as_str()).expect("script");
    assert!(script.contains("trap '__cos_pre_exec' DEBUG"));
    assert!(v.get("instructions").and_then(|s| s.as_str()).is_some());
}

#[test]
fn shell_hooks_init_zsh_returns_zsh_specific_script() {
    let v = shell_hooks_cmd(&["init".into(), "zsh".into()]).expect("init zsh ok");
    assert_eq!(v.get("shell").and_then(|s| s.as_str()), Some("zsh"));
    let script = v.get("script").and_then(|s| s.as_str()).expect("script");
    assert!(script.contains("add-zsh-hook preexec"));
}

#[test]
fn shell_hooks_init_fish_returns_fish_specific_script() {
    let v = shell_hooks_cmd(&["init".into(), "fish".into()]).expect("init fish ok");
    assert_eq!(v.get("shell").and_then(|s| s.as_str()), Some("fish"));
    let script = v.get("script").and_then(|s| s.as_str()).expect("script");
    assert!(script.contains("--on-event fish_preexec"));
}

#[test]
fn shell_hooks_init_unknown_shell_errs() {
    let err = shell_hooks_cmd(&["init".into(), "powershell".into()]).unwrap_err();
    assert!(err.contains("powershell"));
}

#[test]
fn shell_hooks_init_missing_shell_errs() {
    let err = shell_hooks_cmd(&["init".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn shell_hooks_record_pre_requires_cmd() {
    let err = shell_hooks_cmd(&["record-pre".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn shell_hooks_record_post_requires_int_exit() {
    let err = shell_hooks_cmd(&["record-post".into(), "not-a-number".into()]).unwrap_err();
    assert!(err.contains("integer"));
}

#[test]
fn shell_hooks_tail_limit_requires_int() {
    let err = shell_hooks_cmd(&["tail".into(), "--limit".into(), "abc".into()]).unwrap_err();
    assert!(err.contains("--limit"));
}

#[test]
fn shell_hooks_clear_requires_yes_flag() {
    let err = shell_hooks_cmd(&["clear".into()]).unwrap_err();
    assert!(err.contains("--yes"));
}

#[test]
fn media_default_lists_provider_registries() {
    let v = media_cmd(&[]).expect("default ok");
    assert!(v.get("outputs_dir").is_some());
    // The three registries are always present (only `noop` when
    // the active config selects `provider = "none"` for that
    // modality, which is the kernel-default state); each row
    // carries {name, configured}.
    for slot in ["tts", "stt", "imagegen"] {
        let block = v.get(slot).unwrap_or_else(|| panic!("missing {slot}"));
        let providers = block
            .get("providers")
            .and_then(|p| p.as_array())
            .unwrap_or_else(|| panic!("{slot}.providers not an array"));
        assert!(!providers.is_empty(), "{slot} has zero providers");
        let first = &providers[0];
        assert!(first.get("name").is_some());
        assert!(first.get("configured").is_some());
    }
}

#[test]
fn media_providers_default_includes_noop_in_each_registry() {
    let v = media_cmd(&["providers".into()]).expect("providers ok");
    for slot in ["tts", "stt", "imagegen"] {
        let names: Vec<String> = v
            .get(slot)
            .and_then(|s| s.get("providers"))
            .and_then(|p| p.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            names.contains(&"noop".to_string()),
            "{slot} missing noop, got: {names:?}"
        );
    }
}

#[test]
fn media_outputs_dir_returns_path() {
    let v = media_cmd(&["outputs-dir".into()]).expect("outputs-dir ok");
    let p = v.get("path").and_then(|s| s.as_str()).expect("path field");
    assert!(p.contains("media"), "expected 'media' in path, got: {p}");
}

#[test]
fn media_list_outputs_limit_requires_int() {
    let err = media_cmd(&["list-outputs".into(), "--limit".into(), "abc".into()]).unwrap_err();
    assert!(err.contains("--limit"));
}

#[test]
fn media_list_outputs_missing_dir_returns_empty() {
    let dir = std::env::temp_dir().join(format!(
        "cos-media-list-missing-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let v = list_media_outputs(&dir, 10, None).expect("list ok");
    assert_eq!(v.get("exists").and_then(|b| b.as_bool()), Some(false));
    assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(0));
    assert_eq!(
        v.get("files").and_then(|a| a.as_array()).map(|a| a.len()),
        Some(0)
    );
}

#[test]
fn media_list_outputs_returns_files_newest_first_within_limit() {
    let dir =
        std::env::temp_dir().join(format!("cos-media-list-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("mkdir");
    // Write files with sleeps between writes so mtime ordering
    // is deterministic across Windows / Linux / macOS without
    // pulling a fresh `filetime` dep into the workspace.
    for (name, body) in [("a.png", "1"), ("b.png", "22"), ("c.wav", "333")] {
        std::fs::write(dir.join(name), body).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let v = list_media_outputs(&dir, 10, None).expect("list ok");
    assert_eq!(v.get("n").and_then(|n| n.as_u64()), Some(3));
    let files = v.get("files").and_then(|a| a.as_array()).unwrap();
    let names: Vec<&str> = files
        .iter()
        .filter_map(|f| f.get("name").and_then(|s| s.as_str()))
        .collect();
    assert_eq!(names, vec!["c.wav", "b.png", "a.png"]);
    // Filtering by ext narrows the list.
    let v2 = list_media_outputs(&dir, 10, Some("png")).expect("list png ok");
    let names2: Vec<&str> = v2
        .get("files")
        .and_then(|a| a.as_array())
        .unwrap()
        .iter()
        .filter_map(|f| f.get("name").and_then(|s| s.as_str()))
        .collect();
    assert_eq!(names2, vec!["b.png", "a.png"]);
    // Limit caps the result.
    let v3 = list_media_outputs(&dir, 1, None).expect("list lim ok");
    assert_eq!(v3.get("n").and_then(|n| n.as_u64()), Some(1));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn binary_ext_default_lists_with_capped_limit() {
    let v = binary_ext_cmd(&[]).expect("default ok");
    let n = v.get("n").and_then(|n| n.as_u64()).expect("n");
    let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
    assert!(n <= 50, "default limit should cap at 50, got {n}");
    assert!(total >= n, "total ({total}) must be >= shown ({n})");
    assert!(v.get("extensions").is_some());
}

#[test]
fn binary_ext_no_limit_returns_all() {
    let v = binary_ext_cmd(&["list".into(), "--no-limit".into()]).expect("no-limit ok");
    let n = v.get("n").and_then(|n| n.as_u64()).expect("n");
    let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
    assert_eq!(n, total);
}

#[test]
fn binary_ext_list_limit_requires_int() {
    let err = binary_ext_cmd(&["list".into(), "--limit".into(), "abc".into()]).unwrap_err();
    assert!(err.contains("--limit"));
}

#[test]
fn binary_ext_extensions_returns_all_unbounded() {
    let v = binary_ext_cmd(&["extensions".into()]).expect("extensions ok");
    let total = v.get("total").and_then(|n| n.as_u64()).expect("total");
    let len = v
        .get("extensions")
        .and_then(|a| a.as_array())
        .map(|a| a.len() as u64)
        .expect("extensions array");
    assert_eq!(total, len);
}

#[test]
fn binary_ext_check_recognises_path_with_known_ext() {
    let v = binary_ext_cmd(&["check".into(), "C:\\Users\\me\\image.PNG".into()]).expect("check ok");
    assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("path"));
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(true));
    assert_eq!(v.get("extension").and_then(|s| s.as_str()), Some("png"));
}

#[test]
fn binary_ext_check_recognises_text_path_as_not_binary() {
    let v = binary_ext_cmd(&["check".into(), "/etc/passwd".into()]).expect("check ok");
    assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("path"));
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(false));
}

#[test]
fn binary_ext_check_extension_only_input_uses_extension_mode() {
    let v = binary_ext_cmd(&["check".into(), ".gguf".into()]).expect("check ok");
    assert_eq!(v.get("mode").and_then(|s| s.as_str()), Some("extension"));
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(true));
    assert_eq!(v.get("extension").and_then(|s| s.as_str()), Some("gguf"));

    let v2 = binary_ext_cmd(&["check".into(), "exe".into()]).expect("check ok2");
    assert_eq!(v2.get("mode").and_then(|s| s.as_str()), Some("extension"));
    assert_eq!(v2.get("is_binary").and_then(|b| b.as_bool()), Some(true));
}

#[test]
fn binary_ext_check_unknown_extension_returns_false() {
    let v = binary_ext_cmd(&["check".into(), "logfile.unknown".into()]).expect("check ok");
    assert_eq!(v.get("is_binary").and_then(|b| b.as_bool()), Some(false));
}

// ---- context_cmd dispatch ----

// ---- context hints ----

#[test]
fn context_hints_invalid_cwd_errs() {
    let err =
        context_hints_cmd(&["--cwd".into(), "Z:\\definitely\\not\\there".into()]).unwrap_err();
    assert!(err.contains("not a directory"));
}

#[test]
fn context_hints_finds_real_markers_in_temp_dir() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-hints-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    let v = context_hints_cmd(&["--cwd".into(), dir.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(1));
    let hints = v.get("hints").and_then(|h| h.as_array()).unwrap();
    assert!(hints
        .iter()
        .any(|h| h.get("label").and_then(|s| s.as_str()) == Some("Rust crate")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_hints_render_returns_summary_paragraph() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-hints-render-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("package.json"), "{}").unwrap();
    let v = context_hints_cmd(&[
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
        "--render".into(),
    ])
    .expect("ok");
    let s = v.get("summary").and_then(|s| s.as_str()).unwrap_or("");
    assert!(s.contains("Project hints"));
    assert!(s.contains("Node.js project"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_hints_recursive_with_depth() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-hints-deep-{}",
        uuid::Uuid::new_v4().simple()
    ));
    let nested = dir.join("apps").join("web");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("package.json"), "{}").unwrap();
    // Depth 0 → no recursion → no hits.
    let v0 = context_hints_cmd(&[
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
        "--depth".into(),
        "0".into(),
    ])
    .expect("ok");
    assert_eq!(v0.get("count").and_then(|n| n.as_u64()), Some(0));
    // Depth 3 → recursive walk → finds the nested manifest.
    let v3 = context_hints_cmd(&[
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
        "--depth".into(),
        "3".into(),
    ])
    .expect("ok");
    assert_eq!(v3.get("count").and_then(|n| n.as_u64()), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- context refs ----

#[test]
fn context_refs_requires_text() {
    let err = context_refs_cmd(&[]).unwrap_err();
    assert!(err.contains("--text"));
}

#[test]
fn context_refs_extracts_paths_and_urls() {
    let v = context_refs_cmd(&[
        "--text".into(),
        "see @notes.md and @https://example.com/x".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(2));
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs[0].get("kind").and_then(|s| s.as_str()), Some("Path"));
    assert_eq!(
        refs[0].get("raw").and_then(|s| s.as_str()),
        Some("notes.md")
    );
    assert_eq!(refs[1].get("kind").and_then(|s| s.as_str()), Some("Url"));
}

#[test]
fn context_refs_unique_dedupes() {
    let v = context_refs_cmd(&["--text".into(), "@a @a @a".into(), "--unique".into()]).expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(v.get("unique").and_then(|b| b.as_bool()), Some(true));
}

// ---- context markers ----

#[test]
fn context_markers_dumps_table() {
    let v = context_markers_cmd(&[]).expect("ok");
    let total = v.get("total").and_then(|n| n.as_u64()).unwrap();
    assert!(total >= 30);
    let by_kind = v.get("by_kind").and_then(|x| x.as_object()).unwrap();
    let manifests = by_kind.get("Manifest").and_then(|x| x.as_array()).unwrap();
    let names: Vec<&str> = manifests.iter().filter_map(|s| s.as_str()).collect();
    assert!(names.contains(&"Cargo.toml"));
    assert!(names.contains(&"package.json"));
    assert!(names.contains(&"go.mod"));
}

// ---- context build (engine) ----

#[test]
fn context_build_no_args_returns_empty_block() {
    let v = context_cmd(&["build".into()]).expect("ok");
    assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(true));
    assert!(v.get("rendered").map(|x| x.is_null()).unwrap_or(false));
}

#[test]
fn context_build_invalid_cwd_errs() {
    let err = context_cmd(&[
        "build".into(),
        "--cwd".into(),
        "Z:\\definitely\\not\\there".into(),
    ])
    .unwrap_err();
    assert!(err.contains("not a directory"));
}

#[test]
fn context_build_invalid_depth_errs() {
    let err = context_cmd(&["build".into(), "--depth".into(), "abc".into()]).unwrap_err();
    assert!(err.contains("--depth"));
}

#[test]
fn context_build_with_text_extracts_references() {
    let v =
        context_cmd(&["build".into(), "--text".into(), "look at @notes.md".into()]).expect("ok");
    assert_eq!(v.get("is_empty").and_then(|b| b.as_bool()), Some(false));
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs.len(), 1);
    let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
    assert!(rendered.contains("PROJECT_CONTEXT"));
    assert!(rendered.contains("notes.md"));
}

#[test]
fn context_build_with_cwd_picks_up_hints() {
    let dir = std::env::temp_dir().join(format!(
        "cos-context-build-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
    let v = context_cmd(&[
        "build".into(),
        "--cwd".into(),
        dir.to_string_lossy().to_string(),
    ])
    .expect("ok");
    let hints = v.get("hints").and_then(|x| x.as_array()).unwrap();
    assert_eq!(hints.len(), 1);
    assert_eq!(
        hints[0].get("label").and_then(|s| s.as_str()),
        Some("Rust crate")
    );
    let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
    assert!(rendered.contains("Project hints"));
    assert!(rendered.contains("cwd:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn context_build_with_notes_appends_them() {
    let v = context_cmd(&[
        "build".into(),
        "--note".into(),
        "host: Windows".into(),
        "--note".into(),
        "12 MB free".into(),
    ])
    .expect("ok");
    let notes = v.get("notes").and_then(|x| x.as_array()).unwrap();
    assert_eq!(notes.len(), 2);
    let rendered = v.get("rendered").and_then(|s| s.as_str()).unwrap_or("");
    assert!(rendered.contains("Notes:"));
    assert!(rendered.contains("host: Windows"));
    assert!(rendered.contains("12 MB free"));
}

#[test]
fn context_build_max_refs_caps_count() {
    let v = context_cmd(&[
        "build".into(),
        "--text".into(),
        "@a @b @c @d @e".into(),
        "--max-refs".into(),
        "2".into(),
    ])
    .expect("ok");
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs.len(), 2);
}

#[test]
fn context_build_no_dedup_keeps_duplicates() {
    let v = context_cmd(&[
        "build".into(),
        "--text".into(),
        "@a @a @a".into(),
        "--no-dedup".into(),
    ])
    .expect("ok");
    let refs = v.get("references").and_then(|x| x.as_array()).unwrap();
    assert_eq!(refs.len(), 3);
}

// ---- file-safety dispatch ----

#[test]
fn file_safety_check_rejects_multiple_paths() {
    let err = file_safety_cmd(&["check".into(), "a".into(), "b".into()]).unwrap_err();
    assert!(err.contains("single path"));
}

#[test]
fn file_safety_check_allows_normal_file() {
    let v = file_safety_cmd(&["check".into(), "/home/user/project/main.rs".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("allow"));
    assert!(v.get("category").and_then(|c| c.as_str()).is_none());
}

#[test]
fn file_safety_check_denies_credential_dir() {
    let v = file_safety_cmd(&["check".into(), "/home/user/.ssh/id_rsa".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert_eq!(
        v.get("category").and_then(|c| c.as_str()),
        Some("credential")
    );
}

#[test]
fn file_safety_check_denies_dangerous_extension() {
    let v = file_safety_cmd(&["check".into(), "/tmp/payload.exe".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("deny"));
    assert_eq!(
        v.get("category").and_then(|c| c.as_str()),
        Some("dangerous_extension")
    );
}

#[test]
fn file_safety_check_caution_for_shell_script() {
    let v = file_safety_cmd(&["check".into(), "/home/user/run.sh".into()]).expect("ok");
    assert_eq!(v.get("verdict").and_then(|s| s.as_str()), Some("caution"));
}

#[test]
fn file_safety_batch_aggregates_summary() {
    let v = file_safety_cmd(&[
        "batch".into(),
        "/home/user/main.rs".into(),
        "/etc/passwd".into(),
        "/home/user/run.sh".into(),
    ])
    .expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(3));
    let summary = v.get("summary").and_then(|x| x.as_object()).unwrap();
    assert_eq!(summary.get("allow").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(summary.get("caution").and_then(|n| n.as_u64()), Some(1));
    assert_eq!(summary.get("deny").and_then(|n| n.as_u64()), Some(1));
}

#[test]
fn file_safety_batch_requires_at_least_one_path() {
    let err = file_safety_cmd(&["batch".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn file_safety_categories_lists_known_categories() {
    let v = file_safety_cmd(&["categories".into()]).expect("ok");
    let cats = v.get("categories").and_then(|c| c.as_array()).unwrap();
    let names: Vec<&str> = cats.iter().filter_map(|c| c.as_str()).collect();
    assert!(names.contains(&"dangerous_extension"));
    assert!(names.contains(&"credential"));
    assert!(names.contains(&"system_directory"));
    assert!(names.contains(&"vcs_internal"));
    let verdicts = v.get("verdicts").and_then(|x| x.as_array()).unwrap();
    let vs: Vec<&str> = verdicts.iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(vs, vec!["allow", "caution", "deny"]);
}

// ---- osv dispatch (no network) ----

#[test]
fn osv_ecosystems_lists_known_ecosystems() {
    let v = osv_cmd(&["ecosystems".into()]).expect("ok");
    let eco = v.get("ecosystems").and_then(|x| x.as_array()).unwrap();
    let names: Vec<&str> = eco.iter().filter_map(|s| s.as_str()).collect();
    assert!(names.contains(&"crates.io"));
    assert!(names.contains(&"npm"));
    assert!(names.contains(&"PyPI"));
    assert!(names.contains(&"Go"));
    let lockfiles = v.get("lockfiles").and_then(|x| x.as_array()).unwrap();
    let ls: Vec<&str> = lockfiles.iter().filter_map(|s| s.as_str()).collect();
    assert!(ls.contains(&"Cargo.lock"));
    assert!(ls.contains(&"go.sum"));
}

#[test]
fn osv_parse_rejects_extra_args() {
    let err = osv_cmd(&["parse".into(), "a".into(), "b".into()]).unwrap_err();
    assert!(err.contains("single"));
}

#[test]
fn osv_parse_reads_cargo_lock() {
    let dir = std::env::temp_dir().join(format!("cos-osv-parse-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let lock_path = dir.join("Cargo.lock");
    std::fs::write(
        &lock_path,
        "[[package]]\nname = \"foo\"\nversion = \"1.2.3\"\n\n[[package]]\nname = \"bar\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let v = osv_cmd(&["parse".into(), lock_path.to_string_lossy().to_string()]).expect("ok");
    assert_eq!(v.get("count").and_then(|n| n.as_u64()), Some(2));
    let pkgs = v.get("packages").and_then(|x| x.as_array()).unwrap();
    let names: Vec<&str> = pkgs
        .iter()
        .filter_map(|p| p.get("name").and_then(|s| s.as_str()))
        .collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    for p in pkgs {
        assert_eq!(
            p.get("ecosystem").and_then(|s| s.as_str()),
            Some("crates.io")
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn osv_parse_unknown_lockfile_errs() {
    let dir = std::env::temp_dir().join(format!("cos-osv-bad-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("Pipfile.lock");
    std::fs::write(&p, "{}").unwrap();
    let err = osv_cmd(&["parse".into(), p.to_string_lossy().to_string()]).unwrap_err();
    assert!(err.contains("unknown lockfile"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn osv_query_requires_coord() {
    let err = osv_cmd(&["query".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn osv_query_requires_ecosystem_flag() {
    let err = osv_cmd(&["query".into(), "lodash@1.0.0".into()]).unwrap_err();
    assert!(err.contains("--ecosystem"));
}

#[test]
fn osv_query_rejects_malformed_coord() {
    let err = osv_cmd(&[
        "query".into(),
        "no-version".into(),
        "--ecosystem".into(),
        "npm".into(),
    ])
    .unwrap_err();
    assert!(err.contains("name>@<version"));
}

// ---- stream / live async helpers ------------------------------------

/// Build a mock provider with a scripted text response and run
/// `stream_cmd_async` against it. Returns the JSON envelope.
fn run_stream_async(
    text: &str,
    cfg: &crate::config::AgentConfig,
    prompt: &str,
) -> serde_json::Value {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let mock = MockProvider::new(&cfg.model, cfg);
    mock.push_response(MockResponse::Text(text.to_string()));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(stream_cmd_async(provider, cfg, prompt))
        .expect("stream ok")
}

#[test]
fn ask_rejects_empty_prompt() {
    let err = run("ask", &[]).unwrap_err();
    assert!(err.to_lowercase().contains("usage"), "got {err}");
    // Usage hint must document the remaining flag the handler accepts.
    assert!(
        err.contains("--full"),
        "usage hint should mention --full: {err}"
    );
    assert!(
        !err.contains("--stream"),
        "removed flag leaked into usage: {err}"
    );
    let err2 = run("ask", &["".into()]).unwrap_err();
    assert!(err2.to_lowercase().contains("usage"), "got {err2}");
}

#[test]
fn ask_flag_alone_is_not_treated_as_prompt() {
    // Regression: feeding only flags must surface the usage hint
    // rather than silently using a flag string ("--full") as the
    // prompt — which would route to clawd and either error
    // opaquely or actually consume LLM tokens.
    let err = run("ask", &["--full".into()]).unwrap_err();
    assert!(err.contains("usage:"), "got {err}");
}

#[test]
fn ask_session_requires_non_empty_id() {
    let err = run("ask", &["--session".into()]).unwrap_err();
    assert!(err.contains("--session"), "got {err}");
    let err = run("ask", &["--session".into(), "".into(), "hi".into()]).unwrap_err();
    assert!(err.contains("non-empty"), "got {err}");
}

#[test]
fn ask_timeout_requires_positive_integer() {
    for value in ["", "0", "nope"] {
        let err = run("ask", &["--timeout-secs".into(), value.into(), "hi".into()]).unwrap_err();
        assert!(err.contains("positive integer"), "got {err}");
    }
}

#[test]
fn stream_async_accumulates_text_and_returns_envelope() {
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let v = run_stream_async("hello world", &cfg, "say hi");
    assert_eq!(
        v.get("answer").and_then(|a| a.as_str()),
        Some("hello world")
    );
    assert_eq!(v.get("provider").and_then(|p| p.as_str()), Some("mock"));
    assert_eq!(v.get("model").and_then(|m| m.as_str()), Some("mock-model"));
    // mock's chat_stream emits Message + Done; finish_reason for
    // a plain text reply is FinishReason::Stop.
    assert_eq!(v.get("finish").and_then(|f| f.as_str()), Some("Stop"));
    assert!(v.get("tool_calls").unwrap().as_array().unwrap().is_empty());
}

#[test]
fn stream_async_surfaces_tool_calls() {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    use crate::agent::llm::types::ToolCall;
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::ToolUse(vec![ToolCall {
        id: "call_1".into(),
        name: "echo".into(),
        input: serde_json::json!({"text": "hi"}),
    }]));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let v = rt
        .block_on(stream_cmd_async(provider, &cfg, "use a tool"))
        .expect("stream ok");
    let calls = v.get("tool_calls").unwrap().as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(calls[0]["name"], "echo");
    // mock emits ToolUse via Message variant → finish ToolUse.
    assert_eq!(v.get("finish").and_then(|f| f.as_str()), Some("ToolUse"));
}

#[test]
fn stream_async_propagates_provider_error() {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::Error(llm::LlmError::Auth));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(stream_cmd_async(provider, &cfg, "hi"))
        .unwrap_err();
    assert!(
        err.contains("chat_stream") || err.contains("auth"),
        "want chat_stream/auth in err, got {err}"
    );
}

#[test]
fn stream_async_envelope_includes_usage_keys() {
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let v = run_stream_async("ok", &cfg, "ping");
    let usage = v.get("usage").unwrap();
    assert!(usage.get("input_tokens").is_some());
    assert!(usage.get("output_tokens").is_some());
    assert!(usage.get("cache_read_tokens").is_some());
    assert!(usage.get("cache_write_tokens").is_some());
}

async fn live_cmd_async(
    provider: std::sync::Arc<dyn llm::Provider>,
    cfg: &crate::config::AgentConfig,
    user_prompt: &str,
) -> Result<Value, String> {
    use crate::agent::llm::accumulate::StreamSink;
    use crate::agent::llm::types::StreamEvent;
    use std::collections::HashSet;
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    let mut tools = crate::agent::tools::registry::default_registry();
    let mut exposure = crate::agent::tools::exposure::ToolExposureContext::isolated(
        runtime::loop_::guardrails_from_cfg(cfg),
    );
    tools.set_approval(runtime::loop_::approval_from_cfg(cfg));
    let _mcp_handles =
        runtime::loop_::attach_mcp_servers_for_cli(&mut tools, cfg, &mut exposure).await;

    struct LiveSink {
        tool_calls: Mutex<Vec<serde_json::Value>>,
        announced_tools: Mutex<HashSet<String>>,
        warnings: Mutex<Vec<String>>,
        last_usage: Mutex<Option<crate::agent::llm::types::Usage>>,
        last_finish: Mutex<Option<crate::agent::llm::types::FinishReason>>,
        heartbeat: crate::agent::runtime::progress::Heartbeat,
    }

    impl LiveSink {
        fn announce_tool(&self, id: &str, name: &str, out: &mut impl Write) {
            let should_announce =
                id.is_empty() || mlock(&self.announced_tools).insert(id.to_string());
            if should_announce {
                let _ = writeln!(out, "\n[tool: {name}]");
            }
        }
    }

    impl StreamSink for LiveSink {
        fn on_event(&self, event: &StreamEvent) {
            let stderr = std::io::stderr();
            let mut err_lock = stderr.lock();
            match event {
                StreamEvent::TextDelta { text } => {
                    let _ = err_lock.write_all(text.as_bytes());
                    let _ = err_lock.flush();
                }
                StreamEvent::ToolUseStart { id, name } => {
                    self.announce_tool(id, name, &mut err_lock);
                }
                StreamEvent::ToolInputDelta { .. } => {}
                StreamEvent::ToolUse(call) => {
                    self.announce_tool(&call.id, &call.name, &mut err_lock);
                    mlock(&self.tool_calls).push(serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                    }));
                }
                StreamEvent::Reasoning { .. } => {}
                StreamEvent::ToolState { .. } => {}
                StreamEvent::Message(resp) => {
                    for block in &resp.content {
                        if let crate::agent::llm::types::ContentBlock::Text { text } = block {
                            let _ = err_lock.write_all(text.as_bytes());
                        }
                    }
                    for call in &resp.tool_calls {
                        self.announce_tool(&call.id, &call.name, &mut err_lock);
                        mlock(&self.tool_calls).push(serde_json::json!({
                            "id": call.id,
                            "name": call.name,
                        }));
                    }
                    let _ = err_lock.flush();
                }
                StreamEvent::Done { finish, usage } => {
                    let _ = writeln!(err_lock, "\n[turn done finish={finish:?}]");
                    *mlock(&self.last_usage) = Some(usage.clone());
                    *mlock(&self.last_finish) = Some(*finish);
                }
                StreamEvent::Warning { message } => {
                    let _ = writeln!(err_lock, "\n[warning] {message}");
                    mlock(&self.warnings).push(message.clone());
                }
            }
        }
    }

    impl crate::agent::runtime::progress::ProgressSink for LiveSink {
        fn on_tool_start(&self, id: &str, name: &str, _input: &serde_json::Value) {
            self.announce_tool(id, name, &mut std::io::stderr().lock());
            self.heartbeat.start(id, "");
        }

        fn on_tool_result(
            &self,
            id: &str,
            name: &str,
            ok: bool,
            _latency_ms: u64,
            _bytes_returned: usize,
            _content_preview: &str,
        ) {
            self.heartbeat.stop(id);
            if !ok {
                let _ = writeln!(std::io::stderr().lock(), "\n[tool failed: {name}]");
            }
        }
    }

    let sink_obj = Arc::new(LiveSink {
        tool_calls: Mutex::new(Vec::new()),
        announced_tools: Mutex::new(HashSet::new()),
        warnings: Mutex::new(Vec::new()),
        last_usage: Mutex::new(None),
        last_finish: Mutex::new(None),
        heartbeat: crate::agent::runtime::progress::Heartbeat::new(),
    });
    let sink: Arc<dyn StreamSink> = sink_obj.clone();
    let progress: Arc<dyn crate::agent::runtime::progress::ProgressSink> = sink_obj.clone();

    let result = match memory::sqlite_fts::MemoryDb::open_default() {
        Ok(db) => {
            let session_id = uuid::Uuid::new_v4().to_string();
            runtime::loop_::ask_with_stream(
                provider.clone(),
                cfg,
                user_prompt,
                &tools,
                Some((&db, session_id.as_str())),
                sink,
                progress,
            )
            .await
        }
        Err(e) => {
            tracing::warn!(
                "memory: default DB unavailable ({e}); running without history recording"
            );
            runtime::loop_::ask_with_stream(
                provider.clone(),
                cfg,
                user_prompt,
                &tools,
                None,
                sink,
                progress,
            )
            .await
        }
    };

    match result {
        Ok(ask_result) => {
            let usage = mlock(&sink_obj.last_usage).clone().unwrap_or_default();
            let finish = mlock(&sink_obj.last_finish).take();
            Ok(json!({
                "answer": ask_result.answer,
                "evidence": ask_result.evidence,
                "fallback": ask_result.fallback,
                "turns": ask_result.turns,
                "provider": ask_result.provider,
                "model": ask_result.model,
                "session_id": ask_result.session_id,
                "tool_calls": *mlock(&sink_obj.tool_calls),
                "warnings": *mlock(&sink_obj.warnings),
                "finish": finish.map(|f| format!("{f:?}")),
                "usage": {
                    "input_tokens": usage.input_tokens,
                    "output_tokens": usage.output_tokens,
                    "cache_read_tokens": usage.cache_read_tokens,
                    "cache_write_tokens": usage.cache_write_tokens,
                },
            }))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Helper for `cos agent live` integration tests. Mirrors the
/// `run_stream_async` helper above but routes through the new
/// multi-turn streaming path.
fn run_live_async(
    responses: &[(&str, Option<Vec<llm::types::ToolCall>>)],
    cfg: &crate::config::AgentConfig,
    prompt: &str,
) -> Value {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let mock = MockProvider::new(&cfg.model, cfg);
    for (text, tool_calls) in responses {
        match tool_calls {
            Some(calls) if !calls.is_empty() => {
                mock.push_response(MockResponse::ToolUse(calls.clone()));
            }
            _ => {
                mock.push_response(MockResponse::Text((*text).into()));
            }
        }
    }
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(live_cmd_async(provider, cfg, prompt))
        .expect("live ok")
}

#[test]
fn live_async_returns_text_envelope() {
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    // Disable memory recording for this test to keep it
    // hermetic — the temp data_dir scaffolding from
    // env-overrides is intentionally not set up here, so the
    // open_default() may fall back to no-recording mode anyway.
    let v = run_live_async(&[("hello world", None)], &cfg, "say hello");
    assert_eq!(v["answer"].as_str(), Some("hello world"));
    assert!(v["session_id"].as_str().unwrap().len() > 0);
    assert_eq!(v["provider"].as_str(), Some("mock"));
    assert_eq!(v["model"].as_str(), Some("mock-model"));
    // Text-only run: no tool calls.
    assert_eq!(v["tool_calls"].as_array().unwrap().len(), 0);
    // Mock emits Text via Message → Done with Stop finish.
    assert_eq!(v["finish"].as_str(), Some("Stop"));
    let usage = v.get("usage").unwrap();
    assert!(usage.get("input_tokens").is_some());
}

#[test]
fn live_async_records_tool_call_pair() {
    use crate::agent::llm::types::ToolCall;
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    cfg.max_turns = 2; // tool-call → echo result → final text
    let v = run_live_async(
        &[
            (
                "",
                Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "echo".into(),
                    input: serde_json::json!({"text": "abc"}),
                }]),
            ),
            ("done", None),
        ],
        &cfg,
        "echo abc",
    );
    // Streaming sink records the tool_use event.
    let calls = v["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "call_1");
    assert_eq!(calls[0]["name"], "echo");
    // Final answer comes from the second turn's Text response.
    assert_eq!(v["answer"].as_str(), Some("done"));
    assert!(v["turns"].as_u64().unwrap() >= 2);
}

#[test]
fn live_async_propagates_provider_error() {
    use crate::agent::llm::providers::mock::{MockProvider, MockResponse};
    let mut cfg = crate::config::AgentConfig::default();
    cfg.provider = "mock".into();
    cfg.model = "mock-model".into();
    let mock = MockProvider::new(&cfg.model, &cfg);
    mock.push_response(MockResponse::Error(llm::LlmError::Auth));
    let provider: std::sync::Arc<dyn llm::Provider> = std::sync::Arc::new(mock);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let err = rt
        .block_on(live_cmd_async(provider, &cfg, "hi"))
        .unwrap_err();
    // AgentError::Llm wraps the provider error; the formatter
    // includes either "auth" or the provider-error prefix.
    assert!(
        err.to_lowercase().contains("auth")
            || err.to_lowercase().contains("llm")
            || err.to_lowercase().contains("provider"),
        "want auth/llm/provider in err, got {err}"
    );
}

#[test]
fn chat_cmd_max_turns_flag_rejects_non_numeric() {
    let err = chat_cmd(&["--max-turns".into(), "lots".into()]).unwrap_err();
    assert!(err.contains("--max-turns"), "got {err}");
}

#[test]
fn chat_routed_through_run() {
    // Confirm the dispatcher in `run()` reaches `chat_cmd`. Pass an
    // unknown flag so we get a deterministic error without trying
    // to read stdin.
    let err = run("chat", &["--definitely-not-real".into()]).unwrap_err();
    assert!(err.to_lowercase().contains("unknown flag"), "got {err}");
}

// -----------------------------------------------------------------
// interrupt_cmd
// -----------------------------------------------------------------

#[test]
fn interrupt_cmd_default_errs_with_usage() {
    let err = interrupt_cmd(&[]).unwrap_err();
    assert!(err.contains("interrupt"), "got {err}");
    assert!(err.contains("list"), "got {err}");
    assert!(err.contains("signal"), "got {err}");
}

#[test]
fn interrupt_cmd_list_returns_active_sessions() {
    let id = format!("cli-list-{}", uuid::Uuid::new_v4().simple());
    let _h = crate::agent::runtime::interrupt::register(&id);
    let v = interrupt_cmd(&["list".into()]).expect("list ok");
    let arr = v["sessions"].as_array().expect("sessions array");
    let ids: Vec<&str> = arr.iter().filter_map(|s| s.as_str()).collect();
    assert!(ids.contains(&id.as_str()), "list missing {id}: {arr:?}");
    assert!(v["count"].as_u64().unwrap() >= 1);
}

#[test]
fn interrupt_cmd_signal_unknown_session_reports_not_registered() {
    let id = format!("cli-unknown-{}", uuid::Uuid::new_v4().simple());
    let v = interrupt_cmd(&["signal".into(), id.clone()]).expect("ok");
    assert_eq!(v["signaled"], serde_json::Value::Bool(false));
    assert_eq!(v["session_id"].as_str().unwrap(), id);
    assert!(v["reason"].as_str().unwrap().contains("not registered"));
}

#[test]
fn interrupt_cmd_signal_active_session_returns_signaled_true() {
    let id = format!("cli-signal-{}", uuid::Uuid::new_v4().simple());
    let h = crate::agent::runtime::interrupt::register(&id);
    let v = interrupt_cmd(&["signal".into(), id.clone()]).expect("ok");
    assert_eq!(v["signaled"], serde_json::Value::Bool(true));
    assert_eq!(v["session_id"].as_str().unwrap(), id);
    // Signal really took effect.
    assert!(h.check());
}

#[test]
fn interrupt_cmd_signal_requires_session_id() {
    let err = interrupt_cmd(&["signal".into()]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn run_interrupt_routes_to_interrupt_cmd() {
    // Confirm the agent dispatcher reaches interrupt_cmd.
    let err = run("interrupt", &["frobnicate".into()]).unwrap_err();
    assert!(err.contains("unknown"), "got {err}");
}

// -----------------------------------------------------------------
// learn (memory curator CLI)
// -----------------------------------------------------------------

/// Pin the curator default log under a per-test temp dir so we
/// don't trample the real machine's `%ProgramData%\cos\` state.
/// Returns a guard that holds the crate-wide env lock for the
/// test's lifetime: each call mutates `COS_DATA_DIR`, and two
/// tests running in parallel would otherwise observe each
/// other's data directory (cargo test runs many threads).
/// The guard derefs to `&Path` so existing `dir.join(...)`
/// callers keep working without changes.
struct LearnDataDir {
    path: std::path::PathBuf,
    _env: std::sync::MutexGuard<'static, ()>,
}

impl std::ops::Deref for LearnDataDir {
    type Target = std::path::Path;
    fn deref(&self) -> &Self::Target {
        &self.path
    }
}

impl LearnDataDir {
    fn join(&self, p: impl AsRef<std::path::Path>) -> std::path::PathBuf {
        self.path.join(p)
    }
}

fn isolate_cos_data_dir(tag: &str) -> LearnDataDir {
    let env = crate::test_env::lock_env();
    let dir = std::env::temp_dir().join(format!(
        "cos-learn-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("COS_DATA_DIR", &dir);
    LearnDataDir {
        path: dir,
        _env: env,
    }
}

#[test]
fn learn_cmd_extract_requires_session_flag() {
    let _dir = isolate_cos_data_dir("missing-session");
    let err = learn_cmd(&["extract".into()]).unwrap_err();
    assert!(err.contains("--session"), "got {err}");
}

#[test]
fn learn_cmd_extract_min_confidence_out_of_range_errs() {
    let err = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "s".into(),
        "--min-confidence".into(),
        "1.5".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--min-confidence"), "got {err}");
}

#[test]
fn learn_cmd_extract_min_confidence_not_float_errs() {
    let err = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "s".into(),
        "--min-confidence".into(),
        "abc".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--min-confidence"), "got {err}");
}

#[test]
fn learn_cmd_extract_limit_not_integer_errs() {
    let err = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "s".into(),
        "--limit".into(),
        "abc".into(),
    ])
    .unwrap_err();
    assert!(err.contains("--limit"), "got {err}");
}

#[test]
fn learn_cmd_extract_dry_run_with_unknown_session_succeeds() {
    // dry-run skips both LLM and dedupe, and an unknown session
    // simply has zero recent messages — should be a clean
    // success envelope with empty facts.
    let _dir = isolate_cos_data_dir("dry-run-empty");
    let v = learn_cmd(&[
        "extract".into(),
        "--session".into(),
        "no-such-session".into(),
        "--dry-run".into(),
    ])
    .expect("dry-run should not fail");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    assert_eq!(v["dry_run"], serde_json::Value::Bool(true));
    assert_eq!(v["messages_examined"], serde_json::json!(0));
    assert!(
        v["facts_proposed"].as_array().unwrap().is_empty(),
        "got {v}"
    );
}

#[test]
fn learn_cmd_status_default_is_empty_when_log_missing() {
    let _dir = isolate_cos_data_dir("status-empty");
    let v = learn_cmd(&["status".into()]).expect("ok");
    assert_eq!(v["session_count"], serde_json::json!(0));
    assert_eq!(v["log_exists"], serde_json::Value::Bool(false));
}

#[test]
fn learn_cmd_default_subcommand_is_status() {
    let _dir = isolate_cos_data_dir("status-default");
    let v = learn_cmd(&[]).expect("ok");
    assert!(v.get("session_count").is_some(), "got {v}");
}

#[test]
fn learn_cmd_clear_log_requires_session_or_all() {
    let _dir = isolate_cos_data_dir("clear-needs-flag");
    let err = learn_cmd(&["clear-log".into()]).unwrap_err();
    assert!(
        err.contains("--session") || err.contains("--all"),
        "got {err}"
    );
}

#[test]
fn learn_cmd_clear_log_all_writes_empty_log() {
    let dir = isolate_cos_data_dir("clear-all");
    let v = learn_cmd(&["clear-log".into(), "--all".into()]).expect("ok");
    assert_eq!(v["ok"], serde_json::Value::Bool(true));
    // log file is now created on disk under the isolated dir.
    let log = dir.join("agent").join("memory").join("curation_log.json");
    assert!(log.exists(), "expected {} to exist", log.display());
}

#[test]
fn learn_cmd_clear_log_for_unknown_session_reports_zero_removed() {
    let _dir = isolate_cos_data_dir("clear-unknown");
    let v = learn_cmd(&["clear-log".into(), "--session".into(), "ghost".into()]).expect("ok");
    assert_eq!(v["removed_entries"], serde_json::json!(0));
}

#[test]
fn learn_cmd_prompt_returns_embedded_default() {
    let v = learn_cmd(&["prompt".into()]).expect("ok");
    let s = v["system_prompt"].as_str().unwrap();
    assert!(s.contains("<fact"), "prompt should mention <fact tags");
    assert!(s.contains("category"));
}

#[test]
fn run_learn_routes_to_learn_cmd() {
    // dispatcher routing through `dev` namespace — using `prompt` because it's IO-free.
    let v = run("dev", &["learn".into(), "prompt".into()]).expect("ok");
    assert!(v.get("system_prompt").is_some(), "got {v}");
}

// -----------------------------------------------------------------
// hooks (runtime hook registry CLI)
// -----------------------------------------------------------------

#[test]
fn hooks_cmd_list_default_returns_count() {
    let _dir = isolate_cos_data_dir("hooks-list-default");
    let v = hooks_cmd(&[]).expect("ok");
    assert!(v.get("hooks").is_some(), "got {v}");
    assert!(v.get("count").is_some(), "got {v}");
    assert!(v["count"].is_number(), "got {v}");
    assert!(v["persistent"].is_array(), "got {v}");
    assert!(v["config_path"].is_string(), "got {v}");
}

#[test]
fn hooks_cmd_list_after_register_includes_name() {
    use crate::agent::runtime::hooks::{global_registry, Hook, HookContext, HookOutcome};
    let _dir = isolate_cos_data_dir("hooks-list-after-register");
    struct TestHook;
    impl Hook for TestHook {
        fn name(&self) -> &str {
            "cli-test-hook"
        }
        fn pre_turn(&self, _ctx: &HookContext) -> HookOutcome {
            HookOutcome::Continue
        }
    }
    let registry = global_registry();
    registry.register(std::sync::Arc::new(TestHook));
    let v = hooks_cmd(&["list".into()]).expect("ok");
    let names = v["hooks"].as_array().unwrap();
    assert!(
        names.iter().any(|n| n.as_str() == Some("cli-test-hook")),
        "got {v}"
    );
    // Cleanup so we don't leak the registration into other tests.
    registry.unregister("cli-test-hook");
}

#[test]
fn run_hooks_routes_to_hooks_cmd() {
    let _dir = isolate_cos_data_dir("hooks-route");
    let v = run("dev", &["hooks".into(), "list".into()]).expect("ok");
    assert!(v.get("count").is_some(), "got {v}");
}

#[test]
fn hooks_cmd_enable_persists_kind_and_registers_in_process() {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config;
    let _dir = isolate_cos_data_dir("hooks-enable");
    // make sure no leftover registration from a prior test
    global_registry().unregister("logging");

    let v = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["kind"], serde_json::json!("logging"));
    assert_eq!(v["persisted"], serde_json::json!(true));
    assert_eq!(v["registered_now"], serde_json::json!(true));

    // file exists with logging in enabled list
    let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
    assert_eq!(cfg.enabled, vec![hooks_config::HookKind::Logging]);

    // hook actually registered
    assert!(global_registry().names().contains(&"logging".to_string()));

    // cleanup
    global_registry().unregister("logging");
}

#[test]
fn hooks_cmd_enable_idempotent_second_call_is_noop() {
    use crate::agent::runtime::hooks::global_registry;
    let _dir = isolate_cos_data_dir("hooks-enable-idempotent");
    global_registry().unregister("logging");

    let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    let v = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["persisted"], serde_json::json!(false));
    assert_eq!(v["registered_now"], serde_json::json!(false));

    global_registry().unregister("logging");
}

#[test]
fn hooks_cmd_enable_accepts_kind_flag_form() {
    use crate::agent::runtime::hooks::global_registry;
    let _dir = isolate_cos_data_dir("hooks-enable-flag");
    global_registry().unregister("logging");

    let v = hooks_cmd(&["enable".into(), "--kind".into(), "logging".into()]).expect("ok");
    assert_eq!(v["kind"], serde_json::json!("logging"));

    global_registry().unregister("logging");
}

#[test]
fn hooks_cmd_enable_unknown_kind_errs() {
    let _dir = isolate_cos_data_dir("hooks-enable-unknown");
    let err = hooks_cmd(&["enable".into(), "frobnicate".into()]).unwrap_err();
    assert!(err.contains("unknown hook kind"), "got {err}");
}

#[test]
fn hooks_cmd_enable_missing_kind_errs() {
    let _dir = isolate_cos_data_dir("hooks-enable-missing");
    let err = hooks_cmd(&["enable".into()]).unwrap_err();
    assert!(err.contains("missing hook kind"), "got {err}");
}

#[test]
fn hooks_cmd_enable_checkpoint_kind_persists_and_registers() {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config;
    let _dir = isolate_cos_data_dir("hooks-enable-checkpoint");
    global_registry().unregister("checkpoint");

    let v = hooks_cmd(&["enable".into(), "checkpoint".into()]).expect("ok");
    assert_eq!(v["kind"], serde_json::json!("checkpoint"));
    assert_eq!(v["persisted"], serde_json::json!(true));
    assert_eq!(v["registered_now"], serde_json::json!(true));

    let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
    assert_eq!(cfg.enabled, vec![hooks_config::HookKind::Checkpoint]);
    assert!(global_registry()
        .names()
        .contains(&"checkpoint".to_string()));

    global_registry().unregister("checkpoint");
}

#[test]
fn hooks_cmd_disable_removes_from_config_and_registry() {
    use crate::agent::runtime::hooks::global_registry;
    use crate::agent::runtime::hooks_config;
    let _dir = isolate_cos_data_dir("hooks-disable");
    global_registry().unregister("logging");

    let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    let v = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["persisted"], serde_json::json!(true));
    assert_eq!(v["unregistered_now"], serde_json::json!(true));

    let cfg = hooks_config::load(&crate::paths::agent_hooks_path()).expect("load");
    assert!(cfg.enabled.is_empty());
    assert!(!global_registry().names().contains(&"logging".to_string()));
}

#[test]
fn hooks_cmd_disable_idempotent_when_not_enabled() {
    let _dir = isolate_cos_data_dir("hooks-disable-noop");
    let v = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
    assert_eq!(v["persisted"], serde_json::json!(false));
    assert_eq!(v["unregistered_now"], serde_json::json!(false));
}

#[test]
fn hooks_cmd_list_includes_persistent_kinds() {
    use crate::agent::runtime::hooks::global_registry;
    let _dir = isolate_cos_data_dir("hooks-list-persistent");
    global_registry().unregister("logging");

    let _ = hooks_cmd(&["enable".into(), "logging".into()]).expect("ok");
    let v = hooks_cmd(&["list".into()]).expect("ok");
    let pers = v["persistent"].as_array().unwrap();
    assert!(
        pers.iter().any(|x| x.as_str() == Some("logging")),
        "got {v}"
    );

    // cleanup
    let _ = hooks_cmd(&["disable".into(), "logging".into()]).expect("ok");
}

// -----------------------------------------------------------------
// media play / playback-status
// -----------------------------------------------------------------

#[test]
fn media_play_requires_a_path() {
    let err = media_play_cmd(&[]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn media_play_rejects_extra_positional_argument() {
    let err = media_play_cmd(&["a.wav".into(), "b.wav".into()]).unwrap_err();
    assert!(err.contains("unexpected extra"), "got {err}");
}

#[test]
fn media_play_detect_only_returns_format_and_player_for_wav() {
    // --detect doesn't try to play; it just resolves the format
    // and tells you which player would be used. Safe to run on
    // CI because nothing is dispatched.
    let v = media_play_cmd(&["--detect".into(), "foo.wav".into()]).expect("ok");
    assert_eq!(v["format"], serde_json::Value::String("wav".to_string()));
    assert_eq!(v["path"].as_str().unwrap(), "foo.wav");
    // `playable` is OS-dependent; just sanity-check it's bool.
    assert!(v["playable"].is_boolean(), "got {v}");
}

#[test]
fn media_play_detect_only_returns_null_format_for_unknown_extension() {
    let v = media_play_cmd(&["--detect".into(), "foo.txt".into()]).expect("ok");
    assert!(v["format"].is_null(), "got {v}");
    assert!(v["player"].is_null(), "got {v}");
    assert_eq!(v["playable"], serde_json::Value::Bool(false));
}

#[test]
fn media_play_real_dispatch_missing_file_errs() {
    let p = format!(
        "{}\\cos-media-play-test-missing-{}.wav",
        std::env::temp_dir().display(),
        uuid::Uuid::new_v4().simple()
    );
    let err = media_play_cmd(&[p.clone()]).unwrap_err();
    assert!(err.contains("playback failed"), "got {err}");
    assert!(
        err.contains("does not exist") || err.contains("io error"),
        "got {err}"
    );
}

#[test]
fn media_playback_status_rejects_unknown_format_value() {
    let err = media_playback_status_cmd(&["--format".into(), "aac".into()]).unwrap_err();
    assert!(err.contains("aac"), "got {err}");
}

#[test]
fn media_playback_status_default_returns_all_four_formats() {
    let v = media_playback_status_cmd(&[]).expect("ok");
    let arr = v["formats"].as_array().expect("formats array");
    assert_eq!(arr.len(), 4);
    let exts: Vec<&str> = arr.iter().filter_map(|r| r["format"].as_str()).collect();
    assert!(exts.contains(&"wav"));
    assert!(exts.contains(&"mp3"));
    assert!(exts.contains(&"ogg"));
    assert!(exts.contains(&"flac"));
    assert!(v["os"].is_string(), "got {v}");
}

#[test]
fn media_playback_status_format_filter_returns_just_one_row() {
    let v = media_playback_status_cmd(&["--format".into(), "wav".into()]).expect("ok");
    let arr = v["formats"].as_array().expect("formats array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["format"].as_str().unwrap(), "wav");
}

#[test]
fn run_media_play_routes_through_dispatcher() {
    // Confirm the cos-agent dispatcher reaches media_play_cmd via `dev`.
    let err = run("dev", &["media".into(), "play".into()]).unwrap_err();
    assert!(err.contains("usage"), "got {err}");
}

#[test]
fn run_media_playback_status_routes_through_dispatcher() {
    let v = run("dev", &["media".into(), "playback-status".into()]).expect("ok");
    assert!(v["formats"].is_array(), "got {v}");
}

// ----------------------------------------------------------------
// CLI dispatch contract
//
// Every subcommand dispatcher must reject bad input with an error
// that tells the user what to do instead. That contract used to be
// covered by ~120 near-identical 4-line tests; the tables below
// assert the same thing per-dispatcher without the duplication.
// ----------------------------------------------------------------

/// One row of a CLI-rejection table: a label for failure output, the
/// invocation under test, and substrings the error must mention.
type CliCase = (
    &'static str,
    Box<dyn Fn() -> Result<(), String>>,
    Vec<&'static str>,
);

/// Build a [`CliCase`]. The call is normalised to `Result<(), String>`
/// so dispatchers with different `Ok` types share one table.
macro_rules! cli_case {
    ($label:expr, $call:expr, [$($want:expr),* $(,)?]) => {
        (
            $label,
            Box::new(move || $call.map(|_| ())) as Box<dyn Fn() -> Result<(), String>>,
            vec![$($want),*],
        )
    };
}

/// Assert every case errors and that the message mentions each
/// expected substring. Matching is case-insensitive because some
/// dispatchers capitalise their usage banner.
fn assert_cli_rejects(cases: Vec<CliCase>) {
    for (label, invoke, expected) in cases {
        let err = match invoke() {
            Err(e) => e,
            Ok(()) => panic!("{label}: expected an error, got Ok"),
        };
        let hay = err.to_lowercase();
        for want in expected {
            assert!(
                hay.contains(&want.to_lowercase()),
                "{label}: error {err:?} should mention {want:?}"
            );
        }
    }
}

#[test]
fn cli_unknown_subcommand_lists_available_options() {
    assert_cli_rejects(vec![
        cli_case!(
            "agent root",
            run("not-a-command", &[]),
            ["ask", "setup", "sessions", "override", "dev"]
        ),
        cli_case!(
            "budget user",
            budget_cmd(&["user".into(), "bogus".into()]),
            ["bogus", "show", "path"]
        ),
        cli_case!(
            "insights",
            insights_cmd(&["bogus".into()]),
            ["bogus", "overall"]
        ),
        cli_case!(
            "notes",
            notes_cmd(&["bogus".into()]),
            ["list", "read", "write"]
        ),
        cli_case!(
            "skills",
            skills_cmd(&["bogus".into()]),
            ["list", "info", "disabled"]
        ),
        cli_case!(
            "skills hub (no subcommand)",
            skills_cmd(&["hub".into()]),
            ["list", "install"]
        ),
        cli_case!(
            "skills hub",
            skills_cmd(&["hub".into(), "bogus".into(), "owner/repo".into()]),
            ["list", "install"]
        ),
        cli_case!("llm", llm_cmd(&["bogus".into()]), ["providers", "models"]),
        cli_case!(
            "skills usage",
            {
                let dir = tempfile::tempdir().expect("tmp");
                let p = dir.path().join("usage.jsonl");
                skills_usage_cmd_at(&["bogus".into()], &p)
            },
            ["stats", "record"]
        ),
        cli_case!("prompt", prompt_cmd(&["bogus".into()]), ["show", "build"]),
        cli_case!(
            "nudge",
            nudge_cmd(&["bogus".into()]),
            ["list", "add", "fire"]
        ),
        cli_case!("mcp", mcp_cmd(&["bogus".into()]), ["status", "serve"]),
        cli_case!(
            "usage scope",
            usage_cmd(&["bogus".into()]),
            ["provider", "model", "session", "app", "verb"]
        ),
        cli_case!(
            "curator",
            curator_cmd(&["bogus".into()]),
            ["propose", "scan", "author"]
        ),
        cli_case!(
            "curator drafts",
            curator_drafts_cmd(&["bogus".into()]),
            ["auto-title", "retitle"]
        ),
        cli_case!("tools", tools_cmd(&["bogus".into()]), ["bogus", "list"]),
        cli_case!(
            "guardrails",
            guardrails_cmd(&["bogus".into()]),
            ["bogus", "show"]
        ),
        cli_case!(
            "approval",
            approval_cmd(&["bogus".into()]),
            ["bogus", "show"]
        ),
        cli_case!(
            "todo",
            {
                let (_dir, store) = temp_todo_store();
                todo_cmd_at(&["bogus".into()], &store)
            },
            ["bogus"]
        ),
        cli_case!("compress", compress_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("aux", aux_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("retry", retry_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("sessions", sessions_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!(
            "semantic",
            semantic_cmd(&["bogus".into()]),
            ["clear-all", "status"]
        ),
        cli_case!("vision", vision_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("display", display_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!(
            "shell hooks",
            shell_hooks_cmd(&["bogus".into()]),
            ["bogus", "init"]
        ),
        cli_case!(
            "media",
            media_cmd(&["bogus".into()]),
            ["bogus", "providers"]
        ),
        cli_case!(
            "binary ext",
            binary_ext_cmd(&["bogus".into()]),
            ["bogus", "list"]
        ),
        cli_case!(
            "context",
            context_cmd(&["bogus".into()]),
            ["bogus", "hints"]
        ),
        cli_case!("file safety", file_safety_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!("osv", osv_cmd(&["bogus".into()]), ["bogus"]),
        cli_case!(
            "interrupt",
            interrupt_cmd(&["frobnicate".into()]),
            ["unknown"]
        ),
        cli_case!("learn", learn_cmd(&["frobnicate".into()]), ["unknown"]),
        cli_case!("hooks", hooks_cmd(&["frobnicate".into()]), ["unknown"]),
    ]);
}

#[test]
fn cli_unknown_flag_is_rejected() {
    assert_cli_rejects(vec![
        cli_case!(
            "prompt show",
            prompt_cmd(&["show".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "mcp serve",
            mcp_cmd(&["serve".into(), "--bogus".into(), "x".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator propose",
            curator_cmd(&["propose".into(), "any-sid".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator author",
            curator_cmd(&["author".into(), "draft-1".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator scan",
            curator_cmd(&["scan".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "curator drafts auto-title",
            curator_drafts_cmd(&["auto-title".into(), "some-id".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "providers",
            providers_cmd(&["--bogus".into()]),
            ["--bogus", "--names"]
        ),
        cli_case!(
            "provider doctor",
            provider_doctor_cmd(&["--mystery".into()]),
            ["--mystery", "--probe-network"]
        ),
        cli_case!(
            "approval check",
            approval_cmd(&["check".into(), "echo".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "compress check",
            compress_cmd(&["check".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "mcp spawn spec",
            parse_mcp_spawn_spec(&["--cmd".into(), "x".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "aux ask",
            aux_cmd(&[
                "ask".into(),
                "--prompt".into(),
                "hi".into(),
                "--bogus".into(),
            ]),
            ["--bogus"]
        ),
        cli_case!(
            "retry schedule",
            retry_cmd(&["schedule".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "skills guard",
            {
                let dir = skills_guard_test_dir("bad-flag");
                let skill = write_test_skill(&dir, "eta", &["echo"]);
                let map = guard_skills_map(skill);
                skills_guard_cmd_against(&["eta".into(), "--bogus".into()], &map)
            },
            ["--bogus"]
        ),
        cli_case!(
            "vision route",
            vision_route_cmd(&["--bytes".into(), "1024".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "vision sniff",
            vision_sniff_cmd(&["--bogus".into(), "x".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "vision analyze",
            vision_analyze_cmd(&[
                "--bogus".into(),
                "v".into(),
                "--file".into(),
                "x.png".into(),
                "--prompt".into(),
                "describe".into(),
            ]),
            ["--bogus"]
        ),
        cli_case!(
            "display transcript",
            parse_display_transcript_args(&["--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "shell hooks tail",
            shell_hooks_cmd(&["tail".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "media list-outputs",
            media_cmd(&["list-outputs".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "binary ext list",
            binary_ext_cmd(&["list".into(), "--bogus".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "context hints",
            context_hints_cmd(&["--bogus".into(), "x".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "context refs",
            context_refs_cmd(&["--bogus".into(), "v".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "context build",
            context_cmd(&["build".into(), "--bogus".into()]),
            ["--bogus"]
        ),
        cli_case!(
            "osv query",
            osv_cmd(&[
                "query".into(),
                "foo@1.0".into(),
                "--bogus".into(),
                "x".into(),
            ]),
            ["--bogus"]
        ),
        // `ask` must enumerate supported flags so users can discover
        // `--full` without reading the source.
        cli_case!(
            "ask",
            run("ask", &["--bogus".into(), "hi".into()]),
            ["unknown ask flag", "--full"]
        ),
        cli_case!(
            "ask stream removed",
            run("ask", &["--stream".into(), "hi".into()]),
            ["unknown ask flag", "--stream", "--full"]
        ),
        cli_case!("chat", chat_cmd(&["--bogus".into()]), ["unknown flag"]),
        cli_case!(
            "learn extract",
            learn_cmd(&["extract".into(), "--frobnicate".into(), "x".into()]),
            ["unknown"]
        ),
        cli_case!(
            "media play",
            media_play_cmd(&["--frobnicate".into(), "a.wav".into()]),
            ["unknown flag"]
        ),
        cli_case!(
            "media playback-status",
            media_playback_status_cmd(&["--quack".into()]),
            ["unknown flag"]
        ),
    ]);
}

#[test]
fn cli_missing_required_argument_reports_usage() {
    assert_cli_rejects(vec![
        cli_case!("notes read", notes_cmd(&["read".into()]), ["usage"]),
        cli_case!("skills info", skills_cmd(&["info".into()]), ["usage"]),
        cli_case!(
            "skills hub list",
            skills_cmd(&["hub".into(), "list".into()]),
            ["owner/repo"]
        ),
        cli_case!(
            "skills hub install",
            skills_cmd(&["hub".into(), "install".into(), "owner/repo".into()]),
            ["usage:", "install"]
        ),
        cli_case!(
            "skills hub show",
            skills_cmd(&["hub".into(), "show".into(), "owner/repo".into()]),
            ["usage:", "show"]
        ),
        cli_case!("redact", redact_cmd(&[]), ["usage:"]),
        cli_case!(
            "skills usage record",
            {
                let dir = tempfile::tempdir().expect("tmp");
                let p = dir.path().join("usage.jsonl");
                skills_usage_cmd_at(&["record".into()], &p)
            },
            ["usage:"]
        ),
        cli_case!("think-scrub", think_scrub_cmd(&[]), ["usage:"]),
        cli_case!("tokens", tokens_cmd(&[]), ["usage:"]),
        cli_case!("nudge add", nudge_cmd(&["add".into()]), ["usage"]),
        cli_case!(
            "nudge add (due only)",
            nudge_cmd(&["add".into(), "30".into()]),
            ["usage"]
        ),
        cli_case!("nudge fire", nudge_cmd(&["fire".into()]), ["usage"]),
        cli_case!("usage provider", usage_cmd(&["provider".into()]), ["usage"]),
        cli_case!("usage model", usage_cmd(&["model".into()]), ["usage"]),
        cli_case!("usage session", usage_cmd(&["session".into()]), ["usage"]),
        cli_case!("usage app", usage_cmd(&["app".into()]), ["usage"]),
        cli_case!("usage verb", usage_cmd(&["verb".into()]), ["usage"]),
        cli_case!(
            "curator drafts auto-title",
            curator_drafts_cmd(&["auto-title".into()]),
            ["usage"]
        ),
        cli_case!("tools show", tools_cmd(&["show".into()]), ["show"]),
        cli_case!("set-title", parse_set_title_args(&[]), ["usage"]),
        cli_case!("sessions title", sessions_title(&[]), ["usage"]),
        cli_case!("display", display_cmd(&[]), ["usage"]),
        cli_case!(
            "display format-bytes",
            display_format_bytes_cmd(&[]),
            ["usage"]
        ),
        cli_case!(
            "display format-duration",
            display_format_duration_cmd(&[]),
            ["usage"]
        ),
        cli_case!(
            "shell hooks record-post",
            shell_hooks_cmd(&["record-post".into()]),
            ["usage"]
        ),
        cli_case!(
            "binary ext check",
            binary_ext_cmd(&["check".into()]),
            ["usage"]
        ),
        cli_case!("context", context_cmd(&[]), ["usage"]),
        cli_case!("file safety", file_safety_cmd(&[]), ["usage", "check"]),
        cli_case!(
            "file safety check",
            file_safety_cmd(&["check".into()]),
            ["usage"]
        ),
        cli_case!("osv", osv_cmd(&[]), ["usage", "parse"]),
        cli_case!("osv parse", osv_cmd(&["parse".into()]), ["usage"]),
    ]);
}

#[test]
fn cli_flag_without_value_names_the_flag() {
    assert_cli_rejects(vec![
        cli_case!("redact --file", redact_cmd(&["--file".into()]), ["--file"]),
        cli_case!(
            "prompt --extra",
            prompt_cmd(&["show".into(), "--extra".into()]),
            ["--extra"]
        ),
        cli_case!(
            "read_text_input --file",
            read_text_input(&["--file".into()], "tokens"),
            ["--file"]
        ),
        cli_case!(
            "usage --app",
            usage_cmd(&["overall".into(), "--app".into()]),
            ["--app"]
        ),
        cli_case!(
            "usage --verb",
            usage_cmd(&["overall".into(), "--verb".into()]),
            ["--verb"]
        ),
        cli_case!(
            "mcp serve --allow",
            mcp_cmd(&["serve".into(), "--allow".into()]),
            ["--allow"]
        ),
        cli_case!(
            "mcp serve --deny",
            mcp_cmd(&["serve".into(), "--deny".into()]),
            ["--deny"]
        ),
        cli_case!(
            "curator propose --min-turns",
            curator_cmd(&["propose".into(), "any-sid".into(), "--min-turns".into()]),
            ["--min-turns"]
        ),
        cli_case!(
            "curator scan --limit",
            curator_cmd(&["scan".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "providers --names",
            providers_cmd(&["--names".into()]),
            ["--names"]
        ),
        cli_case!(
            "provider doctor --names",
            provider_doctor_cmd(&["--names".into()]),
            ["--names"]
        ),
        cli_case!(
            "summarise --max",
            summarise_cmd(&["--max".into()]),
            ["--max"]
        ),
        cli_case!(
            "classify --labels",
            classify_cmd(&["--labels".into()]),
            ["--labels"]
        ),
        cli_case!(
            "approval check --input",
            approval_cmd(&["check".into(), "echo".into(), "--input".into()]),
            ["--input"]
        ),
        cli_case!(
            "sessions stats --session",
            sessions_stats(&["--session".into()]),
            ["--session requires"]
        ),
        cli_case!(
            "shell hooks tail --limit",
            shell_hooks_cmd(&["tail".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "media list-outputs --limit",
            media_cmd(&["list-outputs".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "media list-outputs --ext",
            media_cmd(&["list-outputs".into(), "--ext".into()]),
            ["--ext"]
        ),
        cli_case!(
            "binary ext list --limit",
            binary_ext_cmd(&["list".into(), "--limit".into()]),
            ["--limit"]
        ),
        cli_case!(
            "chat --session",
            chat_cmd(&["--session".into()]),
            ["--session"]
        ),
        cli_case!(
            "chat --max-turns",
            chat_cmd(&["--max-turns".into()]),
            ["--max-turns"]
        ),
        cli_case!(
            "media playback-status --format",
            media_playback_status_cmd(&["--format".into()]),
            ["--format"]
        ),
    ]);
}
