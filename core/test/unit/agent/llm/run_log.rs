use super::*;

fn sample_engine() -> EngineInfo {
    EngineInfo {
        name: "llama-cpp".into(),
        version: "b4001".into(),
    }
}

fn sample_usage() -> Usage {
    Usage {
        input_tokens: 12,
        output_tokens: 34,
        ..Default::default()
    }
}

#[test]
fn from_success_captures_engine_info() {
    let r = LlmRunRecord::from_success(
        "llama_local",
        "/tmp/m.gguf",
        Some(sample_engine()),
        FinishReason::Stop,
        &sample_usage(),
        42,
        Some("sess-123"),
    );
    assert_eq!(r.provider, "llama_local");
    assert_eq!(r.model, "/tmp/m.gguf");
    assert_eq!(r.engine_name.as_deref(), Some("llama-cpp"));
    assert_eq!(r.engine_version.as_deref(), Some("b4001"));
    assert_eq!(r.duration_ms, 42);
    assert_eq!(r.input_tokens, 12);
    assert_eq!(r.output_tokens, 34);
    assert_eq!(r.finish_reason, "stop");
    assert_eq!(r.status, "ok");
    assert!(r.error.is_none());
    assert_eq!(r.session_id.as_deref(), Some("sess-123"));
    assert_eq!(r.decision, "allowed");
    assert!(r.denial_reason.is_none());
    assert!(r.app_id.is_none());
}

#[test]
fn from_success_omits_engine_for_cloud() {
    let r = LlmRunRecord::from_success(
        "openai_compat",
        "gpt-5",
        None,
        FinishReason::ToolUse,
        &sample_usage(),
        123,
        None,
    );
    assert!(r.engine_name.is_none());
    assert!(r.engine_version.is_none());
    assert_eq!(r.finish_reason, "tool_use");
    assert!(r.session_id.is_none());
}

#[test]
fn from_success_treats_blank_engine_strings_as_absent() {
    let r = LlmRunRecord::from_success(
        "x",
        "y",
        Some(EngineInfo {
            name: String::new(),
            version: String::new(),
        }),
        FinishReason::Other,
        &Usage::default(),
        1,
        None,
    );
    assert!(r.engine_name.is_none());
    assert!(r.engine_version.is_none());
}

#[test]
fn from_error_captures_message() {
    let r = LlmRunRecord::from_error(
        "llama_local",
        "/tmp/m.gguf",
        Some(sample_engine()),
        "library load failed",
        7,
        Some("s-1"),
    );
    assert_eq!(r.status, "error");
    assert_eq!(r.error.as_deref(), Some("library load failed"));
    assert_eq!(r.engine_version.as_deref(), Some("b4001"));
    assert_eq!(r.input_tokens, 0);
    assert_eq!(r.output_tokens, 0);
    assert_eq!(r.decision, "allowed");
    assert!(r.denial_reason.is_none());
}

/// Denials must surface as a distinct status + decision, carry the
/// stable reason token, and attribute to the calling app.
#[test]
fn from_denial_sets_decision_and_reason() {
    let r = LlmRunRecord::from_denial(
        "summarize",
        "claude-sonnet-4",
        "budget_exceeded",
        "monthly unit cap reached (1000000/1000000)",
        3,
        Some("s-1"),
    );
    assert_eq!(r.provider, "gate");
    assert_eq!(r.status, "denied");
    assert_eq!(r.decision, "denied");
    assert_eq!(r.finish_reason, "denied");
    assert_eq!(r.denial_reason.as_deref(), Some("budget_exceeded"));
    assert_eq!(r.app_id.as_deref(), Some("summarize"));
    assert!(r.error.as_deref().unwrap().contains("monthly unit cap"));
    assert!(r.engine_name.is_none());
}

/// `with_app` attaches an app id to an otherwise app-agnostic
/// success record. Used by the `cos ai chat` path.
#[test]
fn with_app_attaches_id() {
    let r = LlmRunRecord::from_success(
        "openai_compat",
        "gpt-4o",
        None,
        FinishReason::Stop,
        &sample_usage(),
        10,
        None,
    )
    .with_app("summarize");
    assert_eq!(r.app_id.as_deref(), Some("summarize"));
    assert_eq!(r.decision, "allowed");
}

/// A log line missing the new `decision` / `denial_reason` /
/// `app_id` fields (pre-Phase-8 format) must still deserialise as
/// an "allowed" record.
#[test]
fn legacy_jsonl_lines_default_to_allowed() {
    let legacy = r#"{
        "timestamp": "2026-04-01T00:00:00.000Z",
        "provider": "mock",
        "model": "mock-model",
        "duration_ms": 5,
        "finish_reason": "stop",
        "status": "ok"
    }"#;
    let r: LlmRunRecord = serde_json::from_str(legacy).expect("valid legacy line");
    assert_eq!(r.decision, "allowed");
    assert!(r.denial_reason.is_none());
    assert!(r.app_id.is_none());
}

/// `record_to_path` is what runs in tests because the public `record()`
/// is a no-op under `cfg(test)`. Round-trip the JSON to make sure the
/// schema actually reaches disk in the expected shape.
#[test]
fn record_to_path_round_trips_through_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("ai.jsonl");
    let r = LlmRunRecord::from_success(
        "mock",
        "mock-model",
        None,
        FinishReason::Stop,
        &sample_usage(),
        5,
        None,
    );
    record_to_path(&r, &p).expect("write should succeed");
    record_to_path(&r, &p).expect("second write should append, not fail");

    let body = std::fs::read_to_string(&p).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "second write should append a line");
    let parsed: LlmRunRecord = serde_json::from_str(lines[0]).expect("valid jsonl");
    assert_eq!(parsed.provider, "mock");
    assert_eq!(parsed.model, "mock-model");
    assert_eq!(parsed.input_tokens, 12);
    assert_eq!(parsed.decision, "allowed");
}

/// Public `record()` MUST be a no-op in test builds — verifying by
/// confirming it doesn't panic and the host's run log path doesn't
/// get touched. We can't reliably stat the host log path without
/// racing other tests, so just check that the call returns.
#[test]
fn record_is_a_noop_in_tests() {
    let r = LlmRunRecord::from_success(
        "mock",
        "mock-model",
        None,
        FinishReason::Stop,
        &Usage::default(),
        0,
        None,
    );
    // Doesn't panic, doesn't touch disk.
    record(&r);
}

#[test]
fn finish_reason_str_covers_all_variants() {
    assert_eq!(finish_reason_str(FinishReason::Stop), "stop");
    assert_eq!(finish_reason_str(FinishReason::Length), "length");
    assert_eq!(finish_reason_str(FinishReason::ToolUse), "tool_use");
    assert_eq!(finish_reason_str(FinishReason::Refusal), "refusal");
    assert_eq!(
        finish_reason_str(FinishReason::ContentFilter),
        "content_filter"
    );
    assert_eq!(finish_reason_str(FinishReason::Other), "other");
}

#[test]
fn from_tool_call_allowed_records_kernel_provider_and_verb() {
    let r = LlmRunRecord::from_tool_call(
        "fs.read_text",
        "summarize",
        "fs.read",
        "allowed",
        None,
        None,
        42,
        Some("sess-abc"),
    );
    assert_eq!(r.provider, "kernel");
    assert_eq!(r.model, "tool:fs.read_text");
    assert_eq!(r.app_id.as_deref(), Some("summarize"));
    assert_eq!(r.verb.as_deref(), Some("fs.read"));
    assert_eq!(r.decision, "allowed");
    assert_eq!(r.status, "ok");
    assert!(r.error.is_none());
    assert!(r.denial_reason.is_none());
    assert_eq!(r.duration_ms, 42);
    assert_eq!(r.session_id.as_deref(), Some("sess-abc"));
}

#[test]
fn from_tool_call_denied_records_denial_reason_and_error() {
    let r = LlmRunRecord::from_tool_call(
        "fs.read_text",
        "summarize",
        "fs.read",
        "denied",
        Some("caps_denied"),
        Some("denied: caps refused fs.read on /etc/passwd"),
        7,
        None,
    );
    assert_eq!(r.decision, "denied");
    assert_eq!(r.status, "denied");
    assert_eq!(r.denial_reason.as_deref(), Some("caps_denied"));
    assert!(r.error.as_deref().unwrap().contains("denied:"));
}

#[test]
fn from_tool_call_unknown_tool_records_empty_verb() {
    let r = LlmRunRecord::from_tool_call(
        "fs.unicorn",
        "summarize",
        "",
        "denied",
        Some("unknown_tool"),
        Some("unknown tool: fs.unicorn"),
        1,
        None,
    );
    assert_eq!(r.model, "tool:fs.unicorn");
    assert!(r.verb.is_none(), "empty verb should not be stored");
    assert_eq!(r.denial_reason.as_deref(), Some("unknown_tool"));
}
