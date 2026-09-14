use super::*;
use crate::agent::memory::conversation_bindings::MessageTaskBinding;
use crate::agent::service::JobStatus;

fn binding(message_id: i64, user_message_id: i64, task_id: &str) -> MessageTaskBinding {
    MessageTaskBinding {
        message_id,
        session_id: "ses_0000000000001_000000000001".to_string(),
        task_id: task_id.to_string(),
        user_message_id,
        source_session_id: "ses_0000000000001_000000000001".to_string(),
        source_message_id: message_id,
        source_user_message_id: user_message_id,
        is_user_prompt: message_id == user_message_id,
    }
}

fn job(id: &str) -> super::super::jobs::ConversationJob {
    super::super::jobs::ConversationJob {
        id: id.to_string(),
        status: JobStatus::Ok,
        prompt: format!("{id} prompt"),
        created_at: "2025-01-01T00:00:00Z".to_string(),
        started_at: None,
        finished_at: None,
        session_id: "ses_0000000000001_000000000001".to_string(),
        error: None,
    }
}

#[test]
fn complete_membership_verifies_and_orders_actual_jobs() {
    let session_id: SessionId = "ses_0000000000001_000000000001".parse().unwrap();
    let members = vec![
        binding(1, 1, "first"),
        binding(2, 1, "first"),
        binding(3, 3, "second"),
        binding(4, 3, "second"),
    ];
    let state = BindingState {
        originals: members.clone(),
        members,
        oversized: false,
    };
    let mut jobs = ConversationJobs {
        jobs: vec![job("second"), job("first")],
        job_count: 2,
        jobs_truncated: false,
        ..ConversationJobs::default()
    };

    verify(&session_id, &state, 4, &mut jobs).unwrap();
    assert!(jobs.task_bindings_complete);
    assert!(jobs.task_bindings_error.is_none());
    assert_eq!(jobs.jobs[0].id, "first");
    assert_eq!(jobs.jobs[1].id, "second");
}

#[test]
fn legacy_partial_and_unowned_job_sets_fail_closed() {
    let session_id: SessionId = "ses_0000000000001_000000000001".parse().unwrap();
    let mut jobs = ConversationJobs::default();
    assert!(verify(&session_id, &BindingState::default(), 1, &mut jobs).is_err());

    let members = vec![binding(1, 1, "task")];
    let partial = BindingState {
        members,
        originals: vec![binding(1, 1, "task"), binding(2, 1, "task")],
        oversized: false,
    };
    let mut jobs = ConversationJobs {
        jobs: vec![job("task")],
        job_count: 1,
        jobs_truncated: false,
        ..ConversationJobs::default()
    };
    assert!(verify(&session_id, &partial, 1, &mut jobs)
        .unwrap_err()
        .contains("partial task history"));

    let complete = BindingState {
        members: vec![binding(1, 1, "task")],
        originals: vec![binding(1, 1, "task")],
        oversized: false,
    };
    let mut no_owned_job = ConversationJobs::default();
    assert!(verify(&session_id, &complete, 1, &mut no_owned_job)
        .unwrap_err()
        .contains("job set"));
}
