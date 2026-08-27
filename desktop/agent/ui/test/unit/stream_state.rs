use super::*;
use cos_agent_protocol::{DeltaPayload, DonePayload, TaskStarted};

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
    assert!(sessions.active().unwrap().messages.last().unwrap().content.is_empty());
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
    assert_eq!(
        stream.cancel_finished(0, 1, Ok(()), &mut sessions),
        Some(0)
    );
    assert!(!stream.is_cancelling());
}
