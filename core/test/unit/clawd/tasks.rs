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

fn fresh_root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn pending_job() -> Job {
    Job::new_pending_with_client(
        "test".to_string(),
        None,
        None,
        Some("session-a".to_string()),
        None,
        Some(1000),
        Some("/home/test".to_string()),
        SessionClient::new(SessionSource::BrokerTask, false, true),
    )
}

#[test]
fn live_submission_presence_is_consumed_once() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = pending_job();
    let task_id = job.id.clone();
    with_presence_publication(&task_id, &attended_client(), 1_000, || store.publish(job))
        .unwrap();
    let (_, presence) = claim_job_with_presence_at(&store, 2_000, 60_000, |pid, start, uid| {
        pid == 4242 && start == 77 && uid == 1000
    })
    .unwrap()
    .expect("published task");
    let presence = presence
    .expect("live authenticated submitter should retain attendance");
    assert_eq!(presence.owner_uid, 1000);
    assert_eq!(presence.pid, 4242);
    assert_eq!(presence.expires_at_ms, 62_000);
}

#[test]
fn submitter_exit_expires_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = pending_job();
    let task_id = job.id.clone();
    with_presence_publication(&task_id, &attended_client(), 1_000, || store.publish(job))
        .unwrap();
    let (_, presence) =
        claim_job_with_presence_at(&store, 2_000, 60_000, |_, _, _| false)
            .unwrap()
            .expect("published task");
    assert!(presence.is_none());
}

#[test]
fn queue_delay_expires_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = pending_job();
    let task_id = job.id.clone();
    with_presence_publication(&task_id, &attended_client(), 1_000, || store.publish(job))
        .unwrap();
    let (_, presence) = claim_job_with_presence_at(
        &store,
        1_000 + SUBMISSION_PRESENCE_TTL_MS + 1,
        60_000,
        |_, _, _| true,
    )
    .unwrap()
    .expect("published task");
    assert!(presence.is_none());
}

#[test]
fn daemon_restart_drops_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = pending_job();
    let task_id = job.id.clone();
    with_presence_publication(&task_id, &attended_client(), 1_000, || store.publish(job))
        .unwrap();
    clear_presence_leases();
    let (_, presence) =
        claim_job_with_presence_at(&store, 2_000, 60_000, |_, _, _| true)
            .unwrap()
            .expect("published task");
    assert!(presence.is_none());
}

#[test]
fn publication_and_claim_are_atomic_with_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let publisher_store = store.clone();
    let job = pending_job();
    let task_id = job.id.clone();
    let client = attended_client();
    let (installed_tx, installed_rx) = std::sync::mpsc::channel();
    let (publish_tx, publish_rx) = std::sync::mpsc::channel();
    let publisher = std::thread::spawn(move || {
        with_presence_publication(&task_id, &client, 1_000, || {
            installed_tx.send(()).unwrap();
            publish_rx.recv().unwrap();
            publisher_store.publish(job)
        })
        .unwrap();
    });
    installed_rx.recv().unwrap();

    let claimant_store = store.clone();
    let (claimed_tx, claimed_rx) = std::sync::mpsc::channel();
    let claimant = std::thread::spawn(move || {
        let claimed =
            claim_job_with_presence_at(&claimant_store, 2_000, 60_000, |_, _, _| true)
                .unwrap();
        claimed_tx.send(claimed).unwrap();
    });
    assert!(
        claimed_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "claim must wait until presence and the pending file are published together"
    );
    publish_tx.send(()).unwrap();
    publisher.join().unwrap();
    let (_, presence) = claimed_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap()
        .expect("published task");
    claimant.join().unwrap();
    assert!(presence.is_some());
}

#[test]
fn failed_publication_removes_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    let result = with_presence_publication(
        "task-failed",
        &attended_client(),
        1_000,
        || Err::<(), _>("write failed"),
    );
    assert_eq!(result, Err("write failed"));
    let leases = presence_leases()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(!leases.contains_key("task-failed"));
}

#[test]
fn recovered_job_cannot_reuse_consumed_presence() {
    let _lock = crate::caps::test_env_lock::env_lock();
    clear_presence_leases();
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = pending_job();
    let task_id = job.id.clone();
    store.publish(job).unwrap();
    let claimed = store.claim_one().unwrap().expect("first claim");
    store
        .release_for_retry(&claimed.id, "worker exited")
        .unwrap()
        .expect("released");
    // Simulate the old publish/lease race completing after recovery. Even a
    // still-live late lease must not make a retried attempt attended.
    with_presence_publication(&task_id, &attended_client(), 2_500, || Ok::<(), ()>(()))
        .unwrap();
    let (_, retry_presence) =
        claim_job_with_presence_at(&store, 3_000, 60_000, |_, _, _| true)
            .unwrap()
            .expect("retry claim");
    assert!(retry_presence.is_none());
}
