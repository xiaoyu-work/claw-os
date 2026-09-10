use super::*;
use serde_json::json;

#[test]
fn activity_v1_additions_have_defaults_and_ignore_future_fields() {
    let value = json!({
        "activity": {"id": "a", "title": "Goal", "goal": "A durable goal", "state": "active"},
        "future_field": true
    });
    let detail: ActivityDetailResponse = serde_json::from_value(value).unwrap();
    assert!(detail.jobs.is_empty());
    assert!(detail.sessions.is_empty());
    assert!(detail.pending_approvals.is_empty());
    assert!(detail.approvals_error.is_none());
    assert!(detail.activity.completion_criteria.is_empty());
    assert!(detail.activity.resources.is_empty());
    assert!(detail.activity.completion_note.is_none());
    assert_eq!(
        serde_json::from_value::<ActivityDetailResponse>(serde_json::to_value(&detail).unwrap())
            .unwrap(),
        detail
    );

    let job: ActivityJobView = serde_json::from_value(json!({
        "id": "j", "status": "ok", "future_field": "ignored"
    }))
    .unwrap();
    assert!(job.waiting_on.is_empty());
    assert!(job.response.is_none());
    let resource: ActivityResource = serde_json::from_value(json!({
        "label": "Notes", "reference": "notes.txt", "future_field": "ignored"
    }))
    .unwrap();
    assert_eq!(resource.reference, "notes.txt");
}

#[test]
fn activity_requests_cannot_supply_owner_or_authority() {
    assert!(
        serde_json::from_value::<ActivityCreateRequest>(json!({
            "title": "Goal", "goal": "Do something", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityUpdateRequest>(json!({
            "goal": "Do something", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityTransitionRequest>(json!({
            "state": "completed", "completion_note": "Confirmed", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityRunRequest>(json!({
            "session_id": "s", "owner_uid": 0
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityRunRequest>(json!({
            "caps": ["fs:write:*"]
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<ActivityListQuery>(json!({
            "owner_uid": 0
        }))
        .is_err()
    );
}

#[test]
fn minimal_requests_keep_defaults_and_explicit_completion_note() {
    let create: ActivityCreateRequest =
        serde_json::from_value(json!({"title": "Goal", "goal": "Do something"})).unwrap();
    assert!(create.resources.is_empty());
    assert_eq!(
        serde_json::to_value(ActivityRunRequest::default()).unwrap(),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(ActivityUpdateRequest::default()).unwrap(),
        json!({})
    );
    let transition = ActivityTransitionRequest {
        state: ActivityState::Completed,
        completion_note: Some("I verified the result".into()),
    };
    assert_eq!(
        serde_json::to_value(transition).unwrap(),
        json!({"state": "completed", "completion_note": "I verified the result"})
    );
    assert!(serde_json::from_value::<ActivityState>(json!("success")).is_err());
}

#[test]
fn job_acknowledgement_has_no_activity_lifecycle_or_worker_state() {
    let work: ActivityWorkResponse = serde_json::from_value(json!({
        "id": "job", "status": "ok", "owner_uid": 1000,
        "worker_pid": 100, "state": "completed"
    }))
    .unwrap();
    let value = serde_json::to_value(work).unwrap();
    assert!(value.get("state").is_none());
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("worker_pid").is_none());
    assert!(value["activity_id"].is_null());
}
