use super::*;
use crate::agent::llm::ToolCall;

#[test]
fn generated_sessions_get_independent_turn_leases() {
    let state = AppState::new(crate::config::AgentConfig::default(), 1000);
    let (first_id, _first_lease) =
        begin_turn(&state, None).unwrap_or_else(|_| panic!("first generated turn was rejected"));
    let (second_id, _second_lease) =
        begin_turn(&state, None).unwrap_or_else(|_| panic!("second generated turn was rejected"));

    assert_ne!(first_id, second_id);
}

#[tokio::test]
async fn active_session_post_returns_conflict_until_turn_finishes() {
    let state = AppState::new(crate::config::AgentConfig::default(), 1000);
    let session_id = format!("parallel-post-{}", uuid::Uuid::new_v4().simple());

    let first_response = handler(
        State(state.clone()),
        Json(ChatRequest {
            prompt: "first request".into(),
            session_id: Some(session_id.clone()),
            use_memory: true,
        }),
    )
    .await;
    assert_eq!(first_response.status(), StatusCode::OK);

    let second_response = handler(
        State(state.clone()),
        Json(ChatRequest {
            prompt: "parallel request".into(),
            session_id: Some(session_id.clone()),
            use_memory: true,
        }),
    )
    .await;
    assert_eq!(second_response.status(), StatusCode::CONFLICT);
    let conflict_body = axum::body::to_bytes(second_response.into_body(), 4096)
        .await
        .unwrap();
    let conflict: serde_json::Value = serde_json::from_slice(&conflict_body).unwrap();
    assert_eq!(conflict["code"], "session_busy");
    assert_eq!(conflict["session_id"], session_id);

    drop(first_response);
    let retry_lease = tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            if let Ok(lease) = state.try_acquire_turn(session_id.clone()) {
                break lease;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled request did not release its turn lease");
    drop(retry_lease);
}

#[test]
fn user_facing_tool_events_omit_inputs_and_results() {
    let (tx, mut rx) = mpsc::channel(16);
    let sink = SseSink::new(tx, "redaction-test".into());

    sink.on_event(&StreamEvent::ToolInputDelta {
        id: "call-1".into(),
        partial_json: r#"{"secret":"input"}"#.into(),
    });
    sink.on_event(&StreamEvent::ToolUse(ToolCall {
        id: "call-1".into(),
        name: "cos_sysinfo".into(),
        input: json!({"secret": "input"}),
    }));
    sink.on_tool_start(
        "call-1",
        "cos_sysinfo",
        &json!({"secret": "input"}),
    );
    sink.on_tool_result(
        "call-1",
        "cos_sysinfo",
        true,
        12,
        128,
        "secret result",
    );
    drop(sink);

    let mut output = String::new();
    while let Ok(frame) = rx.try_recv() {
        output.push_str(std::str::from_utf8(&frame.unwrap()).unwrap());
    }
    assert!(output.contains("cos_sysinfo"));
    assert!(!output.contains("secret"));
    assert!(!output.contains("\"input\""));
    assert!(!output.contains("preview"));
    assert!(!output.contains("tool_input_delta"));
}

#[test]
fn closed_channel_send_signals_runtime_interrupt() {
    let session_id = format!("closed-{}", uuid::Uuid::new_v4().simple());
    let interrupt = runtime::interrupt::register(&session_id);
    let (tx, rx) = mpsc::channel(1);
    drop(rx);

    let sink = SseSink::new(tx, session_id);
    sink.on_event(&StreamEvent::TextDelta {
        text: "unread".into(),
    });

    assert!(interrupt.check(), "closed SSE channel must cancel runtime");
}

#[test]
fn full_channel_send_signals_runtime_interrupt() {
    let session_id = format!("full-{}", uuid::Uuid::new_v4().simple());
    let interrupt = runtime::interrupt::register(&session_id);
    let (tx, _rx) = mpsc::channel(1);
    tx.try_send(Ok(bytes::Bytes::from_static(b"occupied")))
        .unwrap();

    let sink = SseSink::new(tx, session_id);
    sink.on_event(&StreamEvent::TextDelta {
        text: "overflow".into(),
    });

    assert!(interrupt.check(), "full SSE channel must cancel runtime");
}

#[tokio::test]
async fn bounded_sink_waits_for_receiver_capacity() {
    let (tx, mut rx) = mpsc::channel(1);
    tx.try_send(Ok(bytes::Bytes::from_static(b"occupied")))
        .unwrap();
    let sink = SseSink::new(tx, "backpressure-test".into());
    let mut ready = StreamSink::wait_ready(&sink).expect("SSE sink must apply backpressure");

    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut ready)
            .await
            .is_err(),
        "a full bounded channel must back-pressure the producer"
    );

    rx.recv().await.unwrap().unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut ready)
            .await
            .unwrap(),
        "producer should resume after the client drains one frame"
    );
}

#[tokio::test]
async fn response_drop_signals_interrupt_and_aborts_drive_task() {
    struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for DropSignal {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }

    let session_id = format!("disconnect-{}", uuid::Uuid::new_v4().simple());
    let interrupt = runtime::interrupt::register(&session_id);
    let state = AppState::new(crate::config::AgentConfig::default(), 1000);
    let lease = state.try_acquire_turn(session_id.clone()).unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    let drive = tokio::spawn(async move {
        let _lease = lease;
        let _drop_signal = DropSignal(Some(dropped_tx));
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });
    started_rx.await.unwrap();

    let (_tx, rx) = mpsc::channel(1);
    let stream = ReceiverStream::new(rx, CancelOnDrop::new(session_id.clone(), drive));
    drop(stream);

    tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
        .await
        .expect("drive task was not aborted")
        .unwrap();
    assert!(
        interrupt.check(),
        "dropping the response body must signal runtime cancellation"
    );
    assert!(
        state.try_acquire_turn(session_id).is_ok(),
        "disconnect cancellation must release the session turn lease"
    );
}
