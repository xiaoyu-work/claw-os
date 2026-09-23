use super::*;
use serde_json::json;

#[test]
fn stream_records_translate_without_duplicate_text() {
    let mut turn_text = false;
    let mut any_text = false;
    let delta = serde_json::from_value(json!({
        "event": { "kind": "text_delta", "text": "hello" },
    }))
    .unwrap();
    assert_eq!(stream_events(delta, &mut turn_text, &mut any_text).len(), 1);
    assert!(turn_text);
    assert!(any_text);

    let message = serde_json::from_value(json!({
        "event": {
            "kind": "message",
            "content": [{ "type": "text", "text": "hello" }],
            "tool_calls": [],
        },
    }))
    .unwrap();
    assert!(stream_events(message, &mut turn_text, &mut any_text).is_empty());
}

#[test]
fn every_supported_core_stream_record_maps_to_a_typed_event() {
    let records = [
        (
            json!({"event":{"kind":"text_delta","text":"hello"}}),
            "delta",
        ),
        (
            json!({"event":{"kind":"tool_use_start","id":"t","name":"fs.read"}}),
            "tool_use_start",
        ),
        (
            json!({"event":{"kind":"tool_use","id":"t","name":"fs.read","input":{"path":"x"}}}),
            "tool_use",
        ),
        (
            json!({"event":{"kind":"reasoning","summary":["Checking context"]}}),
            "reasoning",
        ),
        (
            json!({"event":{"kind":"message","content":[{"type":"text","text":"complete"}]}}),
            "delta",
        ),
        (
            json!({"event":{"kind":"done","finish":"stop","usage":{"input_tokens":1}}}),
            "turn_done",
        ),
        (
            json!({"event":{"kind":"warning","message":"careful"}}),
            "warning",
        ),
        (
            json!({"progress":{"kind":"tool_start","id":"t","name":"fs.read"}}),
            "tool_start",
        ),
        (
            json!({"progress":{"kind":"tool_result","id":"t","name":"fs.read","ok":true}}),
            "tool_result",
        ),
    ];

    for (record, expected_name) in records {
        let record = serde_json::from_value(record).unwrap();
        let event = stream_events(record, &mut false, &mut false)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(event.event_name(), expected_name);
    }
}

#[test]
fn live_tool_arguments_remain_redacted() {
    let partial = serde_json::from_value(json!({
        "event":{"kind":"tool_input_delta","id":"t","partial_json":"{\"path\":"}
    }))
    .unwrap();
    assert!(stream_events(partial, &mut false, &mut false).is_empty());

    let complete = serde_json::from_value(json!({
        "event":{"kind":"tool_use","id":"t","name":"fs.read","input":{"path":"/private"}}
    }))
    .unwrap();
    let event = stream_events(complete, &mut false, &mut false)
        .into_iter()
        .next()
        .unwrap();
    assert!(matches!(
        event,
        StreamEvent::ToolUse(ToolUsePayload { input: None, .. })
    ));
}

#[test]
fn runtime_progress_exposes_only_presentation_fields() {
    let mut turn_text = false;
    let mut any_text = false;
    let record = serde_json::from_value(json!({
        "progress": {
            "kind": "tool_result",
            "id": "tool-1",
            "name": "fs.read",
            "ok": true,
            "latency_ms": 12,
            "bytes_returned": 34,
            "input": {"path": "/private"},
            "preview": "private output",
            "error_preview": "must not appear",
        },
    }))
    .unwrap();
    let events = stream_events(record, &mut turn_text, &mut any_text);
    assert!(matches!(
        &events[0],
        StreamEvent::ToolResult(ToolResultPayload {
            id,
            name,
            ok: Some(true),
            preview: None,
            latency_ms: Some(12),
            bytes_returned: Some(34),
            error_preview: None,
            ..
        }) if id == "tool-1" && name == "fs.read"
    ));
}

#[test]
fn failed_tool_preview_is_redacted_bounded_and_success_bodies_stay_hidden() {
    let secret = format!("Bearer {} {}", "a".repeat(32), "x".repeat(3_000));
    let record = serde_json::from_value(json!({
        "progress": {
            "kind": "tool_result",
            "id": "tool-1",
            "name": "fs.read",
            "ok": false,
            "latency_ms": 98,
            "bytes_returned": 4096,
            "error_preview": secret,
        },
    }))
    .unwrap();
    let event = stream_events(record, &mut false, &mut false)
        .into_iter()
        .next()
        .unwrap();
    let StreamEvent::ToolResult(result) = event else {
        panic!("expected tool result");
    };
    let preview = result.error_preview.unwrap();
    assert!(preview.contains("[REDACTED]"));
    assert!(!preview.contains(&"a".repeat(32)));
    assert!(preview.len() <= MAX_ERROR_PREVIEW_BYTES);
    assert_eq!(result.latency_ms, Some(98));
    assert_eq!(result.bytes_returned, Some(4096));
    assert!(result.preview.is_none());
    assert!(result.output.is_none());
    assert!(result.content.is_none());
    assert!(result.text.is_none());
}

#[test]
fn terminal_job_redacts_core_implementation_fields() {
    let value = json!({
        "id": "task-1",
        "status": "ok",
        "session_id": "session-1",
        "response": "answer",
        "turns_used": 2,
        "provider": "provider",
        "model": "model",
        "prompt": "secret prompt",
        "owner_uid": 1000,
        "worker_pid": 42,
    });
    let job: CoreJob = serde_json::from_value(value).unwrap();
    let done = job.into_done().unwrap();
    let json = serde_json::to_value(done).unwrap();
    assert_eq!(json["answer"], "answer");
    assert!(json.get("prompt").is_none());
    assert!(json.get("owner_uid").is_none());
    assert!(json.get("worker_pid").is_none());
}

#[test]
fn history_drops_raw_content_and_system_rows() {
    let response = history(json!({
        "session_id": "session-1",
        "n": 2,
        "messages": [
            {"role": "system", "content": "hidden", "text": "hidden"},
            {
                "role": "assistant",
                "content": "[tool_use:fs.read] {\"path\":\"private\"}",
                "text": "visible",
                "tool_calls": [{"name":"fs.read","input":{"path":"visible"}}],
                "tool_results": [{
                    "name":"fs.read",
                    "text":"successful body",
                    "is_error":false
                }, {
                    "name":"shell",
                    "text":"Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa failed",
                    "is_error":true
                }],
                "ts_ms": 10
            }
        ]
    }))
    .unwrap();
    assert_eq!(response.n, 1);
    let json = serde_json::to_value(response).unwrap();
    assert!(json["messages"][0].get("content").is_none());
    assert_eq!(json["messages"][0]["tool_results"][0]["text"], "");
    assert!(
        json["messages"][0]["tool_results"][0]
            .get("error_preview")
            .is_none()
    );
    assert_eq!(json["messages"][0]["tool_results"][1]["text"], "");
    assert_eq!(
        json["messages"][0]["tool_results"][1]["error_preview"],
        "[REDACTED] failed"
    );
}
