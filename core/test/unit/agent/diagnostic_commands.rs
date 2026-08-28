use super::*;

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
