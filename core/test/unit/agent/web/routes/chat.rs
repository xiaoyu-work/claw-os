use super::*;
use crate::agent::llm::{ChatResponse, FinishReason, ToolCall, Usage};

#[tokio::test]
async fn chat_drive_installs_one_complete_config_snapshot() {
    let mut config = crate::config::CosConfig::default();
    config.agent.provider = "mock".into();
    config.agent.model = "snapshot-model".into();
    config.embed.provider = "none".into();
    let config = Arc::new(config);

    let observed = with_request_snapshot(Arc::clone(&config), async {
        crate::config::current_snapshot()
    })
    .await;

    assert!(Arc::ptr_eq(&config, &observed));
    assert_eq!(observed.agent.model, "snapshot-model");
    assert_eq!(observed.embed.provider, "none");
}

#[test]
fn generated_sessions_get_independent_turn_leases() {
    let state = AppState::new(crate::config::AgentConfig::default(), 1000);
    let (first_id, _first_lease) =
        begin_turn(&state, None).unwrap_or_else(|_| panic!("first generated turn was rejected"));
    let (second_id, _second_lease) =
        begin_turn(&state, None).unwrap_or_else(|_| panic!("second generated turn was rejected"));

    assert_ne!(first_id, second_id);
}

#[test]
fn active_session_is_rejected_until_turn_finishes() {
    let state = AppState::new(crate::config::AgentConfig::default(), 1000);
    let session_id = format!("parallel-post-{}", uuid::Uuid::new_v4().simple());
    let (_id, lease) = begin_turn(&state, Some(&session_id)).expect("first lease");
    let conflict = begin_turn(&state, Some(&session_id))
        .err()
        .expect("parallel turn must be rejected");
    assert_eq!(conflict.session_id, session_id);
    drop(lease);
    assert!(begin_turn(&state, Some(&session_id)).is_ok());
}

#[test]
fn user_facing_tool_events_omit_inputs_and_results() {
    let mut turn_emitted_text = false;
    let mut emitted_text = false;
    let mut finish = None;
    let input = serde_json::to_value(StreamEvent::ToolInputDelta {
        id: "call-1".into(),
        partial_json: r#"{"secret":"input"}"#.into(),
    })
    .unwrap();
    assert!(project_stream_record(
        json!({ "event": input }),
        &mut turn_emitted_text,
        &mut emitted_text,
        &mut finish,
    )
    .unwrap()
    .is_empty());

    let call = serde_json::to_value(StreamEvent::ToolUse(ToolCall {
        id: "call-1".into(),
        name: "cos_sysinfo".into(),
        input: json!({"secret": "input"}),
    }))
    .unwrap();
    let mut output = project_stream_record(
        json!({ "event": call }),
        &mut turn_emitted_text,
        &mut emitted_text,
        &mut finish,
    )
    .unwrap()
    .join("");
    output.push_str(
        &project_stream_record(
            json!({
                "progress": {
                    "kind": "tool_result",
                    "id": "call-1",
                    "name": "cos_sysinfo",
                    "ok": true,
                    "preview": "secret result",
                }
            }),
            &mut turn_emitted_text,
            &mut emitted_text,
            &mut finish,
        )
        .unwrap()
        .join(""),
    );
    assert!(output.contains("cos_sysinfo"));
    assert!(!output.contains("secret"));
    assert!(!output.contains("\"input\""));
    assert!(!output.contains("preview"));
}

#[test]
fn buffered_message_emits_one_text_frame_with_tools_and_terminal_metadata() {
    let mut turn_emitted_text = false;
    let mut emitted_text = false;
    let mut finish = None;
    let message = StreamEvent::Message(ChatResponse {
        model: "gemini-test".into(),
        content: vec![
            ContentBlock::Text {
                text: "buffered ".into(),
            },
            ContentBlock::Text {
                text: "answer".into(),
            },
        ],
        tool_calls: vec![ToolCall {
            id: "lookup::0".into(),
            name: "lookup".into(),
            input: json!({"q": "weather"}),
        }],
        finish_reason: FinishReason::ToolUse,
        usage: Usage {
            input_tokens: 8,
            output_tokens: 5,
            ..Usage::default()
        },
    });
    let done = StreamEvent::Done {
        finish: FinishReason::ToolUse,
        usage: Usage {
            input_tokens: 8,
            output_tokens: 5,
            ..Usage::default()
        },
    };

    let mut output = project_stream_record(
        json!({ "event": serde_json::to_value(message).unwrap() }),
        &mut turn_emitted_text,
        &mut emitted_text,
        &mut finish,
    )
    .unwrap()
    .join("");
    output.push_str(
        &project_stream_record(
            json!({ "event": serde_json::to_value(done).unwrap() }),
            &mut turn_emitted_text,
            &mut emitted_text,
            &mut finish,
        )
        .unwrap()
        .join(""),
    );

    assert_eq!(output.matches("event: text\n").count(), 1);
    assert_eq!(output.matches(r#""delta":"buffered answer""#).count(), 1);
    assert_eq!(output.matches("event: tool_use\n").count(), 1);
    assert_eq!(output.matches("event: turn_done\n").count(), 1);
    assert!(output.contains(r#""finish":"tooluse""#));
    assert_eq!(finish.as_deref(), Some("tooluse"));
}

#[test]
fn approval_progress_is_projected_without_exposing_capability_details() {
    let frames = project_progress(&json!({
        "kind": "waiting_approval",
        "request_ids": ["approval-a"],
        "scope": "/private/secret",
    }));
    let output = frames.join("");
    assert!(output.contains("Waiting for approval: approval-a"));
    assert!(!output.contains("/private/secret"));
}

#[tokio::test]
async fn dropping_response_marks_the_task_disconnected() {
    let disconnected = Arc::new(AtomicBool::new(false));
    let observed = disconnected.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        while !observed.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        let _ = tx.send(());
    });
    let guard = DisconnectOnDrop::new(disconnected, task);
    drop(guard);
    tokio::time::timeout(Duration::from_secs(1), rx)
        .await
        .expect("disconnect was not observed")
        .expect("observer stopped");
}
