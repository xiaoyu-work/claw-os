use super::*;
use crate::test_env::{lock_env, TestEnvVarGuard};

#[test]
fn task_session_reuse_requires_owner_and_refreshes_caps() {
    let _lock = lock_env();
    let temp = tempfile::tempdir().unwrap();
    let _data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let home = temp.path().join("home-owner");
    std::fs::create_dir_all(&home).unwrap();

    let trusted_client = SessionClient::new(SessionSource::BrokerTask, true, true);
    let session_id =
        create_task_session_with_client("test", 1001, &home, trusted_client).unwrap();
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
    assert_eq!(
        session::get_meta(&sid).unwrap().client,
        SessionClient {
            attended: false,
            ..trusted_client
        }
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
        attended_local: false,
    };
    let error = submit(json!({ "prompt": "hello" }), &root)
        .await
        .expect_err("a root-owned task must be refused");
    assert_eq!(error, crate::agentd::spawn::ROOT_OWNER_REFUSAL);
    assert!(error.contains("non-root"), "{error}");
}

fn attended_client() -> ClientIdentity {
    ClientIdentity {
        pid: Some(4242),
        uid: Some(1000),
        gid: Some(1000),
        start_time_ticks: Some(77),
        attended_local: true,
    }
}

#[test]
fn live_submission_presence_is_consumed_once() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    remember_submission_presence_at("task-live", &attended_client(), 1_000);
    let presence = claim_attended_presence_at(
        "task-live",
        1000,
        2_000,
        60_000,
        |pid, start, uid| pid == 4242 && start == 77 && uid == 1000,
    )
    .expect("live authenticated submitter should retain attendance");
    assert_eq!(presence.owner_uid, 1000);
    assert_eq!(presence.pid, 4242);
    assert_eq!(presence.expires_at_ms, 62_000);
    assert!(
        claim_attended_presence_at("task-live", 1000, 2_000, 60_000, |_, _, _| true).is_none()
    );
}

#[test]
fn submitter_exit_expires_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    remember_submission_presence_at("task-exit", &attended_client(), 1_000);
    assert!(claim_attended_presence_at(
        "task-exit",
        1000,
        2_000,
        60_000,
        |_, _, _| false
    )
    .is_none());
}

#[test]
fn queue_delay_expires_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    remember_submission_presence_at("task-delay", &attended_client(), 1_000);
    assert!(claim_attended_presence_at(
        "task-delay",
        1000,
        1_000 + SUBMISSION_PRESENCE_TTL_MS + 1,
        60_000,
        |_, _, _| true,
    )
    .is_none());
}

#[test]
fn daemon_restart_drops_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    remember_submission_presence_at("task-restart", &attended_client(), 1_000);
    clear_presence_leases();
    assert!(claim_attended_presence_at(
        "task-restart",
        1000,
        2_000,
        60_000,
        |_, _, _| true
    )
    .is_none());
}
