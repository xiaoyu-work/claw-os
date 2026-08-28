use super::*;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct IsolatedEnv {
    _lock: MutexGuard<'static, ()>,
    prev_data_dir: Option<OsString>,
    _tmp: tempfile::TempDir,
}

impl Drop for IsolatedEnv {
    fn drop(&mut self) {
        match self.prev_data_dir.take() {
            Some(value) => std::env::set_var("COS_DATA_DIR", value),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

fn isolated_env() -> IsolatedEnv {
    let lock = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev_data_dir = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", tmp.path());
    IsolatedEnv {
        _lock: lock,
        prev_data_dir,
        _tmp: tmp,
    }
}

#[test]
fn submit_then_approve_writes_to_approved_dir() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::FS_WRITE,
        Scope::path("/tmp/foo"),
        "sess-a",
        "want to write hosts file",
        None,
    )
    .unwrap();
    assert!(pending_dir().join(format!("{id}.json")).exists());
    let resolved = approve(&id, GrantDuration::Once, None, None).unwrap();
    assert_eq!(resolved.decision.outcome, Outcome::Approved);
    assert!(!pending_dir().join(format!("{id}.json")).exists());
    assert!(approved_dir().join(format!("{id}.json")).exists());
}

#[test]
fn approved_once_grant_is_consumed() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::FS_WRITE,
        Scope::path("/tmp/approved/file.txt"),
        "sess-a",
        "write requested",
        None,
    )
    .unwrap();
    approve(&id, GrantDuration::Once, None, None).unwrap();

    let first = consume_matching_grant(
        "sess-a",
        Verb::FS_WRITE,
        &Scope::path("/tmp/approved/file.txt"),
    )
    .unwrap();
    assert_eq!(first, Some(GrantDuration::Once));
    assert!(!approved_dir().join(format!("{id}.json")).exists());
    assert!(consumed_dir().join(format!("{id}.json")).exists());

    let second = consume_matching_grant(
        "sess-a",
        Verb::FS_WRITE,
        &Scope::path("/tmp/approved/file.txt"),
    )
    .unwrap();
    assert_eq!(second, None);

    let recent = list_recent(10);
    assert!(recent.iter().any(|resolved| resolved.request.id == id));
}

#[test]
fn approved_scope_cannot_be_substituted_with_a_covered_child() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::FS_WRITE,
        Scope::path("/tmp/approved/**"),
        "sess-scope",
        "write requested",
        None,
    )
    .unwrap();
    approve(&id, GrantDuration::Once, None, None).unwrap();

    assert_eq!(
        consume_matching_grant(
            "sess-scope",
            Verb::FS_WRITE,
            &Scope::path("/tmp/approved/file.txt"),
        )
        .unwrap(),
        None,
        "approval matching is exact; capability containment is enforced only after grant minting"
    );
    assert_eq!(
        consume_matching_grant(
            "sess-scope",
            Verb::FS_WRITE,
            &Scope::path("/tmp/approved/**"),
        )
        .unwrap(),
        Some(GrantDuration::Once)
    );
}

#[test]
fn approval_persists_and_matches_the_canonical_scope() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::NET_DIAL,
        Scope::host("API.Example.COM:443"),
        "sess-canonical",
        "connect",
        None,
    )
    .unwrap();
    let pending = lookup_pending(&id).unwrap();
    assert_eq!(pending.scope, Scope::host("api.example.com:443"));
    approve(&id, GrantDuration::Once, None, None).unwrap();
    assert_eq!(
        consume_matching_grant(
            "sess-canonical",
            Verb::NET_DIAL,
            &Scope::host("api.example.com:443"),
        )
        .unwrap(),
        Some(GrantDuration::Once)
    );
}

#[test]
fn approved_session_grant_is_reusable() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-a",
        "install git",
        None,
    )
    .unwrap();
    approve(&id, GrantDuration::Session, None, None).unwrap();

    for _ in 0..2 {
        let grant =
            consume_matching_grant("sess-a", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap();
        assert_eq!(grant, Some(GrantDuration::Session));
    }
    assert!(approved_dir().join(format!("{id}.json")).exists());
}

#[test]
fn deny_moves_to_denied_dir() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::FS_DELETE,
        Scope::path("/tmp/delete-me"),
        "sess-b",
        "trying to wipe",
        None,
    )
    .unwrap();
    let resolved = deny(&id, Some("operator".into()), None).unwrap();
    assert_eq!(resolved.decision.outcome, Outcome::Denied);
    assert!(denied_dir().join(format!("{id}.json")).exists());
}

#[test]
fn list_pending_returns_submitted_requests() {
    let _tmp = isolated_env();
    let id1 = submit(Verb::FS_READ, Scope::path("/a"), "s", "r", None).unwrap();
    let id2 = submit(Verb::FS_WRITE, Scope::path("/b"), "s", "r", None).unwrap();
    let pending = list_pending();
    let ids: Vec<&str> = pending.iter().map(|r| r.id.as_str()).collect();
    assert!(ids.contains(&id1.as_str()));
    assert!(ids.contains(&id2.as_str()));
}

#[test]
fn grant_duration_parse() {
    assert_eq!(GrantDuration::parse("once"), Some(GrantDuration::Once));
    assert_eq!(
        GrantDuration::parse("Session"),
        Some(GrantDuration::Session)
    );
    assert_eq!(
        GrantDuration::parse("FOREVER"),
        Some(GrantDuration::Forever)
    );
    assert_eq!(GrantDuration::parse("nope"), None);
}

/// `submit` must never leave a partially-written file behind. We
/// can't easily simulate a process kill, but we can assert (a)
/// the temp file is gone after `submit` returns and (b) the
/// resulting pending/<id>.json parses cleanly.
#[test]
fn submit_writes_atomically_no_tmp_leftovers() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::FS_WRITE,
        Scope::path("/etc/hosts"),
        "sess",
        "want to edit hosts",
        None,
    )
    .unwrap();
    let path = pending_dir().join(format!("{id}.json"));
    let parsed: Request = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.id, id);

    // No hidden `.<id>.json.tmp.*` siblings should remain.
    for e in fs::read_dir(pending_dir()).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        assert!(
            !name.contains(".tmp."),
            "leftover tmp file in pending/: {name}"
        );
    }
}

/// Two concurrent resolvers on the same request id (e.g. CLI
/// approve racing the GUI applet's deny) must NOT both succeed.
/// Exactly one wins; the other gets "no pending request".
/// Before the rename-claim fix this race could leave the same id
/// in BOTH approved/ and denied/.
#[test]
fn concurrent_approve_and_deny_only_one_wins() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::FS_WRITE,
        Scope::path("/race"),
        "sess",
        "race target",
        None,
    )
    .unwrap();

    // Run approve and deny on background threads. Whichever
    // rename-claims first writes its outcome and the other has
    // to fail. We don't care which side wins; we care that
    // exactly one side is recorded.
    let id_a = id.clone();
    let id_b = id.clone();
    let h_a = std::thread::spawn(move || approve(&id_a, GrantDuration::Once, None, None));
    let h_b = std::thread::spawn(move || deny(&id_b, None, None));
    let r_a = h_a.join().unwrap();
    let r_b = h_b.join().unwrap();

    let approved_exists = approved_dir().join(format!("{id}.json")).exists();
    let denied_exists = denied_dir().join(format!("{id}.json")).exists();
    let pending_exists = pending_dir().join(format!("{id}.json")).exists();

    assert!(
        !pending_exists,
        "pending file must be gone after either resolver wins"
    );
    assert_ne!(
        approved_exists, denied_exists,
        "exactly one of approved/ or denied/ must exist (got approved={approved_exists}, denied={denied_exists})"
    );
    // Exactly one of the two calls succeeded.
    assert_eq!(
        r_a.is_ok() ^ r_b.is_ok(),
        true,
        "exactly one resolver should have succeeded; got approve={:?}, deny={:?}",
        r_a,
        r_b
    );
    let loser_err = if r_a.is_err() {
        r_a.unwrap_err()
    } else {
        r_b.unwrap_err()
    };
    assert!(
        loser_err.contains("no pending request"),
        "loser should see 'no pending request', got: {loser_err}"
    );
}

/// Resolving the same id twice in serial (legitimate retry, not a
/// race) must error the second time with a clear message — not
/// crash, not double-write.
#[test]
fn second_resolve_after_approve_errors_cleanly() {
    let _tmp = isolated_env();
    let id = submit(Verb::FS_READ, Scope::path("/x"), "s", "r", None).unwrap();
    approve(&id, GrantDuration::Once, None, None).unwrap();
    let err = deny(&id, None, None).unwrap_err();
    assert!(
        err.contains("no pending request"),
        "expected 'no pending request', got: {err}"
    );
    // Approve outcome is preserved; no denied/<id>.json appears.
    assert!(approved_dir().join(format!("{id}.json")).exists());
    assert!(!denied_dir().join(format!("{id}.json")).exists());
}

/// Approval queue should survive a power-loss simulation: if a
/// pending file's tmp sibling appears mid-write, the read side
/// must NOT mistake it for a real pending request.
#[test]
fn list_pending_ignores_tmp_files() {
    let _tmp = isolated_env();
    fs::create_dir_all(pending_dir()).unwrap();
    // Simulate an in-flight atomic write: hidden tmp file with
    // a `.tmp.` infix. list_dir already filters by `.json`
    // extension, but the leading `.` and the `.tmp.` infix
    // double-protect us.
    fs::write(
        pending_dir().join(".ap-xyz.json.tmp.abc"),
        r#"not real json"#,
    )
    .unwrap();
    let pending = list_pending();
    assert!(
        pending.is_empty(),
        "should ignore tmp file, got {pending:?}"
    );
}

const LAUNCHER: &str = "app-launch:uid=1000:pid=42:start=7";

fn approve_for(verb: Verb, scope: Scope, duration: GrantDuration) -> String {
    let id = submit_owned(verb, scope, LAUNCHER, "launch", None, Some(1000)).unwrap();
    approve_for_owner(&id, duration, Some("uid:0".into()), None, Some(1000)).unwrap();
    id
}

#[test]
fn grant_set_consumption_retires_every_duration_once() {
    for duration in [
        GrantDuration::Once,
        GrantDuration::Session,
        GrantDuration::Forever,
    ] {
        let _tmp = isolated_env();
        let id = approve_for(Verb::SYS_IDENTITY, Scope::name("accounts"), duration);
        let required = vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts"))];

        assert!(consume_grant_set_once_for_owner(LAUNCHER, &required, Some(1000)).unwrap());
        assert!(
            !approved_dir().join(format!("{id}.json")).exists(),
            "{duration:?} must not stay reusable after an App launch"
        );
        assert!(consumed_dir().join(format!("{id}.json")).exists());
        assert!(!consume_grant_set_once_for_owner(LAUNCHER, &required, Some(1000)).unwrap());
    }
}

#[test]
fn grant_set_consumption_requires_an_exact_session_owner_verb_and_scope() {
    let _tmp = isolated_env();
    let id = approve_for(
        Verb::SYS_IDENTITY,
        Scope::name("accounts"),
        GrantDuration::Once,
    );

    let cases = [
        (
            "app-launch:uid=1000:pid=43:start=9",
            Verb::SYS_IDENTITY,
            Scope::name("accounts"),
            Some(1000),
        ),
        (
            LAUNCHER,
            Verb::SYS_CONFIG,
            Scope::path("/accounts"),
            Some(1000),
        ),
        (
            LAUNCHER,
            Verb::SYS_IDENTITY,
            Scope::name("other"),
            Some(1000),
        ),
        (
            LAUNCHER,
            Verb::SYS_IDENTITY,
            Scope::name("accounts"),
            Some(1001),
        ),
    ];
    for (session, verb, scope, owner) in cases {
        let required = vec![Cap::new(verb, scope.clone())];
        assert!(
            !consume_grant_set_once_for_owner(session, &required, owner).unwrap(),
            "grant matching must stay exact for {session} {}",
            verb.as_str()
        );
    }
    assert!(approved_dir().join(format!("{id}.json")).exists());
}

#[test]
fn grant_set_consumption_is_all_or_none() {
    let _tmp = isolated_env();
    let approved = approve_for(
        Verb::SYS_IDENTITY,
        Scope::name("accounts"),
        GrantDuration::Once,
    );
    let required = vec![
        Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts")),
        Cap::new(Verb::SYS_CONFIG, Scope::path("/etc/cos/agent.toml")),
    ];

    assert!(
        !consume_grant_set_once_for_owner(LAUNCHER, &required, Some(1000)).unwrap(),
        "a partly approved set must not be settled"
    );
    assert!(
        approved_dir().join(format!("{approved}.json")).exists(),
        "the approved half must not be burned while the other half is pending"
    );

    let second = approve_for(
        Verb::SYS_CONFIG,
        Scope::path("/etc/cos/agent.toml"),
        GrantDuration::Once,
    );
    assert!(consume_grant_set_once_for_owner(LAUNCHER, &required, Some(1000)).unwrap());
    for id in [approved, second] {
        assert!(!approved_dir().join(format!("{id}.json")).exists());
        assert!(consumed_dir().join(format!("{id}.json")).exists());
    }
}

#[test]
fn grant_set_consumption_needs_one_grant_per_capability() {
    let _tmp = isolated_env();
    approve_for(Verb::SYS_OBSERVE, Scope::name("**"), GrantDuration::Once);
    let required = vec![
        Cap::new(Verb::SYS_OBSERVE, Scope::name("packages")),
        Cap::new(Verb::SYS_OBSERVE, Scope::name("services")),
    ];
    assert!(
        !consume_grant_set_once_for_owner(LAUNCHER, &required, Some(1000)).unwrap(),
        "one approval must not satisfy two required capabilities"
    );
}

#[test]
fn status_reports_the_decision_state_for_the_owner_only() {
    let _tmp = isolated_env();
    let pending = submit_owned(
        Verb::SYS_IDENTITY,
        Scope::name("accounts"),
        LAUNCHER,
        "launch",
        None,
        Some(1000),
    )
    .unwrap();
    assert_eq!(
        status_for_owner(&pending, Some(1000)),
        RequestStatus::Pending
    );
    assert_eq!(
        status_for_owner(&pending, Some(1001)),
        RequestStatus::Unknown
    );
    assert_eq!(
        status_for_owner("ap-missing", Some(1000)),
        RequestStatus::Unknown
    );
    assert_eq!(
        status_for_owner("../escape", Some(1000)),
        RequestStatus::Unknown
    );

    approve_for_owner(
        &pending,
        GrantDuration::Once,
        Some("uid:0".into()),
        None,
        Some(1000),
    )
    .unwrap();
    assert_eq!(
        status_for_owner(&pending, Some(1000)),
        RequestStatus::Approved
    );

    let required = vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts"))];
    assert!(consume_grant_set_once_for_owner(LAUNCHER, &required, Some(1000)).unwrap());
    assert_eq!(
        status_for_owner(&pending, Some(1000)),
        RequestStatus::Consumed
    );

    let denied = submit_owned(
        Verb::SYS_CONFIG,
        Scope::path("/etc/cos/agent.toml"),
        LAUNCHER,
        "launch",
        None,
        Some(1000),
    )
    .unwrap();
    deny_for_owner(&denied, Some("uid:0".into()), None, Some(1000)).unwrap();
    assert_eq!(status_for_owner(&denied, Some(1000)), RequestStatus::Denied);
}

// ---------------------------------------------------------------------------
// What an approval actually authorises
// ---------------------------------------------------------------------------

#[test]
fn an_approval_carries_a_bounded_grant() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-bound",
        "install git",
        None,
    )
    .unwrap();
    let resolved = approve(&id, GrantDuration::Forever, None, None).unwrap();
    let grant = resolved
        .decision
        .grant
        .expect("an approval mints a bounded grant");

    assert!(grant.expires_at > resolved.decision.decided_at);
    assert!(
        grant.expires_at <= resolved.decision.decided_at + FOREVER_GRANT_SECS,
        "`forever` still has a deadline"
    );
    assert_eq!(grant.uses_remaining, REPEATABLE_GRANT_USES);
    assert!(!grant.reference.is_empty());
    assert!(
        !grant.reference.contains(&id),
        "the audit reference is keyed, not the request id"
    );
    let authorization = grant
        .authorization
        .expect("new approvals bind exact authority");
    assert_eq!(authorization.owner_uid, None);
    assert_eq!(authorization.session, "sess-bound");
    assert_eq!(
        authorization.capability,
        Cap::new(Verb::SYS_PACKAGE, Scope::name("git"))
    );
    assert_eq!(authorization.risk, crate::caps::Risk::Critical);
    assert_eq!(authorization.context, None);
    assert_eq!(authorization.execution, None);
}

#[test]
fn attended_high_and_critical_defaults_refuse_overbroad_durations() {
    let _tmp = isolated_env();
    LocalApprovalInvocation::new("test:duration-policy")
        .unwrap()
        .sync_scope(|| {
            let critical = submit_owned_with_context(
                Verb::SYS_PACKAGE,
                Scope::name("git"),
                "agent-critical",
                "install git",
                None,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            let error =
                approve_for_owner(&critical, GrantDuration::Session, None, None, Some(1000))
                    .unwrap_err();
            assert!(error.contains("only be approved once"), "{error}");
            assert_eq!(
                status_for_owner(&critical, Some(1000)),
                RequestStatus::Pending
            );
            approve_for_owner(&critical, GrantDuration::Once, None, None, Some(1000)).unwrap();

            let high = submit_owned_with_context(
                Verb::SECRET_READ,
                Scope::name("default/API_KEY"),
                "agent-high",
                "read key",
                None,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            let error = approve_for_owner(&high, GrantDuration::Forever, None, None, Some(1000))
                .unwrap_err();
            assert!(error.contains("may not be approved forever"), "{error}");
        });
}

#[test]
fn unattended_context_cannot_create_an_interactive_request() {
    let _tmp = isolated_env();
    let error = submit_owned_with_context(
        Verb::FS_DELETE,
        Scope::path("/tmp/unattended"),
        "agent-unattended",
        "delete",
        None,
        Some(1000),
        Some(crate::caps::ConsentContext::Unattended),
    )
    .unwrap_err();
    assert!(error.contains("unattended"), "{error}");
    assert!(list_pending().is_empty());
}

#[test]
fn attended_agent_request_without_invocation_identity_fails_closed() {
    let _tmp = isolated_env();
    let error = submit_owned_with_context(
        Verb::FS_DELETE,
        Scope::path("/tmp/no-invocation"),
        "agent-no-invocation",
        "delete",
        None,
        Some(1000),
        Some(crate::caps::ConsentContext::Attended),
    )
    .unwrap_err();
    assert!(error.contains("per-invocation"), "{error}");
    assert!(list_pending().is_empty());
}

#[tokio::test]
async fn concurrent_web_conversations_cannot_redeem_each_others_approval() {
    let _tmp = isolated_env();
    let session = "shared-capability-session";
    let scope = Scope::path("/tmp/concurrent-web");
    let first = LocalApprovalInvocation::new("web:conversation-a:turn:1").unwrap();
    let second = LocalApprovalInvocation::new("web:conversation-b:turn:1").unwrap();
    let first_identity = first.identity().clone();
    let second_identity = second.identity().clone();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
    let (approved_tx, approved_rx) = tokio::sync::oneshot::channel();

    let first_run = {
        let barrier = barrier.clone();
        let scope = scope.clone();
        first.scope(async move {
            let id = submit_owned_with_context(
                Verb::FS_DELETE,
                scope.clone(),
                session,
                "delete",
                None,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            approve_for_owner(&id, GrantDuration::Once, None, None, Some(1000)).unwrap();
            approved_tx.send(()).unwrap();
            barrier.wait().await;
            redeem_matching_grant_for_owner(
                session,
                Verb::FS_DELETE,
                &scope,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap()
        })
    };
    let second_run = {
        let barrier = barrier.clone();
        let scope = scope.clone();
        second.scope(async move {
            approved_rx.await.unwrap();
            let substituted = redeem_matching_grant_for_owner(
                session,
                Verb::FS_DELETE,
                &scope,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            barrier.wait().await;
            substituted
        })
    };

    let (owner_result, substituted_result) = tokio::join!(first_run, second_run);
    assert_ne!(first_identity.task_id, second_identity.task_id);
    assert_ne!(first_identity.lease_nonce, second_identity.lease_nonce);
    assert!(substituted_result.is_none());
    assert!(owner_result.is_some());
}

#[test]
fn later_web_turn_cannot_reuse_or_restore_prior_turn_grant() {
    let _tmp = isolated_env();
    let session = "shared-web-session";
    let scope = Scope::path("/tmp/later-turn");
    let first = LocalApprovalInvocation::new("web:conversation-a:turn:1").unwrap();
    let first_identity = first.identity().clone();
    let (id, approved_record) = first.sync_scope(|| {
        let id = submit_owned_with_context(
            Verb::FS_DELETE,
            scope.clone(),
            session,
            "delete",
            None,
            Some(1000),
            Some(crate::caps::ConsentContext::Attended),
        )
        .unwrap();
        approve_for_owner(&id, GrantDuration::Session, None, None, Some(1000)).unwrap();
        let approved_record =
            std::fs::read_to_string(approved_dir().join(format!("{id}.json"))).unwrap();
        (id, approved_record)
    });

    assert_eq!(status_for_owner(&id, Some(1000)), RequestStatus::Consumed);
    std::fs::write(approved_dir().join(format!("{id}.json")), approved_record).unwrap();

    let second = LocalApprovalInvocation::new("web:conversation-a:turn:2").unwrap();
    let second_identity = second.identity().clone();
    let result = second.sync_scope(|| {
        redeem_matching_grant_for_owner(
            session,
            Verb::FS_DELETE,
            &scope,
            Some(1000),
            Some(crate::caps::ConsentContext::Attended),
        )
        .unwrap()
    });
    assert_ne!(first_identity.task_id, second_identity.task_id);
    assert_ne!(first_identity.lease_nonce, second_identity.lease_nonce);
    assert!(
        result.is_none(),
        "an ended turn stays revoked even when its approved file is restored"
    );
}

#[tokio::test]
async fn web_disconnect_invalidates_pending_and_approved_invocation_state() {
    let _tmp = isolated_env();
    let ids = std::sync::Arc::new(std::sync::Mutex::new(None));
    let ids_for_run = ids.clone();
    let invocation = LocalApprovalInvocation::new("web:conversation-disconnect:turn:1").unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(20),
        invocation.scope(async move {
            let pending = submit_owned_with_context(
                Verb::FS_DELETE,
                Scope::path("/tmp/disconnected-pending"),
                "disconnect-session",
                "delete",
                None,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            let approved = submit_owned_with_context(
                Verb::SECRET_READ,
                Scope::name("default/disconnected"),
                "disconnect-session",
                "read",
                None,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            approve_for_owner(&approved, GrantDuration::Session, None, None, Some(1000)).unwrap();
            *ids_for_run.lock().unwrap() = Some((pending, approved));
            std::future::pending::<()>().await;
        }),
    )
    .await;
    assert!(
        result.is_err(),
        "the simulated disconnect must cancel the turn"
    );

    let (pending, approved) = ids.lock().unwrap().clone().unwrap();
    assert_eq!(
        status_for_owner(&pending, Some(1000)),
        RequestStatus::Denied
    );
    assert_eq!(
        status_for_owner(&approved, Some(1000)),
        RequestStatus::Consumed
    );
}

#[test]
fn expired_execution_request_cannot_be_approved() {
    let _tmp = isolated_env();
    LocalApprovalInvocation::new("test:expired-request")
        .unwrap()
        .sync_scope(|| {
            let id = submit_owned_with_context(
                Verb::FS_DELETE,
                Scope::path("/tmp/expired-request"),
                "agent-expired",
                "delete",
                None,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            let path = pending_dir().join(format!("{id}.json"));
            let mut request: Request =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            request.execution.as_mut().unwrap().expires_at = 1;
            std::fs::write(&path, serde_json::to_string_pretty(&request).unwrap()).unwrap();

            let error =
                approve_for_owner(&id, GrantDuration::Once, None, None, Some(1000)).unwrap_err();
            assert!(
                error.contains("no longer matches a live execution"),
                "{error}"
            );
            assert_eq!(status_for_owner(&id, Some(1000)), RequestStatus::Denied);
        });
}

#[test]
fn generation_change_after_request_invalidates_the_decision() {
    let _tmp = isolated_env();
    LocalApprovalInvocation::new("test:generation-change")
        .unwrap()
        .sync_scope(|| {
            let id = submit_owned_with_context(
                Verb::FS_DELETE,
                Scope::path("/tmp/stale-request"),
                "agent-stale",
                "delete",
                None,
                Some(1000),
                Some(crate::caps::ConsentContext::Attended),
            )
            .unwrap();
            generations::revoke(&RevocationScope::Session {
                uid: Some(1000),
                session: "agent-stale".to_string(),
            })
            .unwrap();

            let error =
                approve_for_owner(&id, GrantDuration::Once, None, None, Some(1000)).unwrap_err();
            assert!(
                error.contains("no longer matches a live execution"),
                "{error}"
            );
            assert_eq!(status_for_owner(&id, Some(1000)), RequestStatus::Denied);
        });
}

#[test]
fn a_denial_mints_nothing() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-denied",
        "install git",
        None,
    )
    .unwrap();
    let resolved = deny(&id, None, None).unwrap();
    assert!(resolved.decision.grant.is_none());
}

#[test]
fn a_record_with_no_grant_provenance_authorises_nothing() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-legacy",
        "install git",
        None,
    )
    .unwrap();
    let mut resolved = approve(&id, GrantDuration::Forever, None, None).unwrap();

    // Exactly what a record written before the authority existed looks
    // like: a real decision, no expiry, no budget, no provenance.
    resolved.decision.grant = None;
    let path = approved_dir().join(format!("{id}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&resolved).unwrap()).unwrap();

    assert_eq!(
        consume_matching_grant("sess-legacy", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        None,
        "a historical yes is evidence, not standing authority"
    );
    assert!(!has_approved_grant_for_owner(
        "sess-legacy",
        &Cap::new(Verb::SYS_PACKAGE, Scope::name("git")),
        None,
    )
    .unwrap());
}

#[test]
fn a_legacy_bounded_grant_without_exact_authorization_fails_closed() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::FS_WRITE,
        Scope::path("/tmp/legacy"),
        "sess-legacy-bound",
        "write",
        None,
    )
    .unwrap();
    let mut resolved = approve(&id, GrantDuration::Once, None, None).unwrap();
    resolved.decision.grant.as_mut().unwrap().authorization = None;
    let path = approved_dir().join(format!("{id}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&resolved).unwrap()).unwrap();

    assert_eq!(
        consume_matching_grant(
            "sess-legacy-bound",
            Verb::FS_WRITE,
            &Scope::path("/tmp/legacy"),
        )
        .unwrap(),
        None,
        "legacy records are history, not exact capability authority"
    );
}

#[test]
fn a_forged_record_with_an_expired_binding_authorises_nothing() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-expired",
        "install git",
        None,
    )
    .unwrap();
    let mut resolved = approve(&id, GrantDuration::Forever, None, None).unwrap();
    resolved.decision.grant.as_mut().unwrap().expires_at = 1;
    let path = approved_dir().join(format!("{id}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&resolved).unwrap()).unwrap();

    assert_eq!(
        consume_matching_grant("sess-expired", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        None
    );
}

#[test]
fn a_repeatable_grant_still_spends_its_budget() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-budget",
        "install git",
        None,
    )
    .unwrap();
    let mut resolved = approve(&id, GrantDuration::Session, None, None).unwrap();
    resolved.decision.grant.as_mut().unwrap().uses_remaining = 2;
    let path = approved_dir().join(format!("{id}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&resolved).unwrap()).unwrap();

    for expected in [Some(GrantDuration::Session), Some(GrantDuration::Session)] {
        assert_eq!(
            consume_matching_grant("sess-budget", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
            expected
        );
    }
    assert_eq!(
        consume_matching_grant("sess-budget", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        None,
        "an exhausted approval stops authorising"
    );
    assert!(consumed_dir().join(format!("{id}.json")).exists());
}

#[test]
fn concurrent_consumers_of_a_one_shot_approval_produce_one_winner() {
    let _tmp = isolated_env();
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-race",
        "install git",
        None,
    )
    .unwrap();
    approve(&id, GrantDuration::Once, None, None).unwrap();

    let winners = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut threads = Vec::new();
    for _ in 0..6 {
        let winners = std::sync::Arc::clone(&winners);
        threads.push(std::thread::spawn(move || {
            if matches!(
                consume_matching_grant("sess-race", Verb::SYS_PACKAGE, &Scope::name("git")),
                Ok(Some(_))
            ) {
                winners.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(winners.load(std::sync::atomic::Ordering::SeqCst), 1);
}

// ---------------------------------------------------------------------------
// Revocation
// ---------------------------------------------------------------------------

fn approve_reusable(session: &str, duration: GrantDuration) -> String {
    let id = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        session,
        "install git",
        None,
    )
    .unwrap();
    approve(&id, duration, None, None).unwrap();
    id
}

#[test]
fn an_approval_captures_the_current_revocation_generation() {
    let _tmp = isolated_env();
    generations::revoke(&RevocationScope::Session {
        uid: None,
        session: "sess-gen".to_string(),
    })
    .unwrap();
    let id = approve_reusable("sess-gen", GrantDuration::Forever);
    let resolved: Resolved = serde_json::from_str(
        &std::fs::read_to_string(approved_dir().join(format!("{id}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resolved.decision.grant.unwrap().generation,
        Some(1),
        "the binding records the generation in force when it was approved"
    );
}

#[test]
fn owner_revoke_advances_past_a_session_generation_and_kills_newer_grants() {
    let _tmp = isolated_env();
    let session_generation = generations::revoke(&RevocationScope::Session {
        uid: Some(1000),
        session: "sess-owner-floor".to_string(),
    })
    .unwrap();
    let id = submit_owned(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-owner-floor",
        "install git",
        None,
        Some(1000),
    )
    .unwrap();
    approve_for_owner(&id, GrantDuration::Session, None, None, Some(1000)).unwrap();

    let owner_generation =
        generations::revoke(&RevocationScope::Owner { uid: Some(1000) }).unwrap();
    assert!(owner_generation > session_generation);
    assert_eq!(
        consume_matching_grant_for_owner(
            "sess-owner-floor",
            Verb::SYS_PACKAGE,
            &Scope::name("git"),
            Some(1000),
        )
        .unwrap(),
        None
    );
}

#[test]
fn revoking_a_session_kills_its_reusable_approvals_immediately() {
    let _tmp = isolated_env();
    approve_reusable("sess-revoke", GrantDuration::Forever);
    assert_eq!(
        consume_matching_grant("sess-revoke", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        Some(GrantDuration::Forever)
    );

    generations::revoke(&RevocationScope::Session {
        uid: None,
        session: "sess-revoke".to_string(),
    })
    .unwrap();

    assert_eq!(
        consume_matching_grant("sess-revoke", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        None,
        "a 30-day approval stops being authority the moment it is revoked"
    );
}

#[test]
fn revoking_an_owner_kills_every_session_it_approved() {
    let _tmp = isolated_env();
    for session in ["sess-one", "sess-two"] {
        let id = submit(
            Verb::SYS_PACKAGE,
            Scope::name("git"),
            session,
            "install git",
            Some("uid:1000".to_string()),
        )
        .unwrap();
        approve_for_owner(&id, GrantDuration::Session, None, None, None).unwrap();
    }
    // The requests were filed without an owner, so they live under the
    // system scope; revoke that and both die.
    generations::revoke(&RevocationScope::Owner { uid: None }).unwrap();
    for session in ["sess-one", "sess-two"] {
        assert_eq!(
            consume_matching_grant(session, Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
            None
        );
    }
}

#[test]
fn a_restored_backup_cannot_revive_a_revoked_approval() {
    let _tmp = isolated_env();
    let id = approve_reusable("sess-backup", GrantDuration::Forever);
    let path = approved_dir().join(format!("{id}.json"));
    // Exactly what a backup taken before the revocation contains.
    let backup = std::fs::read_to_string(&path).unwrap();

    generations::revoke(&RevocationScope::Session {
        uid: None,
        session: "sess-backup".to_string(),
    })
    .unwrap();
    std::fs::write(&path, &backup).unwrap();

    assert_eq!(
        consume_matching_grant("sess-backup", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        None,
        "the generation lives outside the record, so restoring the record changes nothing"
    );
}

#[test]
fn a_binding_without_a_generation_fails_closed_on_every_path() {
    let _tmp = isolated_env();
    let id = approve_reusable("sess-nogen", GrantDuration::Forever);
    let path = approved_dir().join(format!("{id}.json"));
    let mut resolved: Resolved =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Exactly the shape of a record written before revocation
    // generations existed: a real approval, a live expiry, a use budget
    // left — and nothing to compare against.
    resolved.decision.grant.as_mut().unwrap().generation = None;
    assert!(resolved.decision.grant.as_ref().unwrap().uses_remaining > 0);
    assert!(resolved.decision.grant.as_ref().unwrap().expires_at > now_secs());
    std::fs::write(&path, serde_json::to_string_pretty(&resolved).unwrap()).unwrap();

    let cap = Cap::new(Verb::SYS_PACKAGE, Scope::name("git"));

    // 1. The gate's own spend.
    assert_eq!(
        consume_matching_grant("sess-nogen", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        None,
        "a binding with nothing to compare against is not authority"
    );
    // 2. The owner-scoped spend the agentd gateway uses.
    assert_eq!(
        consume_matching_grant_for_owner(
            "sess-nogen",
            Verb::SYS_PACKAGE,
            &Scope::name("git"),
            None,
        )
        .unwrap(),
        None
    );
    // 3. The non-consuming probe that decides whether to re-prompt.
    assert!(!has_approved_grant_for_owner("sess-nogen", &cap, None).unwrap());
    // 4. The all-or-none set consumption an App launch settles with.
    assert!(!consume_grant_set_once_for_owner("sess-nogen", &[cap], None).unwrap());

    // The record is still on disk and still readable as history: it is
    // evidence a decision happened, not authority.
    assert!(path.exists());
    assert!(list_recent(10).iter().any(|entry| entry.request.id == id));
}

#[test]
fn a_revocation_that_races_a_spend_leaves_no_use_behind() {
    let _tmp = isolated_env();
    approve_reusable("sess-race-revoke", GrantDuration::Session);

    let spends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut threads = Vec::new();
    for index in 0..6 {
        let spends = std::sync::Arc::clone(&spends);
        threads.push(std::thread::spawn(move || {
            if index == 3 {
                let _ = generations::revoke(&RevocationScope::Session {
                    uid: None,
                    session: "sess-race-revoke".to_string(),
                });
            } else if matches!(
                consume_matching_grant("sess-race-revoke", Verb::SYS_PACKAGE, &Scope::name("git")),
                Ok(Some(_))
            ) {
                spends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    // Whatever the interleaving, nothing may be spent afterwards.
    assert_eq!(
        consume_matching_grant("sess-race-revoke", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        None
    );
}

#[test]
fn one_owners_revocation_does_not_touch_another() {
    let _tmp = isolated_env();
    let mine = submit(
        Verb::SYS_PACKAGE,
        Scope::name("git"),
        "sess-mine",
        "install git",
        None,
    )
    .unwrap();
    approve_for_owner(&mine, GrantDuration::Session, None, None, None).unwrap();

    generations::revoke(&RevocationScope::Owner { uid: Some(1000) }).unwrap();

    assert_eq!(
        consume_matching_grant("sess-mine", Verb::SYS_PACKAGE, &Scope::name("git")).unwrap(),
        Some(GrantDuration::Session),
        "revoking uid 1000 must not retire an unattributed approval"
    );
}
