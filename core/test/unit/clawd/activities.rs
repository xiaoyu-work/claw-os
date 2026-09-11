use super::*;
use crate::clawd::protocol::BrokerErrorKind;
use crate::test_env::TestEnvVarGuard;

fn peer(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(uid),
        start_time_ticks: Some(1),
    }
}

fn draft() -> Value {
    json!({
        "title": "Release v2",
        "goal": "Publish next Friday",
        "completion_criteria": "The release is available",
        "boundaries": "Ask before public publication",
        "resources": [{"label": "Draft", "reference": "/home/user/release.md"}],
    })
}

#[test]
fn activity_broker_uses_one_persistent_owner_scoped_backend() {
    let dir = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", dir.path());
    let created = create(draft(), &peer(1000)).unwrap();
    let id = created["id"].as_str().unwrap();
    assert_eq!(created["owner_uid"], 1000);
    assert_eq!(created["state"], "active");

    let shown = get(json!({"id": id}), &peer(1000)).unwrap();
    assert_eq!(shown["schema"], 1, "database migrations must not change the Activity wire contract");
    assert_eq!(shown["activity"], created);
    assert_eq!(shown["jobs"], json!([]));
    assert_eq!(shown["sessions"], json!([]));

    let edited = update(json!({"id": id, "title": "Release v2.1"}), &peer(1000)).unwrap();
    assert_eq!(edited["goal"], created["goal"]);
    assert_eq!(edited["resources"], created["resources"]);
    let listed = list(json!({"state": "active"}), &peer(1000)).unwrap();
    assert_eq!(listed["schema"], 1);
    assert_eq!(listed["activities"], json!([edited]));
    assert_eq!(
        list(json!({}), &peer(2000)).unwrap()["activities"],
        json!([])
    );
    assert_eq!(list(json!({}), &peer(0)).unwrap()["activities"], json!([]));

    let missing_id = uuid::Uuid::new_v4().to_string();
    for caller in [peer(2000), peer(0)] {
        let foreign = get(json!({"id": id}), &caller).unwrap_err();
        let missing = get(json!({"id": missing_id}), &caller).unwrap_err();
        assert_eq!(foreign.kind, missing.kind);
        assert_eq!(foreign.message, missing.message);
        assert!(update(json!({"id": id, "title": "other"}), &caller).is_err());
        assert!(transition(json!({"id": id, "state": "cancelled"}), &caller).is_err());
    }
}

#[test]
fn activity_completion_requires_confirmation_and_preserves_reopen_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", dir.path());
    let created = create(draft(), &peer(1000)).unwrap();
    let id = created["id"].as_str().unwrap();
    assert!(transition(json!({"id": id, "state": "completed"}), &peer(1000)).is_err());
    let paused = transition(json!({"id": id, "state": "paused"}), &peer(1000)).unwrap();
    assert_eq!(paused["state"], "paused");
    let completed = transition(
        json!({"id": id, "state": "completed", "completion_note": "Reviewed and published"}),
        &peer(1000),
    )
    .unwrap();
    assert_eq!(completed["completion_note"], "Reviewed and published");
    assert!(update(json!({"id": id, "goal": "New work"}), &peer(1000)).is_err());
    let reopened = transition(json!({"id": id, "state": "active"}), &peer(1000)).unwrap();
    assert_eq!(reopened["state"], "active");
}

#[tokio::test]
async fn inadmissible_activity_work_never_creates_a_task() {
    let dir = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", dir.path());
    let created = create(draft(), &peer(1000)).unwrap();
    let id = created["id"].as_str().unwrap();
    assert!(run(json!({"id": id}), &peer(2000)).await.is_err());
    transition(json!({"id": id, "state": "paused"}), &peer(1000)).unwrap();
    let error = run(json!({"id": id}), &peer(1000)).await.unwrap_err();
    assert_eq!(error.audit_class, Some("activity_not_active"));
    assert!(!crate::paths::agent_jobs_dir().exists());
}

#[test]
fn identity_and_invalid_input_are_rejected_before_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", dir.path());
    let error = create(draft(), &ClientIdentity::unknown()).unwrap_err();
    assert_eq!(error.kind, BrokerErrorKind::Unauthorized);
    let mut forged = draft();
    forged["owner_uid"] = json!(0);
    assert!(create(forged, &peer(1000)).is_err());
    assert!(create(json!({"title": "", "goal": "work"}), &peer(1000)).is_err());
    assert!(list(json!({"limit": 101}), &peer(1000)).is_err());
    assert!(get(json!({"id": "not-a-uuid"}), &peer(1000)).is_err());
    assert!(transition(json!({"id": "not-a-uuid", "state": "paused"}), &peer(1000)).is_err());
    assert!(!dir.path().join("activities.db").exists());
}

#[test]
fn activity_storage_errors_remain_unavailable() {
    let error = service_error(ActivityError::Database(rusqlite::Error::InvalidQuery));
    assert_eq!(error.kind, BrokerErrorKind::Unavailable);
    assert_eq!(error.audit_class, Some("activity_unavailable"));
}

#[test]
fn activity_job_view_reports_execution_without_claiming_goal_achievement() {
    let job: Job = serde_json::from_value(json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "prompt": "Prepare a release",
        "status": "ok",
        "created_at": "2026-09-10T00:00:00Z",
        "response": "I reached the work limit; the release is not published.",
    }))
    .unwrap();
    let view = job_view(&job);
    assert_eq!(view["status"], "ok");
    assert!(view["response"].as_str().unwrap().contains("not published"));
    assert!(view.get("activity_state").is_none());
    assert!(view.get("goal_achieved").is_none());
    assert!(preview(&"a".repeat(5000), 4096).ends_with("[open the task for the full result]"));
}
