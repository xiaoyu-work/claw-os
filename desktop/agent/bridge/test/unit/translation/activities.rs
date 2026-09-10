use super::*;
use serde_json::json;

fn metadata() -> Value {
    json!({
        "id": "activity-1",
        "owner_uid": 1000,
        "title": "Prepare a release",
        "goal": "Publish a verified release",
        "completion_criteria": "Tests and review",
        "boundaries": "Ask before publishing",
        "resources": [{"label": "Notes", "reference": "file:notes", "future": true}],
        "state": "active",
        "completion_note": null,
        "created_at": "2026-09-10T12:00:00Z",
        "updated_at": "2026-09-10T12:00:00Z",
        "private_future_field": "hidden"
    })
}

fn detail_value() -> Value {
    json!({
        "schema": 1,
        "activity": metadata(),
        "jobs": [{
            "id": "job-1", "title": "Check release", "status": "ok",
            "session_id": "session-1",
            "created_at": "2026-09-10T12:00:00Z",
            "finished_at": "2026-09-10T12:01:00Z",
            "response": "Checks passed", "error": null,
            "waiting_on": ["approval-1"], "worker_pid": 10,
            "prompt": "not part of the desktop contract", "owner_uid": 1000
        }],
        "sessions": ["session-1"]
    })
}

#[test]
fn activity_projection_is_explicit_and_resources_remain_inert_text() {
    let value = serde_json::to_value(activity(metadata()).unwrap()).unwrap();
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("private_future_field").is_none());
    assert!(value["resources"][0].get("future").is_none());
    assert_eq!(value["resources"][0]["reference"], "file:notes");
    assert_eq!(value["boundaries"], "Ask before publishing");
}

#[test]
fn successful_jobs_do_not_complete_the_activity() {
    let response = detail(detail_value()).unwrap();
    assert_eq!(response.activity.state, ActivityState::Active);
    assert!(response.activity.completion_note.is_none());
    assert_eq!(response.jobs[0].status, "ok");
    assert_eq!(response.jobs[0].waiting_on, ["approval-1"]);
    let job = serde_json::to_value(&response.jobs[0]).unwrap();
    assert!(job.get("prompt").is_none());
    assert!(job.get("worker_pid").is_none());
    assert!(job.get("owner_uid").is_none());
}

#[test]
fn detail_previews_are_unicode_safe_and_bounded() {
    let mut value = detail_value();
    value["jobs"][0]["response"] = json!("é".repeat(PREVIEW_CHARS + 10));
    value["jobs"][0]["error"] = json!("失".repeat(PREVIEW_CHARS + 10));
    let response = detail(value).unwrap();
    for preview in [&response.jobs[0].response, &response.jobs[0].error] {
        let preview = preview.as_ref().unwrap();
        assert_eq!(preview.chars().count(), PREVIEW_CHARS + 1);
        assert!(preview.ends_with('…'));
    }
}

#[test]
fn unsupported_schema_and_malformed_metadata_are_not_success() {
    assert!(list(json!({"schema": 2, "activities": []})).is_err());
    let mut value = detail_value();
    value["schema"] = json!(2);
    assert!(detail(value).is_err());
    let mut value = metadata();
    value["state"] = json!("succeeded");
    assert!(activity(value).is_err());
    let mut value = metadata();
    value["id"] = json!("");
    assert!(activity(value).is_err());
    assert!(work(json!({"id": "", "status": "queued"})).is_err());
}

#[test]
fn approval_projection_is_limited_to_associated_sessions() {
    let response = detail(detail_value()).unwrap();
    let approvals = approvals(
        json!({"requests": [
            {"id": "a", "session": "session-1", "verb": "fs.write", "reason": "Publish",
             "owner_uid": 1000, "meta": {"label": "Write files", "risk": "high"}},
            {"id": "b", "session": "unrelated", "verb": "fs.write", "reason": "Unrelated"},
            {"id": "c", "session": "", "verb": "fs.write", "reason": "Unassociated"}
        ]}),
        &response,
    )
    .unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].id, "a");
    assert_eq!(approvals[0].label, "Write files");
    assert!(
        serde_json::to_value(&approvals[0])
            .unwrap()
            .get("owner_uid")
            .is_none()
    );
    assert!(super::approvals(json!({"invalid": []}), &response).is_err());
}

#[test]
fn work_acknowledgement_keeps_identity_without_private_job_payloads() {
    let response = work(json!({
        "id": "job-1", "status": "queued", "session_id": "session-1",
        "activity_id": "activity-1", "owner_uid": 1000, "prompt": "hidden", "worker_pid": 10
    }))
    .unwrap();
    let value = serde_json::to_value(response).unwrap();
    assert_eq!(value["activity_id"], "activity-1");
    assert!(value.get("prompt").is_none());
    assert!(value.get("owner_uid").is_none());
    assert!(value.get("worker_pid").is_none());
    assert!(value.get("state").is_none());
}
