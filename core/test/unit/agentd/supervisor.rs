use super::*;

use crate::agent::service::{JobStatus, Store};
use crate::agentd::grant::GRANT_AUDIENCE;

fn new_lease() -> Lease {
    Lease {
        task_id: "task-a".to_string(),
        session_id: Some("session-a".to_string()),
        owner_uid: 1000,
        execution_gid: 1000,
        client: crate::session::SessionClient::new(
            crate::session::SessionSource::BrokerTask,
            true,
            true,
        ),
        presence: None,
        capability_generation: "caps-a".to_string(),
        extension: None,
        worker_pid: std::process::id(),
        worker_start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
        deadline: Instant::now() + Duration::from_secs(60),
        approval_expires_at: approval_deadline(Duration::from_secs(60)),
        approval_nonce: "0123456789abcdef".to_string(),
        consent_context: crate::caps::ConsentContext::Attended,
    }
}

fn signer_and_hello(lease: &Lease) -> (GrantSigner, WorkerFrame) {
    let signer = GrantSigner::from_secret([9u8; 32]);
    let grant = signer.issue(claims_for(
        std::process::id(),
        lease,
        Duration::from_secs(60),
    ));
    let hello = WorkerFrame::Hello(Box::new(WorkerHello {
        protocol: protocol::PROTOCOL_VERSION,
        grant,
        pid: lease.worker_pid,
        start_time_ticks: lease.worker_start_time_ticks,
        uid: lease.owner_uid,
        euid: lease.owner_uid,
        gid: 1000,
        egid: 1000,
        supplementary_groups: Vec::new(),
        no_new_privs: true,
    }));
    (signer, hello)
}

#[test]
fn the_handshake_is_required_before_any_other_route() {
    let mut lease = new_lease();
    let (signer, _hello) = signer_and_hello(&lease);
    let broker_pid = std::process::id();
    let result = WorkerFrame::Result {
        task_id: lease.task_id.clone(),
        outcome: Box::new(WorkerOutcome::Cancelled),
    };
    let error = accept(&signer, broker_pid, &mut lease, &result, false)
        .expect_err("a result before the handshake must be refused");
    assert!(error.contains("before the handshake"), "{error}");
    assert!(accept(&signer, broker_pid, &mut lease, &result, true).is_ok());
}

#[test]
fn a_worker_cannot_report_on_another_owners_task() {
    let mut lease = new_lease();
    let (signer, _hello) = signer_and_hello(&lease);
    let broker_pid = std::process::id();
    let stolen = WorkerFrame::Result {
        task_id: "task-b".to_string(),
        outcome: Box::new(WorkerOutcome::Cancelled),
    };
    let error = accept(&signer, broker_pid, &mut lease, &stolen, true)
        .expect_err("a frame for another task must be refused");
    assert!(error.contains("different task"), "{error}");
}

#[test]
fn a_grant_for_a_different_worker_is_refused_at_the_handshake() {
    let mut lease = new_lease();
    let signer = GrantSigner::from_secret([9u8; 32]);
    let mut claims = claims_for(std::process::id(), &lease, Duration::from_secs(60));
    claims.worker_pid = lease.worker_pid.wrapping_add(1);
    let hello = WorkerFrame::Hello(Box::new(WorkerHello {
        protocol: protocol::PROTOCOL_VERSION,
        grant: signer.issue(claims),
        pid: lease.worker_pid,
        start_time_ticks: lease.worker_start_time_ticks,
        uid: lease.owner_uid,
        euid: lease.owner_uid,
        gid: 1000,
        egid: 1000,
        supplementary_groups: Vec::new(),
        no_new_privs: true,
    }));
    let error = accept(&signer, std::process::id(), &mut lease, &hello, false)
        .expect_err("a grant bound to another pid must be refused");
    assert!(error.contains("worker pid"), "{error}");
}

#[test]
fn an_expired_lease_stops_accepting_frames() {
    let mut lease = new_lease();
    let (signer, _hello) = signer_and_hello(&lease);
    lease.deadline = Instant::now() - Duration::from_secs(1);
    let beat = WorkerFrame::Heartbeat {
        task_id: lease.task_id.clone(),
    };
    let error = accept(&signer, std::process::id(), &mut lease, &beat, true)
        .expect_err("an expired lease must stop the channel");
    assert!(error.contains("lease"), "{error}");
}

#[test]
fn approval_frames_require_a_nonzero_correlation_and_valid_nonce() {
    let mut lease = new_lease();
    let (signer, _hello) = signer_and_hello(&lease);
    let ask = ApprovalAsk::Consume {
        verb: crate::caps::Verb::FS_READ.as_str().to_string(),
        scope: crate::caps::Scope::path("/home/user/notes.txt"),
        operation_digest: None,
    };
    let invalid_nonce = WorkerFrame::Approval {
        task_id: lease.task_id.clone(),
        correlation_id: 1,
        exchange: protocol::ApprovalExchange {
            nonce: "predictable".to_string(),
            ask: ask.clone(),
        },
    };
    let error = accept(
        &signer,
        std::process::id(),
        &mut lease,
        &invalid_nonce,
        true,
    )
    .expect_err("an invalid nonce must be refused");
    assert!(error.contains("nonce"), "{error}");

    let zero_correlation = WorkerFrame::Approval {
        task_id: lease.task_id.clone(),
        correlation_id: 0,
        exchange: protocol::ApprovalExchange::new(ask.clone()),
    };
    let error = accept(
        &signer,
        std::process::id(),
        &mut lease,
        &zero_correlation,
        true,
    )
    .expect_err("correlation zero must be refused");
    assert!(error.contains("correlation"), "{error}");

    let valid = WorkerFrame::Approval {
        task_id: lease.task_id.clone(),
        correlation_id: 1,
        exchange: protocol::ApprovalExchange::new(ask),
    };
    assert!(accept(&signer, std::process::id(), &mut lease, &valid, true,).is_ok());
}

#[test]
fn the_grant_the_supervisor_mints_only_carries_worker_channel_routes() {
    let lease = new_lease();
    let claims = claims_for(std::process::id(), &lease, Duration::from_secs(60));
    assert_eq!(claims.audience, GRANT_AUDIENCE);
    assert_eq!(claims.routes, protocol::worker_routes());
    assert!(claims.expires_at_ms > claims.issued_at_ms);
    for route in &claims.routes {
        assert!(crate::clawd::routes::Command::parse(route).is_none());
    }
}

#[test]
fn a_worker_that_did_not_shed_privilege_is_rejected() {
    let lease = new_lease();
    let mut hello = WorkerHello {
        protocol: protocol::PROTOCOL_VERSION,
        grant: GrantSigner::from_secret([9u8; 32]).issue(claims_for(
            std::process::id(),
            &lease,
            Duration::from_secs(60),
        )),
        pid: lease.worker_pid,
        start_time_ticks: lease.worker_start_time_ticks,
        uid: lease.owner_uid,
        euid: lease.owner_uid,
        gid: 1000,
        egid: 1000,
        supplementary_groups: Vec::new(),
        no_new_privs: true,
    };
    assert!(check_hello_with(&hello, &lease, true).is_ok());

    hello.no_new_privs = false;
    assert!(check_hello_with(&hello, &lease, true)
        .unwrap_err()
        .contains("NO_NEW_PRIVS"));
    hello.no_new_privs = true;

    hello.supplementary_groups = vec![27];
    assert!(check_hello_with(&hello, &lease, true)
        .unwrap_err()
        .contains("supplementary groups"));
    hello.supplementary_groups = Vec::new();

    hello.euid = 0;
    assert!(check_hello_with(&hello, &lease, true).is_err());
    hello.euid = lease.owner_uid;

    hello.protocol = protocol::PROTOCOL_VERSION + 1;
    let error = check_hello_with(&hello, &lease, true).unwrap_err();
    assert!(error.contains("protocol mismatch"), "{error}");
    assert!(error.contains("reinstall"), "{error}");
}

#[test]
fn a_worker_that_dies_without_a_result_returns_its_task_to_the_queue() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = Store::with_root(root.path().to_path_buf()).expect("store");
    let job = store
        .submit("hello".to_string(), None, None, Some(1000), None)
        .expect("submit");
    let claimed = store.claim_one().expect("claim").expect("a pending job");
    assert_eq!(claimed.id, job.id);

    let released = store
        .release_for_retry(&job.id, "agent worker exited early")
        .expect("release")
        .expect("job");
    assert_eq!(released.status, JobStatus::Pending);
    assert_eq!(released.recovery_count, 1);
    assert!(released.worker_pid.is_none());
    // Retryable, so the queue can hand it to a fresh worker.
    assert!(store.claim_one().expect("reclaim").is_some());
}

#[test]
fn a_task_that_keeps_killing_workers_is_failed_not_retried_forever() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = Store::with_root(root.path().to_path_buf()).expect("store");
    let job = store
        .submit("hello".to_string(), None, None, Some(1000), None)
        .expect("submit");
    let mut last = None;
    for _ in 0..6 {
        if store.claim_one().expect("claim").is_none() {
            break;
        }
        last = store
            .release_for_retry(&job.id, "worker crashed")
            .expect("release");
    }
    let last = last.expect("a terminal record");
    assert_eq!(last.status, JobStatus::Error);
    assert!(last.error.unwrap_or_default().contains("abandoned"));
    assert!(store.claim_one().expect("claim").is_none());
}

#[test]
fn the_queue_record_tracks_the_worker_process_not_the_broker() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = Store::with_root(root.path().to_path_buf()).expect("store");
    let job = store
        .submit("hello".to_string(), None, None, Some(1000), None)
        .expect("submit");
    let claimed = store.claim_one().expect("claim").expect("job");
    assert_eq!(claimed.worker_pid, Some(std::process::id()));

    let bound = store.bind_worker(&job.id, 424_242, Some(7)).expect("bind");
    assert_eq!(bound.worker_pid, Some(424_242));
    assert_eq!(bound.worker_start_time_ticks, Some(7));
}

#[test]
fn a_cancelled_task_stays_cancelled_when_its_worker_is_released() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = Store::with_root(root.path().to_path_buf()).expect("store");
    let job = store
        .submit("hello".to_string(), None, None, Some(1000), None)
        .expect("submit");
    store.claim_one().expect("claim").expect("job");
    store
        .request_cancel_for_owner(&job.id, Some(1000))
        .expect("cancel");
    let released = store
        .release_for_retry(&job.id, "worker exited")
        .expect("release")
        .expect("job");
    assert_eq!(released.status, JobStatus::Cancelled);
}

#[test]
fn supervision_can_be_disabled_without_disabling_the_broker() {
    let _lock = crate::test_env::lock_env();
    let previous = std::env::var_os("CLAWD_AGENTD");
    std::env::set_var("CLAWD_AGENTD", "off");
    assert!(!SupervisorConfig::from_env().enabled);
    std::env::set_var("CLAWD_AGENTD", "on");
    assert!(SupervisorConfig::from_env().enabled);
    match previous {
        Some(value) => std::env::set_var("CLAWD_AGENTD", value),
        None => std::env::remove_var("CLAWD_AGENTD"),
    }
}

#[test]
fn concurrent_workers_and_spawn_retries_are_bounded() {
    let _lock = crate::test_env::lock_env();
    let previous = std::env::var_os("CLAWD_AGENTD_MAX_WORKERS");
    std::env::set_var("CLAWD_AGENTD_MAX_WORKERS", "9999");
    assert_eq!(SupervisorConfig::from_env().max_workers, 64);
    match previous {
        Some(value) => std::env::set_var("CLAWD_AGENTD_MAX_WORKERS", value),
        None => std::env::remove_var("CLAWD_AGENTD_MAX_WORKERS"),
    }

    let mut throttle = SpawnThrottle::default();
    assert!(throttle.wait().is_none());
    for _ in 0..12 {
        throttle.record_failure();
    }
    let wait = throttle.wait().expect("a backoff after repeated failures");
    assert!(wait <= SPAWN_BACKOFF_MAX);
    throttle.record_success();
    assert!(throttle.wait().is_none());
}

// ---------------------------------------------------------------------------
// Permission mediation
// ---------------------------------------------------------------------------

struct ConsentStore {
    _dir: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl ConsentStore {
    fn new() -> Self {
        let lock = crate::test_env::lock_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let previous = std::env::var_os("COS_DATA_DIR");
        std::env::set_var("COS_DATA_DIR", dir.path());
        Self {
            _dir: dir,
            _lock: lock,
            previous,
        }
    }
}

impl Drop for ConsentStore {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

fn approve_once(lease: &Lease, verb: crate::caps::Verb, scope: &crate::caps::Scope) {
    let session = lease.session_id.as_deref().expect("session");
    let execution = crate::approvals::ApprovalExecutionBinding::for_worker(
        lease.task_id.clone(),
        lease.worker_pid,
        lease.worker_start_time_ticks,
        lease.approval_nonce.clone(),
        lease.approval_expires_at,
        Some(lease.owner_uid),
        session,
    )
    .expect("execution binding");
    let id = crate::approvals::submit_owned_with_execution(
        verb,
        scope.clone(),
        session,
        "test".to_string(),
        Some("test".to_string()),
        Some(lease.owner_uid),
        Some(crate::caps::ConsentContext::Attended),
        Some(execution),
    )
    .expect("submit");
    crate::approvals::approve_for_owner(
        &id,
        crate::approvals::GrantDuration::Once,
        Some("test".to_string()),
        None,
        Some(lease.owner_uid),
    )
    .expect("approve");
}

fn approve_once_for_operation(
    lease: &Lease,
    verb: crate::caps::Verb,
    scope: &crate::caps::Scope,
    operation_digest: &str,
) {
    let session = lease.session_id.as_deref().expect("session");
    let id = crate::approvals::submit_worker_request_for_operation(
        verb,
        scope.clone(),
        session,
        "test".to_string(),
        Some("test".to_string()),
        lease.owner_uid,
        lease.task_id.clone(),
        lease.worker_pid,
        lease.worker_start_time_ticks,
        lease.approval_nonce.clone(),
        lease.approval_expires_at,
        Some(operation_digest),
    )
    .expect("submit");
    crate::approvals::approve_for_owner(
        &id,
        crate::approvals::GrantDuration::Once,
        Some("test".to_string()),
        None,
        Some(lease.owner_uid),
    )
    .expect("approve");
}

fn consume_ask(scope: &crate::caps::Scope) -> ApprovalAsk {
    ApprovalAsk::Consume {
        verb: crate::caps::Verb::FS_READ.as_str().to_string(),
        scope: scope.clone(),
        operation_digest: None,
    }
}

#[test]
fn an_approved_grant_is_spent_once_for_the_leased_session_and_owner() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    approve_once(&lease, crate::caps::Verb::FS_READ, &scope);

    let mut used = 0;
    assert_eq!(
        mediate_approval(&mut used, &lease, &consume_ask(&scope)),
        ApprovalReply::Granted
    );
    // One-shot: a replay of the same ask finds nothing left to spend.
    assert_eq!(
        mediate_approval(&mut used, &lease, &consume_ask(&scope)),
        ApprovalReply::Pending { request_id: None }
    );
}

#[test]
fn worker_approval_cannot_substitute_a_different_operation_digest() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::self_ref("children");
    let lease = new_lease();
    let harmless = crate::crypto::sha256_hex(b"/usr/bin/printf\0hello");
    let substituted = crate::crypto::sha256_hex(b"/bin/sh\0-c\0id");
    approve_once_for_operation(&lease, crate::caps::Verb::PROC_SPAWN, &scope, &harmless);
    let mut used = 0;

    let wrong = ApprovalAsk::Consume {
        verb: crate::caps::Verb::PROC_SPAWN.as_str().to_string(),
        scope: scope.clone(),
        operation_digest: Some(substituted),
    };
    assert_eq!(
        mediate_approval(&mut used, &lease, &wrong),
        ApprovalReply::Pending { request_id: None }
    );

    let exact = ApprovalAsk::Consume {
        verb: crate::caps::Verb::PROC_SPAWN.as_str().to_string(),
        scope,
        operation_digest: Some(harmless),
    };
    assert_eq!(
        mediate_approval(&mut used, &lease, &exact),
        ApprovalReply::Granted
    );
}

#[test]
fn worker_authority_rechecks_operation_digest_after_durable_spend() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::self_ref("children");
    let lease = new_lease();
    let session = lease.session_id.clone().expect("session");
    let approved = crate::crypto::sha256_hex(b"/usr/bin/printf\0hello");
    let substituted = crate::crypto::sha256_hex(b"/bin/sh\0-c\0id");
    approve_once_for_operation(&lease, crate::caps::Verb::PROC_SPAWN, &scope, &approved);
    let consumed = crate::approvals::redeem_matching_worker_grant_for_owner_operation(
        &session,
        crate::caps::Verb::PROC_SPAWN,
        &scope,
        lease.owner_uid,
        &lease.approval_identity(),
        Some(&approved),
    )
    .unwrap()
    .expect("approved consent");

    let error = crate::clawd::authority::authorize_worker_approval(
        lease.owner_uid,
        &lease.task_id,
        &session,
        lease.worker_pid,
        lease.worker_start_time_ticks,
        &lease.approval_nonce,
        lease.deadline.saturating_duration_since(Instant::now()),
        Some(&substituted),
        &consumed,
    )
    .unwrap_err();
    assert!(error.contains("validated operation"), "{error}");
}

#[test]
fn approval_redemption_mints_an_exact_worker_bound_authority_grant() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    let session = lease.session_id.clone().expect("session");
    approve_once(&lease, crate::caps::Verb::FS_READ, &scope);
    let consumed = crate::approvals::redeem_matching_worker_grant_for_owner(
        &session,
        crate::caps::Verb::FS_READ,
        &scope,
        lease.owner_uid,
        &lease.approval_identity(),
    )
    .unwrap()
    .expect("approved consent");
    let view = crate::clawd::authority::authorize_worker_approval(
        lease.owner_uid,
        &lease.task_id,
        &session,
        lease.worker_pid,
        lease.worker_start_time_ticks,
        &lease.approval_nonce,
        lease.deadline.saturating_duration_since(Instant::now()),
        None,
        &consumed,
    )
    .expect("worker-bound authority");

    assert_eq!(view.issuer, crate::clawd::authority::Issuer::Approval);
    assert_eq!(view.owner_uid, lease.owner_uid);
    assert_eq!(view.bound_pid, lease.worker_pid);
    assert_eq!(view.subject.session_id.as_deref(), Some(session.as_str()));
    assert_eq!(
        view.subject.task_id.as_deref(),
        Some(lease.task_id.as_str())
    );
    assert_eq!(view.generation, u64::from(consumed.generation));
    assert_eq!(
        view.caps.iter().cloned().collect::<Vec<_>>(),
        vec![crate::caps::Cap::new(crate::caps::Verb::FS_READ, scope)]
    );
    assert!(view.expires_in <= Duration::from_secs(30));
    assert!(view.expires_in <= consumed.expires_in());
    assert_eq!(view.uses_remaining, Some(1));
}

#[test]
fn revocation_between_durable_spend_and_worker_grant_fails_closed() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    let session = lease.session_id.clone().expect("session");
    approve_once(&lease, crate::caps::Verb::FS_READ, &scope);
    let consumed = crate::approvals::redeem_matching_worker_grant_for_owner(
        &session,
        crate::caps::Verb::FS_READ,
        &scope,
        lease.owner_uid,
        &lease.approval_identity(),
    )
    .unwrap()
    .expect("approved consent");
    crate::approvals::generations::revoke(&crate::approvals::RevocationScope::Session {
        uid: Some(lease.owner_uid),
        session: session.clone(),
    })
    .unwrap();

    let error = crate::clawd::authority::authorize_worker_approval(
        lease.owner_uid,
        &lease.task_id,
        &session,
        lease.worker_pid,
        lease.worker_start_time_ticks,
        &lease.approval_nonce,
        lease.deadline.saturating_duration_since(Instant::now()),
        None,
        &consumed,
    )
    .unwrap_err();
    assert!(error.contains("revoked"), "{error}");
}

#[test]
fn a_worker_cannot_spend_another_sessions_or_owners_grant() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    approve_once(&lease, crate::caps::Verb::FS_READ, &scope);

    // Identity comes from the lease, so a worker whose lease names a
    // different session or owner simply finds no grant — there is no
    // field it could set to reach the real one.
    let mut other_session = new_lease();
    other_session.session_id = Some("session-b".to_string());
    let mut used = 0;
    assert_eq!(
        mediate_approval(&mut used, &other_session, &consume_ask(&scope)),
        ApprovalReply::Pending { request_id: None }
    );

    let mut other_owner = new_lease();
    other_owner.owner_uid = lease.owner_uid + 1;
    assert_eq!(
        mediate_approval(&mut used, &other_owner, &consume_ask(&scope)),
        ApprovalReply::Pending { request_id: None }
    );

    // The rightful lease can still spend it.
    assert_eq!(
        mediate_approval(&mut used, &lease, &consume_ask(&scope)),
        ApprovalReply::Granted
    );
}

#[test]
fn concurrent_same_session_workers_cannot_cross_spend() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    approve_once(&lease, crate::caps::Verb::FS_READ, &scope);

    let mut concurrent = new_lease();
    concurrent.session_id = lease.session_id.clone();
    concurrent.owner_uid = lease.owner_uid;
    concurrent.task_id = "task-b".to_string();
    concurrent.approval_nonce = "fedcba9876543210".to_string();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let approved_scope = scope.clone();
    let approved_barrier = std::sync::Arc::clone(&barrier);
    let approved = std::thread::spawn(move || {
        approved_barrier.wait();
        let mut used = 0;
        mediate_approval(&mut used, &lease, &consume_ask(&approved_scope))
    });
    let other_barrier = std::sync::Arc::clone(&barrier);
    let other = std::thread::spawn(move || {
        other_barrier.wait();
        let mut used = 0;
        mediate_approval(&mut used, &concurrent, &consume_ask(&scope))
    });
    barrier.wait();

    assert_eq!(approved.join().unwrap(), ApprovalReply::Granted);
    assert_eq!(
        other.join().unwrap(),
        ApprovalReply::Pending { request_id: None }
    );
}

#[test]
fn replacement_worker_with_same_task_cannot_reuse_old_lease_approval() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    approve_once(&lease, crate::caps::Verb::FS_READ, &scope);

    let mut replacement = new_lease();
    replacement.session_id = lease.session_id.clone();
    replacement.owner_uid = lease.owner_uid;
    replacement.task_id = lease.task_id.clone();
    replacement.approval_nonce = "replacement-lease".to_string();
    let mut used = 0;
    assert_eq!(
        mediate_approval(&mut used, &replacement, &consume_ask(&scope)),
        ApprovalReply::Pending { request_id: None }
    );
    assert_eq!(
        mediate_approval(&mut used, &lease, &consume_ask(&scope)),
        ApprovalReply::Granted
    );
}

#[test]
fn task_teardown_generation_invalidates_pending_worker_consent() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    approve_once(&lease, crate::caps::Verb::FS_READ, &scope);
    let session = lease.session_id.as_deref().unwrap();
    crate::clawd::authority::revoke_session_for_owner(session, lease.owner_uid);

    let mut used = 0;
    assert_eq!(
        mediate_approval(&mut used, &lease, &consume_ask(&scope)),
        ApprovalReply::Pending { request_id: None }
    );
}

#[test]
fn task_teardown_invalidates_an_undecided_request() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    let ask = ApprovalAsk::Request {
        verb: crate::caps::Verb::FS_READ.as_str().to_string(),
        scope,
        operation_digest: None,
    };
    let mut used = 0;
    let ApprovalReply::Pending {
        request_id: Some(id),
    } = mediate_approval(&mut used, &lease, &ask)
    else {
        panic!("expected pending approval");
    };
    let session = lease.session_id.as_deref().unwrap();
    assert_eq!(
        crate::approvals::invalidate_pending_for_execution(
            Some(lease.owner_uid),
            session,
            &lease.approval_identity(),
            "worker lease ended",
        )
        .unwrap(),
        1
    );
    crate::clawd::authority::revoke_session_for_owner(session, lease.owner_uid);

    assert_eq!(
        crate::approvals::status_for_owner(&id, Some(lease.owner_uid)),
        crate::approvals::RequestStatus::Denied
    );
}

#[test]
fn a_grant_for_a_different_verb_or_scope_is_not_spent() {
    let _store = ConsentStore::new();
    let approved = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    approve_once(&lease, crate::caps::Verb::FS_READ, &approved);

    let mut used = 0;
    let other_scope = consume_ask(&crate::caps::Scope::path("/etc/shadow"));
    assert_eq!(
        mediate_approval(&mut used, &lease, &other_scope),
        ApprovalReply::Pending { request_id: None }
    );
    let other_verb = ApprovalAsk::Consume {
        verb: crate::caps::Verb::FS_WRITE.as_str().to_string(),
        scope: approved.clone(),
        operation_digest: None,
    };
    assert_eq!(
        mediate_approval(&mut used, &lease, &other_verb),
        ApprovalReply::Pending { request_id: None }
    );
}

#[test]
fn an_unknown_verb_or_unusable_scope_is_refused() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let mut used = 0;
    let unknown = ApprovalAsk::Request {
        verb: "fs.read; rm -rf /".to_string(),
        scope: crate::caps::Scope::path("/tmp/x"),
        operation_digest: None,
    };
    assert!(matches!(
        mediate_approval(&mut used, &lease, &unknown),
        ApprovalReply::Refused { .. }
    ));

    let injected = ApprovalAsk::Request {
        verb: crate::caps::Verb::FS_READ.as_str().to_string(),
        scope: crate::caps::Scope::path("/tmp/x\nfs.write /etc"),
        operation_digest: None,
    };
    assert!(matches!(
        mediate_approval(&mut used, &lease, &injected),
        ApprovalReply::Refused { .. }
    ));

    let invalid_digest = ApprovalAsk::Request {
        verb: crate::caps::Verb::PROC_SPAWN.as_str().to_string(),
        scope: crate::caps::Scope::self_ref("children"),
        operation_digest: Some("not-a-sha256".to_string()),
    };
    assert!(matches!(
        mediate_approval(&mut used, &lease, &invalid_digest),
        ApprovalReply::Refused { .. }
    ));
    assert!(crate::approvals::list_pending().is_empty());
}

#[test]
fn a_task_without_a_session_cannot_reach_consent() {
    let _store = ConsentStore::new();
    let mut lease = new_lease();
    lease.session_id = None;
    let mut used = 0;
    assert!(matches!(
        mediate_approval(
            &mut used,
            &lease,
            &consume_ask(&crate::caps::Scope::path("/tmp/x"))
        ),
        ApprovalReply::Refused { .. }
    ));
}

#[test]
fn an_unattended_task_cannot_file_an_interactive_request() {
    let _store = ConsentStore::new();
    let mut lease = new_lease();
    lease.consent_context = crate::caps::ConsentContext::Unattended;
    let ask = ApprovalAsk::Request {
        verb: crate::caps::Verb::FS_DELETE.as_str().to_string(),
        scope: crate::caps::Scope::path("/home/user/notes.txt"),
        operation_digest: None,
    };
    let mut used = 0;
    let reply = mediate_approval(&mut used, &lease, &ask);
    match reply {
        ApprovalReply::Refused { message } => {
            assert!(message.contains("unattended"), "{message}");
            assert!(message.contains("scheduling"), "{message}");
        }
        other => panic!("unattended request must fail closed, got {other:?}"),
    }
    assert!(crate::approvals::list_pending().is_empty());
}

#[test]
fn filing_a_request_dedupes_and_records_only_broker_composed_text() {
    let _store = ConsentStore::new();
    let scope = crate::caps::Scope::path("/home/user/notes.txt");
    let lease = new_lease();
    let ask = ApprovalAsk::Request {
        verb: crate::caps::Verb::FS_READ.as_str().to_string(),
        scope: scope.clone(),
        operation_digest: None,
    };
    let mut used = 0;
    let ApprovalReply::Pending {
        request_id: Some(first),
    } = mediate_approval(&mut used, &lease, &ask)
    else {
        panic!("expected a filed request");
    };
    let ApprovalReply::Pending {
        request_id: Some(second),
    } = mediate_approval(&mut used, &lease, &ask)
    else {
        panic!("expected the same request to be reused");
    };
    assert_eq!(first, second);

    let pending = crate::approvals::list_pending();
    assert_eq!(pending.len(), 1);
    let request = &pending[0];
    // Session and owner come from the lease, never from the ask.
    assert_eq!(Some(request.session.as_str()), lease.session_id.as_deref());
    assert_eq!(request.owner_uid, Some(lease.owner_uid));
    assert_eq!(request.requester.as_deref(), Some("agentd-worker"));
    assert_eq!(request.risk, Some(crate::caps::Risk::Low));
    assert_eq!(request.context, Some(crate::caps::ConsentContext::Attended));
    let execution = request.execution.as_ref().expect("worker binding");
    assert_eq!(execution.identity.task_id, lease.task_id);
    assert_eq!(execution.identity.worker_pid, lease.worker_pid);
    assert_eq!(
        execution.identity.worker_start_time_ticks,
        lease.worker_start_time_ticks
    );
    assert_eq!(execution.identity.lease_nonce, lease.approval_nonce);
    assert_eq!(execution.expires_at, lease.approval_expires_at);
    assert_eq!(
        execution.generation,
        crate::approvals::generations::current(Some(lease.owner_uid), &request.session).unwrap()
    );
    assert!(request.reason.contains(&scope.to_string()));
}

#[test]
fn broker_side_mediation_is_bounded_per_task() {
    let _store = ConsentStore::new();
    let lease = new_lease();
    let mut used = protocol::MAX_APPROVAL_ASKS;
    assert!(matches!(
        mediate_approval(
            &mut used,
            &lease,
            &consume_ask(&crate::caps::Scope::path("/tmp/x"))
        ),
        ApprovalReply::Refused { .. }
    ));
}
