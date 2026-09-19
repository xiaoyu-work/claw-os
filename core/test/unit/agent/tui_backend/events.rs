use super::super::tests::{job, SESSION, TASK};
use super::*;

fn projection() -> Projection {
    let mut projection = Projection::new("thread-1".into(), SESSION.into(), TASK.into());
    projection.begin("Hello Claw", Some("client-1"), &job("running"));
    projection
}

fn event(value: Value) -> Value {
    json!({ "event": value, "ts": "2026-09-13T12:00:01Z" })
}

fn done(finish: &str) -> Value {
    event(json!({
        "kind": "done", "finish": finish,
        "usage": { "input_tokens": 3, "output_tokens": 5, "cache_read_tokens": 2, "cache_write_tokens": 1 },
    }))
}

#[test]
fn provider_done_is_not_the_task_turn_completion() {
    let mut projection = projection();
    projection
        .record(&event(
            json!({ "kind": "text_delta", "text": "I will check." }),
        ))
        .unwrap();
    projection
        .record(&event(json!({
            "kind": "tool_use_start", "id": "tool-1", "name": "cos_fs",
        })))
        .unwrap();
    let projected = projection.record(&done("tool_use")).unwrap();
    assert!(projected
        .messages
        .iter()
        .any(|event| event["method"] == "item/completed"));
    assert!(!projected
        .messages
        .iter()
        .any(|event| event["method"] == "turn/completed"));
    assert_eq!(
        projection
            .items
            .iter()
            .find(|item| item["id"] == "tool-1")
            .unwrap()["status"],
        "inProgress"
    );
    let usage = projected
        .messages
        .iter()
        .find(|event| event["method"] == "thread/tokenUsage/updated")
        .unwrap();
    assert_eq!(usage["params"]["tokenUsage"]["total"]["totalTokens"], 8);
}

#[test]
fn tool_lifecycle_deduplicates_provider_and_runtime_events_and_hides_bodies() {
    let mut projection = projection();
    let first = projection
        .record(&event(json!({
            "kind": "tool_use_start", "id": "tool-1", "name": "cos_fs",
        })))
        .unwrap();
    assert_eq!(first.messages.len(), 1);
    let formed = projection.record(&event(json!({
        "kind": "tool_use", "id": "tool-1", "name": "cos_fs", "input": { "secret": "private-input" },
    }))).unwrap();
    assert!(formed.messages.is_empty());
    let started = projection.record(&json!({
        "progress": { "kind": "tool_start", "id": "tool-1", "name": "cos_fs", "input": { "secret": "private-input" } },
    })).unwrap();
    assert!(started.messages.is_empty());
    let completed = projection.record(&json!({
        "progress": { "kind": "tool_result", "id": "tool-1", "name": "cos_fs", "ok": true, "latency_ms": 13, "content_preview": "private-result" },
    })).unwrap();
    assert_eq!(completed.messages[0]["method"], "item/completed");
    let item = &completed.messages[0]["params"]["item"];
    assert_eq!(item["id"], "tool-1");
    assert_eq!(item["success"], true);
    assert_eq!(item["durationMs"], 13);
    assert_eq!(item["arguments"], Value::Null);
    assert!(!completed.messages[0].to_string().contains("private-result"));
}

#[test]
fn emits_only_provider_reasoning_summaries_not_opaque_state() {
    let mut projection = projection();
    let output = projection
        .record(&event(json!({
            "kind": "reasoning", "id": "reason-1", "summary": ["A visible summary"],
            "encrypted_content": "opaque-provider-secret",
        })))
        .unwrap();
    let serialized = json!(output.messages).to_string();
    assert!(serialized.contains("A visible summary"));
    assert!(!serialized.contains("opaque-provider-secret"));
    assert!(!serialized.contains("encrypted_content"));
    assert!(projection
        .record(&event(json!({
            "kind": "tool_state", "tool_use_id": "tool-1", "thought_signature": "hidden-signature",
        })))
        .unwrap()
        .messages
        .is_empty());
}

#[test]
fn nonstreaming_messages_preserve_public_items_without_provider_state() {
    let response = crate::agent::llm::types::ChatResponse {
        model: "model-for-tests".into(),
        content: vec![
            ContentBlock::Text {
                text: "Visible response".into(),
            },
            ContentBlock::ToolUse {
                id: "tool-2".into(),
                name: "cos_fs".into(),
                input: json!({ "secret": "hidden-input" }),
            },
            ContentBlock::Reasoning {
                id: "rs-2".into(),
                summary: vec!["Public summary".into()],
                encrypted_content: Some("hidden-state".into()),
            },
            ContentBlock::ToolState {
                tool_use_id: "tool-2".into(),
                thought_signature: "hidden-signature".into(),
            },
        ],
        tool_calls: Vec::new(),
        finish_reason: crate::agent::llm::types::FinishReason::ToolUse,
        usage: crate::agent::llm::types::Usage::default(),
    };
    let mut projection = projection();
    let mut output = projection
        .record(&event(
            serde_json::to_value(StreamEvent::Message(response)).unwrap(),
        ))
        .unwrap()
        .messages;
    output.extend(projection.record(&done("tool_use")).unwrap().messages);
    let wire = json!(output).to_string();
    assert!(wire.contains("Visible response"));
    assert!(wire.contains("Public summary"));
    assert!(wire.contains("tool-2"));
    for hidden in ["hidden-input", "hidden-state", "hidden-signature"] {
        assert!(!wire.contains(hidden));
    }
}

#[test]
fn terminal_job_drives_completion_error_and_cancellation() {
    for (job_status, turn_status) in [
        ("ok", "completed"),
        ("error", "failed"),
        ("cancelled", "interrupted"),
    ] {
        let mut projection = projection();
        let messages = projection.finish(&job(job_status)).unwrap();
        let turn = &messages
            .iter()
            .find(|event| event["method"] == "turn/completed")
            .unwrap()["params"]["turn"];
        assert_eq!(turn["id"], TASK);
        assert_eq!(turn["status"], turn_status);
        assert_eq!(!turn["error"].is_null(), job_status == "error");
        assert_eq!(messages.last().unwrap()["params"]["status"]["type"], "idle");
    }
    assert!(projection().finish(&job("running")).is_err());
    let mut wrong = job("ok");
    wrong["session_id"] = json!("different-session");
    assert!(projection().finish(&wrong).is_err());
}

#[test]
fn missing_tool_result_never_becomes_a_success() {
    let mut projection = projection();
    projection
        .record(&event(json!({
            "kind": "tool_use_start", "id": "tool-1", "name": "cos_exec",
        })))
        .unwrap();
    let output = projection.finish(&job("cancelled")).unwrap();
    let completed = output
        .iter()
        .find(|event| {
            event["method"] == "item/completed" && event["params"]["item"]["id"] == "tool-1"
        })
        .unwrap();
    assert_eq!(completed["params"]["item"]["success"], Value::Null);
    assert_eq!(completed["params"]["item"]["status"], "failed");
}

#[test]
fn streaming_redaction_handles_split_credentials() {
    let mut projection = projection();
    let mut output = Vec::new();
    output.extend(
        projection
            .record(&event(json!({
                "kind": "text_delta", "text": "Do not reveal sk-",
            })))
            .unwrap()
            .messages,
    );
    output.extend(
        projection
            .record(&event(json!({
                "kind": "text_delta", "text": "abcdefghijklmnopqrstuvwx ",
            })))
            .unwrap()
            .messages,
    );
    output.extend(projection.record(&done("stop")).unwrap().messages);
    let serialized = json!(output).to_string();
    assert!(!serialized.contains("sk-abcdefghijklmnopqrstuvwx"));
    assert!(serialized.contains("REDACTED"));
}

#[test]
fn private_key_blocks_wait_for_a_complete_matching_footer() {
    let mut text = TextProjection::default();
    assert!(text
        .push(
            "-----BEGIN PRIVATE KEY-----\nprivate-body\n-----END ",
            false,
        )
        .unwrap()
        .is_empty());
    assert!(text
        .push(&format!("{} ", "unrelated".repeat(50)), false)
        .unwrap()
        .is_empty());
    let completed = text.push("", true).unwrap();
    assert!(!completed.contains("private-body"));
    assert!(completed.contains("REDACTED"));
    let mut text = TextProjection::default();
    let text_with_second_key = concat!(
        "-----BEGIN PRIVATE KEY-----\nfirst-body\n-----END PRIVATE KEY-----\n",
        "-----BEGIN PRIVATE KEY-----\nsecond-body\n"
    );
    let rendered = text.push(text_with_second_key, true).unwrap();
    assert!(!rendered.contains("first-body"));
    assert!(!rendered.contains("second-body"));
}

#[test]
fn unknown_events_fail_explicitly_and_reasoning_is_bounded() {
    let mut projection = projection();
    assert!(projection
        .record(&event(json!({ "kind": "unknown_protocol_event" })))
        .is_err());
    assert!(projection
        .record(&event(json!({
            "kind": "reasoning", "id": "reason-1",
            "summary": ["x".repeat(MAX_MESSAGE_BYTES)], "encrypted_content": null,
        })))
        .is_err());
}
