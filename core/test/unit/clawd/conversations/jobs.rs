use super::*;
use serde_json::json;

fn recorded_job(id: &str, sid: &SessionId, owner_uid: Option<u32>, index: i64) -> Job {
    serde_json::from_value(json!({
        "id": id,
        "session_id": sid.as_str(),
        "owner_uid": owner_uid,
        "prompt": format!("recorded prompt {index}"),
        "status": "error",
        "created_at": chrono::DateTime::from_timestamp_millis(1_700_000_000_000 + index)
            .unwrap().to_rfc3339(),
        "error": "recorded failure",
        "response": "Assistant text claiming success",
        "context": "private context",
        "branch_context": "private branch context",
        "owner_home": "/home/private-owner",
        "worker_pid": 123,
    }))
    .unwrap()
}

#[test]
fn conversation_jobs_are_owner_and_session_scoped_actual_records() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::with_root(root.path().join("jobs")).unwrap();
    let sid = SessionId::generate();
    let other = SessionId::generate();
    for job in [
        recorded_job("owned", &sid, Some(1001), 0),
        recorded_job("other-session", &other, Some(1001), 1),
        recorded_job("foreign", &sid, Some(1002), 2),
        recorded_job("ownerless", &sid, None, 3),
    ] {
        std::fs::write(
            store.root().join("done").join(format!("{}.json", job.id)),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
    }

    let projection = collect(&store, &sid, 1001).unwrap();
    assert_eq!(projection.job_count, 1);
    assert!(!projection.jobs_truncated);
    assert!(!projection.task_bindings_complete);
    assert_eq!(
        projection.task_bindings_error.as_deref(),
        Some(UNVERIFIED_BINDINGS)
    );
    assert_eq!(projection.jobs[0].id, "owned");
    assert_eq!(projection.jobs[0].status, JobStatus::Error);
    assert_eq!(projection.jobs[0].session_id, sid.as_str());
    let value = serde_json::to_value(&projection.jobs[0]).unwrap();
    for hidden in [
        "owner_uid",
        "owner_home",
        "context",
        "branch_context",
        "worker_pid",
        "response",
        "evidence",
    ] {
        assert!(
            value.get(hidden).is_none(),
            "{hidden} must not be copied into metadata"
        );
    }
}

#[test]
fn conversation_jobs_are_bounded_and_keep_recent_records() {
    let sid = SessionId::generate();
    let records = (0..MAX_JOBS + 5)
        .map(|index| recorded_job(&format!("job-{index}"), &sid, Some(1001), index as i64))
        .collect();
    let projection = bounded_projection(records, &sid).unwrap();
    assert_eq!(projection.job_count, MAX_JOBS as u64 + 5);
    assert!(projection.jobs_truncated);
    assert_eq!(projection.jobs.len(), MAX_JOBS);
    assert_eq!(projection.jobs[0].id, "job-5");
    assert_eq!(
        projection.jobs.last().unwrap().id,
        format!("job-{}", MAX_JOBS + 4)
    );

    let mut large = (0..6)
        .map(|index| recorded_job(&format!("large-{index}"), &sid, Some(1001), index))
        .collect::<Vec<_>>();
    for job in &mut large {
        job.prompt = "x".repeat(512 * 1024);
    }
    let projection = bounded_projection(large, &sid).unwrap();
    assert_eq!(projection.job_count, 6);
    assert!(projection.jobs_truncated);
    assert!(!projection.jobs.is_empty());
    assert!(projection.jobs.len() < 6);
    assert!(serde_json::to_vec(&projection.jobs).unwrap().len() <= MAX_JOB_BYTES);
    assert_eq!(projection.jobs.last().unwrap().id, "large-5");
}

#[test]
fn conversation_jobs_reject_invalid_dates_and_cross_session_records() {
    let sid = SessionId::generate();
    let other = SessionId::generate();
    let mut invalid = recorded_job("invalid-date", &sid, Some(1001), 0);
    invalid.started_at = Some("invalid".to_string());
    assert!(bounded_projection(vec![invalid], &sid)
        .unwrap_err()
        .contains("timestamp"));
    assert!(bounded_projection(
        vec![recorded_job("wrong-session", &other, Some(1001), 0)],
        &sid
    )
    .is_err());
}
