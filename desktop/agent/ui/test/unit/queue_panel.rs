use super::*;

fn follow_up(id: &str) -> FollowUp {
    FollowUp {
        id: id.into(),
        prompt: id.into(),
        status: "pending".into(),
        after_task_id: "current".into(),
        created_at: "2026-10-15T12:00:00Z".into(),
    }
}

#[test]
fn queue_confirmation_keeps_the_displayed_selection_and_rejects_late_results() {
    crate::localize::localize();
    let mut state = QueuePanel {
        task_id: Some("current".into()),
        queue: Some(FollowUpQueue {
            task_id: "current".into(),
            session_id: "session".into(),
            tasks: vec![follow_up("next")],
            truncated: false,
        }),
        ..Default::default()
    };
    let _ = state.update(Message::AskAll, None);
    state
        .queue
        .as_mut()
        .unwrap()
        .tasks
        .push(follow_up("new-arrival"));
    assert_eq!(state.selection.as_ref().unwrap().len(), 1);
    let _ = state.view();
    let previous = state.generation;
    let _ = state.update(Message::Poll(None), None);
    assert!(state.selection.is_none());
    let _ = state.update(
        Message::Loaded(
            previous,
            Ok(FollowUpQueue {
                task_id: "current".into(),
                session_id: "session".into(),
                tasks: vec![follow_up("late")],
                truncated: false,
            }),
        ),
        None,
    );
    assert!(state.queue.is_none());
}
