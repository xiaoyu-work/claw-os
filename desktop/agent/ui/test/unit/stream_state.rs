use super::*;
use crate::session::TokenUsage;
use cos_agent_protocol::{
    DeltaPayload, DonePayload, ReasoningPayload, TaskStarted, ToolResultPayload, ToolStartPayload,
    TurnDonePayload, Usage,
};

fn active_stream() -> (StreamState, SessionState, u64) {
    crate::localize::localize();
    let mut sessions = SessionState::default();
    let session = sessions.begin_stream("hello".into());
    let (abort, _) = AbortHandle::new_pair();
    let mut stream = StreamState::default();
    let generation = stream.start(session.index, abort);
    (stream, sessions, generation)
}

#[test]
fn stale_events_do_not_mutate_the_active_generation() {
    let (mut stream, mut sessions, generation) = active_stream();
    let reduction = stream.reduce(
        generation.wrapping_add(1),
        StreamEvent::Delta(DeltaPayload::new("stale")),
        &mut sessions,
    );

    assert_eq!(reduction, StreamReduction::Stale);
    assert!(stream.is_active());
    assert!(
        sessions
            .active()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content
            .is_empty()
    );
}

#[test]
fn detach_aborts_only_the_local_stream_and_rejects_late_events() {
    let (mut stream, mut sessions, generation) = active_stream();
    stream.reduce(
        generation,
        StreamEvent::TaskStarted(TaskStarted {
            task_id: "task-1".into(),
            session_id: Some("remote-1".into()),
        }),
        &mut sessions,
    );

    assert_eq!(stream.detach(), Some(0));
    assert!(!stream.is_active());
    assert_eq!(
        stream.reduce(
            generation,
            StreamEvent::Delta(DeltaPayload::new("late")),
            &mut sessions,
        ),
        StreamReduction::Stale
    );
}

#[test]
fn done_is_terminal_and_finalizes_the_assistant() {
    let (mut stream, mut sessions, generation) = active_stream();
    stream.reduce(
        generation,
        StreamEvent::Delta(DeltaPayload::new("answer")),
        &mut sessions,
    );
    let reduction = stream.reduce(
        generation,
        StreamEvent::Done(DonePayload {
            event_type: "done".into(),
            task_id: "task-1".into(),
            session_id: Some("remote-1".into()),
            answer: None,
            response: None,
            turns_used: None,
            provider: None,
            model: None,
        }),
        &mut sessions,
    );

    assert_eq!(reduction, StreamReduction::Terminal);
    assert!(!stream.is_active());
    let session = sessions.active().unwrap();
    assert_eq!(session.remote_id.as_deref(), Some("remote-1"));
    assert!(!session.messages.last().unwrap().in_progress);
}

#[test]
fn presentation_events_accumulate_reasoning_usage_and_safe_tool_metrics() {
    let (mut stream, mut sessions, generation) = active_stream();
    stream.reduce(
        generation,
        StreamEvent::Reasoning(ReasoningPayload {
            summary: vec!["Checking context".into(), String::new()],
        }),
        &mut sessions,
    );
    stream.reduce(
        generation,
        StreamEvent::Reasoning(ReasoningPayload {
            summary: vec!["Checking context".into(), "Comparing sources".into()],
        }),
        &mut sessions,
    );
    for usage in [
        Usage {
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: 40,
            cache_write_tokens: 0,
        },
        Usage {
            input_tokens: 30,
            output_tokens: 10,
            cache_read_tokens: 0,
            cache_write_tokens: 5,
        },
    ] {
        stream.reduce(
            generation,
            StreamEvent::TurnDone(TurnDonePayload {
                finish: Some("tool_use".into()),
                usage,
            }),
            &mut sessions,
        );
    }
    stream.reduce(
        generation,
        StreamEvent::ToolStart(ToolStartPayload {
            kind: Some("tool_start".into()),
            id: "tool-1".into(),
            name: "fs.read".into(),
            input: None,
        }),
        &mut sessions,
    );
    stream.reduce(
        generation,
        StreamEvent::ToolResult(ToolResultPayload {
            id: "tool-1".into(),
            name: "fs.read".into(),
            ok: Some(true),
            output: Some("successful body must remain hidden".into()),
            latency_ms: Some(25),
            bytes_returned: Some(512),
            error_preview: Some("not an error".into()),
            ..ToolResultPayload::default()
        }),
        &mut sessions,
    );

    let message = sessions.active().unwrap().messages.last().unwrap();
    assert_eq!(message.reasoning, ["Checking context", "Comparing sources"]);
    assert_eq!(
        message.usage,
        Some(TokenUsage {
            input_tokens: 130,
            output_tokens: 30,
            cache_read_tokens: 40,
            cache_write_tokens: 5,
        })
    );
    assert_eq!(message.tool_results.len(), 1);
    let result = &message.tool_results[0];
    assert!(!result.is_error);
    assert_eq!(result.latency_ms, Some(25));
    assert_eq!(result.bytes_returned, Some(512));
    assert!(result.text.is_empty());
    assert!(result.error_preview.is_none());
}

#[test]
fn cancellation_waits_for_task_identity_then_rejects_late_events() {
    let (mut stream, mut sessions, generation) = active_stream();
    assert!(matches!(
        stream.request_cancel(&mut sessions),
        Some(CancelRequest::AwaitTask)
    ));
    assert!(stream.request_cancel(&mut sessions).is_none());
    assert!(stream.is_cancelling());
    let reduction = stream.reduce(
        generation,
        StreamEvent::TaskStarted(TaskStarted {
            task_id: "task-1".into(),
            session_id: Some("remote-1".into()),
        }),
        &mut sessions,
    );
    assert!(matches!(
        reduction,
        StreamReduction::CancelRemote {
            ref task_id,
            session_index: 0,
            message_index: 1
        } if task_id == "task-1"
    ));
    assert_eq!(
        stream.reduce(
            generation,
            StreamEvent::Delta(DeltaPayload::new("late")),
            &mut sessions,
        ),
        StreamReduction::Stale
    );
    assert!(
        !sessions
            .active()
            .unwrap()
            .messages
            .last()
            .unwrap()
            .content
            .contains("late")
    );
    assert_eq!(stream.cancel_finished(0, 1, Ok(()), &mut sessions), Some(0));
    assert!(!stream.is_cancelling());
}
