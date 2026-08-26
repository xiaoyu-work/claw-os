use super::*;

#[test]
fn stream_records_avoid_duplicate_message_text() {
    let mut turn_text = false;
    let mut any_text = false;
    let delta = json!({
        "event": { "kind": "text_delta", "text": "hello" },
    });
    assert_eq!(
        events_from_stream_record(&delta, &mut turn_text, &mut any_text).len(),
        1
    );
    assert!(turn_text);
    assert!(any_text);

    let message = json!({
        "event": {
            "kind": "message",
            "content": [{ "type": "text", "text": "hello" }],
            "tool_calls": [],
        },
    });
    assert!(events_from_stream_record(&message, &mut turn_text, &mut any_text).is_empty());
}

#[test]
fn non_streaming_message_still_emits_text() {
    let mut turn_text = false;
    let mut any_text = false;
    let message = json!({
        "event": {
            "kind": "message",
            "content": [{ "type": "text", "text": "complete answer" }],
            "tool_calls": [],
        },
    });
    assert_eq!(
        events_from_stream_record(&message, &mut turn_text, &mut any_text).len(),
        1
    );
    assert!(any_text);
}

#[test]
fn runtime_tool_progress_is_forwarded() {
    let mut turn_text = false;
    let mut any_text = false;
    let progress = json!({
        "progress": {
            "kind": "tool_result",
            "id": "tool-1",
            "name": "fs.read",
            "ok": true,
            "input": {"path": "/private"},
            "preview": "done",
        },
    });
    assert_eq!(
        events_from_stream_record(&progress, &mut turn_text, &mut any_text).len(),
        1
    );
    let (_, payload) = visible_tool_progress(&progress["progress"]).unwrap();
    assert_eq!(payload["name"], "fs.read");
    assert!(payload.get("input").is_none());
    assert!(payload.get("preview").is_none());
}
