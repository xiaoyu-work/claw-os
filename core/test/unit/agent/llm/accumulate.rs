use super::*;
use futures_util::stream;
use std::sync::Mutex;

fn s(events: Vec<Result<StreamEvent>>) -> BoxStream<'static, Result<StreamEvent>> {
    Box::pin(stream::iter(events))
}

#[derive(Default)]
struct CountingSink {
    events: Mutex<Vec<StreamEvent>>,
}
impl StreamSink for CountingSink {
    fn on_event(&self, event: &StreamEvent) {
        self.events.lock().unwrap().push(event.clone());
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn assembles_text_only_response() {
    let sink: Arc<CountingSink> = Arc::default();
    let stream = s(vec![
        Ok(StreamEvent::TextDelta {
            text: "Hello".into(),
        }),
        Ok(StreamEvent::TextDelta {
            text: " world".into(),
        }),
        Ok(StreamEvent::Done {
            finish: FinishReason::Stop,
            usage: Usage {
                input_tokens: 5,
                output_tokens: 2,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink.clone(), "test-model"))
        .unwrap();
    assert_eq!(resp.content.len(), 1);
    match &resp.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
        other => panic!("expected Text block, got {other:?}"),
    }
    assert!(resp.tool_calls.is_empty());
    assert_eq!(resp.usage.input_tokens, 5);
    assert_eq!(resp.usage.output_tokens, 2);
    assert!(matches!(resp.finish_reason, FinishReason::Stop));

    // Sink saw all 3 events.
    let seen = sink.events.lock().unwrap();
    assert_eq!(seen.len(), 3);
}

#[test]
fn assembles_tool_use_with_streamed_input() {
    let sink = Arc::new(NullSink);
    let stream = s(vec![
        Ok(StreamEvent::ToolUseStart {
            id: "t1".into(),
            name: "echo".into(),
        }),
        Ok(StreamEvent::ToolInputDelta {
            id: "t1".into(),
            partial_json: "{\"text\":\"hi".into(),
        }),
        Ok(StreamEvent::ToolInputDelta {
            id: "t1".into(),
            partial_json: "\"}".into(),
        }),
        Ok(StreamEvent::Done {
            finish: FinishReason::ToolUse,
            usage: Usage::default(),
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink, "test-model"))
        .unwrap();
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "echo");
    assert_eq!(resp.tool_calls[0].input, serde_json::json!({"text": "hi"}));
    assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
}

#[test]
fn preserves_responses_reasoning_before_tool_use() {
    let sink = Arc::new(NullSink);
    let stream = s(vec![
        Ok(StreamEvent::Reasoning {
            id: "rs_1".into(),
            summary: vec!["Need to inspect the file.".into()],
            encrypted_content: Some("opaque-ciphertext".into()),
        }),
        Ok(StreamEvent::ToolState {
            tool_use_id: "call_1".into(),
            thought_signature: "opaque-thought-signature".into(),
        }),
        Ok(StreamEvent::ToolUse(ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "/tmp/a"}),
        })),
        Ok(StreamEvent::Done {
            finish: FinishReason::ToolUse,
            usage: Usage::default(),
        }),
    ]);
    let response = rt()
        .block_on(accumulate_stream(stream, sink, "gpt-5.6-sol"))
        .unwrap();

    assert!(matches!(
        &response.content[0],
        ContentBlock::Reasoning {
            id,
            encrypted_content: Some(content),
            ..
        } if id == "rs_1" && content == "opaque-ciphertext"
    ));
    assert!(matches!(
        &response.content[1],
        ContentBlock::ToolState {
            tool_use_id,
            thought_signature,
        } if tool_use_id == "call_1" && thought_signature == "opaque-thought-signature"
    ));
    assert!(matches!(
        &response.content[2],
        ContentBlock::ToolUse { id, .. } if id == "call_1"
    ));
}

#[test]
fn final_tool_use_event_overrides_partial_json() {
    let sink = Arc::new(NullSink);
    let stream = s(vec![
        Ok(StreamEvent::ToolUseStart {
            id: "t1".into(),
            name: "echo".into(),
        }),
        Ok(StreamEvent::ToolInputDelta {
            id: "t1".into(),
            partial_json: "incomplete".into(),
        }),
        Ok(StreamEvent::ToolUse(ToolCall {
            id: "t1".into(),
            name: "echo".into(),
            input: serde_json::json!({"final": true}),
        })),
        Ok(StreamEvent::Done {
            finish: FinishReason::ToolUse,
            usage: Usage::default(),
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink, "test-model"))
        .unwrap();
    assert_eq!(resp.tool_calls[0].input, serde_json::json!({"final": true}));
}

#[test]
fn explicit_message_short_circuits_block_assembly() {
    let sink = Arc::new(NullSink);
    let mut explicit = ChatResponse {
        model: "explicit-model".into(),
        content: vec![ContentBlock::Text {
            text: "from message".into(),
        }],
        tool_calls: vec![],
        finish_reason: FinishReason::Length,
        usage: Usage {
            input_tokens: 100,
            ..Default::default()
        },
    };
    // Provider sends both Message and (less-trusted) text deltas.
    let stream = s(vec![
        Ok(StreamEvent::TextDelta {
            text: "noise".into(),
        }),
        Ok(StreamEvent::Message(explicit.clone())),
        Ok(StreamEvent::Done {
            finish: FinishReason::Stop,
            usage: Usage {
                input_tokens: 200, // overrides Message's usage
                ..Default::default()
            },
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink, "test-model"))
        .unwrap();
    // Adopted from explicit Message, NOT assembled from deltas.
    assert_eq!(resp.content.len(), 1);
    match &resp.content[0] {
        ContentBlock::Text { text } => assert_eq!(text, "from message"),
        _ => panic!(),
    }
    // But Done overrides Message's usage and finish.
    assert_eq!(resp.usage.input_tokens, 200);
    assert!(matches!(resp.finish_reason, FinishReason::Stop));
    // Mute "unused mut" — explicit was prepared up front for clarity.
    explicit.usage.input_tokens = 0;
}

#[test]
fn done_with_tool_use_finish_reason_propagates() {
    let sink = Arc::new(NullSink);
    let stream = s(vec![
        Ok(StreamEvent::ToolUseStart {
            id: "t1".into(),
            name: "echo".into(),
        }),
        Ok(StreamEvent::Done {
            finish: FinishReason::ToolUse,
            usage: Usage::default(),
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink, "test-model"))
        .unwrap();
    assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
}

#[test]
fn missing_done_with_tool_use_blocks_upgrades_finish_reason() {
    // Provider streamed a tool_use start but never issued Done
    // — accumulator's default `Stop` should be upgraded so the
    // outer loop continues to dispatch tools.
    let sink = Arc::new(NullSink);
    let stream = s(vec![Ok(StreamEvent::ToolUseStart {
        id: "t1".into(),
        name: "echo".into(),
    })]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink, "test-model"))
        .unwrap();
    assert!(matches!(resp.finish_reason, FinishReason::ToolUse));
    assert_eq!(resp.tool_calls.len(), 1);
}

#[test]
fn warning_events_are_forwarded_to_sink_but_dont_appear_in_response() {
    let sink: Arc<CountingSink> = Arc::default();
    let stream = s(vec![
        Ok(StreamEvent::Warning {
            message: "rate limit nearing".into(),
        }),
        Ok(StreamEvent::TextDelta { text: "ok".into() }),
        Ok(StreamEvent::Done {
            finish: FinishReason::Stop,
            usage: Usage::default(),
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink.clone(), "test-model"))
        .unwrap();
    assert_eq!(resp.content.len(), 1);
    let seen = sink.events.lock().unwrap();
    assert_eq!(seen.len(), 3);
    assert!(matches!(seen[0], StreamEvent::Warning { .. }));
}

#[test]
fn first_error_propagates_and_terminates() {
    let sink: Arc<CountingSink> = Arc::default();
    let stream = s(vec![
        Ok(StreamEvent::TextDelta {
            text: "partial".into(),
        }),
        Err(LlmError::Stream("upstream gone".into())),
        // Should never be observed by the sink (stream short-circuits).
        Ok(StreamEvent::Done {
            finish: FinishReason::Stop,
            usage: Usage::default(),
        }),
    ]);
    let res = rt().block_on(accumulate_stream(stream, sink.clone(), "test-model"));
    assert!(matches!(res, Err(LlmError::Stream(_))));
    let seen = sink.events.lock().unwrap();
    // Sink got 1 successful event before the error terminated drain.
    assert_eq!(seen.len(), 1);
}

#[test]
fn out_of_order_tool_input_delta_is_recorded() {
    // ToolInputDelta arrives BEFORE its ToolUseStart — accumulator
    // should still credit the partial JSON to the right id.
    let sink = Arc::new(NullSink);
    let stream = s(vec![
        Ok(StreamEvent::ToolInputDelta {
            id: "t1".into(),
            partial_json: "{\"x\":1}".into(),
        }),
        Ok(StreamEvent::ToolUseStart {
            id: "t1".into(),
            name: "echo".into(),
        }),
        Ok(StreamEvent::Done {
            finish: FinishReason::ToolUse,
            usage: Usage::default(),
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink, "test-model"))
        .unwrap();
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].input, serde_json::json!({"x": 1}));
}

#[test]
fn tool_use_without_explicit_start_still_lands_in_response() {
    // Bedrock can send ToolUse(ToolCall) directly without a
    // preceding ToolUseStart. The accumulator must auto-register.
    let sink = Arc::new(NullSink);
    let stream = s(vec![
        Ok(StreamEvent::ToolUse(ToolCall {
            id: "t1".into(),
            name: "echo".into(),
            input: serde_json::json!({"a": 1}),
        })),
        Ok(StreamEvent::Done {
            finish: FinishReason::ToolUse,
            usage: Usage::default(),
        }),
    ]);
    let resp = rt()
        .block_on(accumulate_stream(stream, sink, "test-model"))
        .unwrap();
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "echo");
}
