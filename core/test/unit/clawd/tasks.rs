use super::*;
use crate::activities::{Activity, ActivityService, ActivityState};
use crate::test_env::{lock_env, TestEnvVarGuard};

fn activity_task_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(".activity-tasks-test-")
        .tempdir_in(".")
        .unwrap()
}

fn activity_task_client(uid: u32) -> ClientIdentity {
    ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(uid),
        gid: Some(unsafe { libc::getegid() } as u32),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
    }
}

fn activity_task_fixture(owner_uid: u32) -> Activity {
    crate::activities::open_default()
        .unwrap()
        .create(
            owner_uid,
            serde_json::from_value(json!({
                "title": "Prepare a release",
                "goal": "Prepare release notes",
                "completion_criteria": "The user accepts the notes",
                "boundaries": "Do not publish",
                "resources": [],
            }))
            .unwrap(),
        )
        .unwrap()
}

fn finish_activity_task(store: &Store, id: &str) {
    let claimed = store.claim_one().unwrap().unwrap();
    assert_eq!(claimed.id, id);
    store
        .finish(
            claimed,
            crate::agent::service::FinishOutcome::Error("test worker ended".to_string()),
        )
        .unwrap();
}

#[test]
fn task_list_summary_omits_heavy_and_private_fields() {
    let summary = task_summary_value(&json!({
        "id": "task-a",
        "prompt": "summarize this report",
        "status": "ok",
        "session_id": "session-a",
        "activity_id": "activity-a",
        "created_at": "2026-01-01T00:00:00Z",
        "response": "large response",
        "evidence": {"large": true},
        "owner_home": "/home/alice",
        "owner_uid": 1000,
    }));
    assert_eq!(summary["title"], "summarize this report");
    assert_eq!(summary["session_id"], "session-a");
    assert_eq!(summary["activity_id"], "activity-a");
    for hidden in ["prompt", "response", "evidence", "owner_home", "owner_uid"] {
        assert!(
            summary.get(hidden).is_none(),
            "{hidden} leaked into summary"
        );
    }
}

#[test]
fn task_session_reuse_requires_owner_and_refreshes_caps() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let home = temp.path().join("home-owner");
    std::fs::create_dir_all(&home).unwrap();

    let session_id = create_task_session("test", 1001, &home).unwrap();
    prepare_task_session(&session_id, 1001, &home)
        .expect("an empty task session must be reusable after an early worker failure");
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open(
        crate::paths::clawd_user_memory_db_path(1001),
    )
    .unwrap();
    db.record_message(&session_id, "user", "hello").unwrap();

    let sid = session_id.parse::<session::SessionId>().unwrap();
    // Provenance is stamped by the issuer, never by a request field.
    assert_eq!(
        session::get_meta(&sid).unwrap().origin,
        Some(SessionOrigin::SystemAgentTask)
    );
    session::set_caps(&sid, &crate::caps::CapSet::new()).unwrap();
    // A session that acquired a delegation marker is re-stamped as
    // ambient when it is resumed, so it can never be replayed as one.
    session::update_meta(&sid, |meta| {
        meta.origin = Some(SessionOrigin::TriggerDelegation);
    })
    .unwrap();
    prepare_task_session(&session_id, 1001, &home).unwrap();
    assert_eq!(
        session::get_meta(&sid).unwrap().origin,
        Some(SessionOrigin::SystemAgentTask)
    );
    let refreshed = session::get_caps(&sid).unwrap();
    assert!(refreshed.covers(&crate::caps::Cap::new(
        crate::caps::Verb::FS_READ,
        crate::caps::Scope::path(home.join("notes.md").to_string_lossy().into_owned())
    )));
    // The refresh restores daemon policy, not ambient authority.
    assert!(!refreshed.covers(&crate::caps::Cap::new(
        crate::caps::Verb::NET_DIAL,
        crate::caps::Scope::host("example.com")
    )));
    assert!(!refreshed.covers(&crate::caps::Cap::new(
        crate::caps::Verb::FS_READ,
        crate::caps::Scope::path("/etc/shadow")
    )));

    let error = prepare_task_session(&session_id, 1002, &home).unwrap_err();
    assert!(error.contains("not owned"));
    // Root is not exempt: resuming a task re-derives capabilities for
    // the resuming account, so it may not adopt another owner's.
    let error = prepare_task_session(&session_id, 0, &home).unwrap_err();
    assert!(error.contains("not owned"));
}

#[tokio::test]
async fn a_root_peer_cannot_submit_an_agent_task() {
    // The agent runtime runs unprivileged in `claw-agentd`, and root has
    // no account to drop to, so a root-owned task is refused where it is
    // submitted rather than becoming a queued task that can only fail.
    let root = ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(0),
        gid: Some(0),
        start_time_ticks: Some(1),
    };
    let error = submit(json!({ "prompt": "hello" }), &root)
        .await
        .expect_err("a root-owned task must be refused");
    assert_eq!(error, crate::agentd::spawn::ROOT_OWNER_REFUSAL);
    assert!(error.contains("non-root"), "{error}");
}

#[test]
fn retry_creates_a_new_pending_task_for_the_same_session() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let owner_home = crate::clawd::system_caps::verified_owner_home(owner_uid).unwrap();
    let session_id = create_task_session("retry test", owner_uid, &owner_home).unwrap();
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open(
        crate::paths::clawd_user_memory_db_path(owner_uid),
    )
    .unwrap();
    db.record_message(&session_id, "user", "retry me").unwrap();

    let store = Store::open_default().unwrap();
    let original = store
        .submit(
            "retry me".to_string(),
            Some(session_id.clone()),
            None,
            Some(owner_uid),
            Some(owner_home.to_string_lossy().into_owned()),
        )
        .unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    store
        .finish(
            claimed,
            crate::agent::service::FinishOutcome::Error("failed".into()),
        )
        .unwrap();
    let client = ClientIdentity {
        pid: Some(std::process::id()),
        uid: Some(owner_uid),
        gid: Some(unsafe { libc::getegid() } as u32),
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
    };

    let retried = retry(json!({ "id": original.id }), &client).unwrap();
    assert_ne!(retried["id"], original.id);
    assert_eq!(retried["status"], "pending");
    assert_eq!(retried["session_id"], session_id);
    assert_eq!(retried["prompt"], "retry me");
    assert!(retried.get("activity_id").is_none());
}

#[tokio::test]
async fn activity_continuation_and_retry_preserve_identity_without_relabeling_old_jobs() {
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let _lock = lock_env();
    let root = activity_task_root();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
    let client = activity_task_client(owner_uid);
    let activity = activity_task_fixture(owner_uid);
    let old = submit(json!({"prompt": "old unassociated turn"}), &client).await.unwrap();
    let store = Store::open_default().unwrap();
    finish_activity_task(&store, old["id"].as_str().unwrap());
    let session_id = old["session_id"].as_str().unwrap();

    let attached = submit(
        json!({
            "prompt": "associated turn",
            "context": "original app context",
            "session_id": session_id,
            "activity_id": activity.id.to_ascii_uppercase(),
            "use_memory": false,
        }),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(attached["activity_id"], activity.id);
    assert_eq!(
        session::get_meta(&session_id.parse().unwrap()).unwrap().activity_id.as_deref(),
        Some(activity.id.as_str())
    );
    assert!(store
        .locate(old["id"].as_str().unwrap())
        .unwrap()
        .unwrap()
        .1
        .activity_id
        .is_none());
    assert_eq!(
        store.list_for_activity(owner_uid, &activity.id, 100).unwrap().len(),
        1
    );
    finish_activity_task(&store, attached["id"].as_str().unwrap());

    let continuation = submit(
        json!({"prompt": "continue", "session_id": session_id}),
        &client,
    )
    .await
    .unwrap();
    assert_eq!(continuation["activity_id"], activity.id);
    store.cancel_pending(continuation["id"].as_str().unwrap()).unwrap().unwrap();
    let retried = retry(json!({"id": attached["id"]}), &client).unwrap();
    assert_eq!(retried["activity_id"], activity.id);
    assert_eq!(retried["session_id"], session_id);
    assert_eq!(retried["context"], "original app context");
    assert_eq!(retried["use_memory"], false);
    assert_ne!(retried["id"], attached["id"]);
    assert_eq!(
        crate::activities::open_default().unwrap().get(owner_uid, &activity.id).unwrap().state,
        ActivityState::Active
    );
}

#[tokio::test]
async fn invalid_or_foreign_activity_submission_has_no_session_or_job_side_effects() {
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let _lock = lock_env();
    let root = activity_task_root();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
    let client = activity_task_client(owner_uid);
    let foreign = activity_task_fixture(owner_uid + 1);
    for activity_id in [
        json!(""),
        json!(" "),
        json!("../invalid"),
        json!(uuid::Uuid::new_v4().to_string()),
        json!(foreign.id),
        json!(17),
    ] {
        assert!(submit(json!({"prompt": "refused", "activity_id": activity_id}), &client)
            .await
            .is_err());
        assert!(!session::sessions_root().exists());
    }
    let activity = activity_task_fixture(owner_uid);
    let sid = session::create("foreign session").unwrap();
    session::update_meta(&sid, |meta| {
        meta.owner_uid = Some(owner_uid + 1);
        meta.creator_runtime = Some("clawd".to_string());
    })
    .unwrap();
    let before = session::get_meta(&sid).unwrap();
    let error = submit(
        json!({"prompt": "refused", "activity_id": activity.id, "session_id": sid}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(error.contains("not owned"), "{error}");
    assert_eq!(session::get_meta(&sid).unwrap(), before);
    assert_eq!(Store::open_default().unwrap().counts().unwrap(), (0, 0, 0, 0));
}

#[tokio::test]
async fn activity_mismatch_is_rejected_before_refreshing_session_authority() {
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let _lock = lock_env();
    let root = activity_task_root();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
    let client = activity_task_client(owner_uid);
    let activity = activity_task_fixture(owner_uid);
    let other = activity_task_fixture(owner_uid);
    let original = submit(
        json!({"prompt": "first", "activity_id": activity.id}),
        &client,
    )
    .await
    .unwrap();
    let store = Store::open_default().unwrap();
    finish_activity_task(&store, original["id"].as_str().unwrap());
    let sid = original["session_id"].as_str().unwrap().parse::<session::SessionId>().unwrap();
    session::set_caps(&sid, &crate::caps::CapSet::new()).unwrap();
    session::update_meta(&sid, |meta| {
        meta.origin = Some(SessionOrigin::TriggerDelegation);
    })
    .unwrap();
    let before = session::get_meta(&sid).unwrap();
    let error = submit(
        json!({"prompt": "move", "session_id": sid, "activity_id": other.id}),
        &client,
    )
    .await
    .unwrap_err();
    assert!(error.contains("different Activity"), "{error}");
    assert_eq!(session::get_meta(&sid).unwrap(), before);
    assert_eq!(session::get_caps(&sid).unwrap(), crate::caps::CapSet::new());
    assert_eq!(store.counts().unwrap(), (0, 0, 0, 1));

    session::update_meta(&sid, |meta| meta.activity_id = Some(other.id.clone())).unwrap();
    let before = session::get_meta(&sid).unwrap();
    let error = retry(json!({"id": original["id"]}), &client).unwrap_err();
    assert!(error.contains("different Activity"), "{error}");
    assert_eq!(session::get_meta(&sid).unwrap(), before);
    assert_eq!(store.counts().unwrap(), (0, 0, 0, 1));
}

#[tokio::test]
async fn inactive_activity_refuses_explicit_inherited_and_retry_admission_before_mutation() {
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let _lock = lock_env();
    let root = activity_task_root();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
    let client = activity_task_client(owner_uid);
    let service = crate::activities::open_default().unwrap();
    let store = Store::open_default().unwrap();
    for state in [
        ActivityState::Paused,
        ActivityState::Completed,
        ActivityState::Cancelled,
    ] {
        let activity = activity_task_fixture(owner_uid);
        let original = submit(
            json!({"prompt": "first", "activity_id": activity.id}),
            &client,
        )
        .await
        .unwrap();
        finish_activity_task(&store, original["id"].as_str().unwrap());
        let sid = original["session_id"].as_str().unwrap().parse::<session::SessionId>().unwrap();
        session::set_caps(&sid, &crate::caps::CapSet::new()).unwrap();
        session::update_meta(&sid, |meta| {
            meta.origin = Some(SessionOrigin::TriggerDelegation);
        })
        .unwrap();
        let before = session::get_meta(&sid).unwrap();
        let counts = store.counts().unwrap();
        let sessions = session::list().unwrap().len();
        service
            .transition(
                owner_uid,
                &activity.id,
                state,
                (state == ActivityState::Completed).then(|| "User confirmed".to_string()),
            )
            .unwrap();
        for params in [
            json!({"prompt": "new", "activity_id": activity.id}),
            json!({"prompt": "continue", "session_id": sid}),
        ] {
            let error = submit(params, &client).await.unwrap_err();
            assert!(error.contains(state.as_str()), "{error}");
            assert_eq!(session::get_meta(&sid).unwrap(), before);
            assert_eq!(session::get_caps(&sid).unwrap(), crate::caps::CapSet::new());
            assert_eq!(session::list().unwrap().len(), sessions);
        }
        let error = retry(json!({"id": original["id"]}), &client).unwrap_err();
        assert!(error.contains(state.as_str()), "{error}");
        assert_eq!(session::get_meta(&sid).unwrap(), before);
        assert_eq!(store.counts().unwrap(), counts);
    }
}

#[tokio::test]
async fn associated_task_retry_does_not_grant_root_or_other_owners_activity_access() {
    let owner_uid = unsafe { libc::geteuid() } as u32;
    if owner_uid == 0 {
        return;
    }
    let _lock = lock_env();
    let root = activity_task_root();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
    let activity = activity_task_fixture(owner_uid);
    let original = submit(
        json!({"prompt": "first", "activity_id": activity.id}),
        &activity_task_client(owner_uid),
    )
    .await
    .unwrap();
    let store = Store::open_default().unwrap();
    finish_activity_task(&store, original["id"].as_str().unwrap());
    for uid in [0, owner_uid + 1] {
        assert!(retry(json!({"id": original["id"]}), &activity_task_client(uid)).is_err());
    }
    assert_eq!(store.counts().unwrap(), (0, 0, 0, 1));
}

#[test]
fn activity_task_list_filters_status_before_limit_and_preserves_owner_boundary() {
    let _lock = lock_env();
    let root = activity_task_root();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", root.path().canonicalize().unwrap());
    let activity = activity_task_fixture(1001);
    let other = activity_task_fixture(1001);
    let store = Store::open_default().unwrap();
    let failed = store
        .submit_with_activity(
            "first".to_string(), None, None, None, None, true, Some(1001), None,
            Some(activity.id.clone()),
        )
        .unwrap();
    finish_activity_task(&store, &failed.id);
    for activity_id in [&activity.id, &other.id] {
        store
            .submit_with_activity(
                "newer".to_string(), None, None, None, None, true, Some(1001), None,
                Some(activity_id.clone()),
            )
            .unwrap();
    }
    let client = activity_task_client(1001);
    let params = json!({
        "activity_id": activity.id.to_ascii_uppercase(),
        "status": "error",
        "summary": true,
        "limit": 1,
    });
    let result = list(params.clone(), &client).unwrap();
    assert_eq!(result["jobs"].as_array().unwrap().len(), 1);
    assert_eq!(result["jobs"][0]["id"], failed.id);
    assert_eq!(result["jobs"][0]["activity_id"], activity.id);
    assert_eq!(get(json!({"id": failed.id}), &client).unwrap()["activity_id"], activity.id);
    for uid in [0, 1002] {
        assert!(list(params.clone(), &activity_task_client(uid)).is_err());
    }
    crate::activities::open_default()
        .unwrap()
        .transition(1001, &activity.id, ActivityState::Completed, Some("User confirmed".to_string()))
        .unwrap();
    assert_eq!(list(params, &client).unwrap()["jobs"][0]["id"], failed.id);
}
