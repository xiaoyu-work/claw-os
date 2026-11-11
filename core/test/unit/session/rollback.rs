use super::*;
use crate::session::mutation::{Mutation, MutationRecord};
use crate::session::store;

/// Audit fix (session/rollback.rs HIGH): every replayed mutation
/// must re-check caps at replay time rather than trusting the
/// forward-action authorisation that was recorded weeks ago. If
/// caps now deny the verb, the inverse must be `Skipped` with a
/// `denied by caps:` detail — *not* silently replayed.
///
/// We force a denial by running in Strict mode without a session
/// in the process registry, which is the simplest way to make
/// `caps::require` return `Err` for every call.
#[test]
fn rollback_rechecks_caps() {
    // Serialize against other tests that mutate process-global env.
    let _lock = crate::test_env::lock_env();

    // Isolate the on-disk session store under a tempdir so the
    // test doesn't touch /var/lib/cos or another test's data.
    let dir = tempfile::tempdir().expect("tempdir");
    let prev_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", dir.path());

    // Capability decisions are always recorded, so redirect the
    // caps.jsonl sink into the same tempdir rather than letting the
    // replay checks below write to /var/log.
    let prev_log = std::env::var_os("COS_LOG_DIR");
    std::env::set_var("COS_LOG_DIR", dir.path());

    // Force caps::require to deny every call:
    //   - Strict mode: missing session ⇒ denied.
    //   - COS_SESSION unset ⇒ also denied.
    let prev_mode = std::env::var_os("COS_PERMS_MODE");
    let prev_session = std::env::var_os("COS_SESSION");
    std::env::set_var("COS_PERMS_MODE", "strict");
    std::env::remove_var("COS_SESSION");

    // Create a session and append one fs.write mutation. We do
    // this AFTER setting strict mode + clearing COS_SESSION so
    // the caps check at replay time hits the "no session" branch.
    let sid = store::create("test").expect("create session");
    let mutation = MutationRecord::new(Mutation::FsWrite {
        path: format!("{}/file.txt", dir.path().display()),
        prev_blob: None,
    });
    store::record_mutation(&sid, mutation).expect("record mutation");

    let outcomes = rollback(&sid).expect("rollback");
    assert_eq!(outcomes.len(), 1, "expected exactly one replay outcome");
    let o = &outcomes[0];
    assert_eq!(
        o.status,
        Status::Skipped,
        "denied entry must be Skipped, got {:?} (detail={:?})",
        o.status,
        o.detail,
    );
    assert!(
        o.detail.contains("denied by caps:"),
        "skip detail must mention caps denial, got {:?}",
        o.detail,
    );

    // Restore env so we don't poison neighbouring tests.
    match prev_data {
        Some(v) => std::env::set_var("COS_DATA_DIR", v),
        None => std::env::remove_var("COS_DATA_DIR"),
    }

    #[tokio::test]
    async fn accepted_package_inverse_stops_ordered_replay_until_root_confirms_completion() {
        let _lock = crate::test_env::lock_env();
        let directory = tempfile::tempdir().unwrap();
        let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", directory.path());
        let _log = crate::test_env::TestEnvVarGuard::set("COS_LOG_DIR", directory.path());
        let _mode = crate::test_env::TestEnvVarGuard::set("COS_PERMS_MODE", "strict");
        let session = store::create("isolated retained package inverse").unwrap();
        let mut sequences = Vec::new();
        for package in ["older-fixture", "newer-fixture"] {
            sequences.push(
                store::record_mutation(
                    &session,
                    MutationRecord::new(Mutation::SystemPackage {
                        package: package.into(),
                        previous_version: None,
                        was_held: false,
                    }),
                )
                .unwrap(),
            );
        }
        let row = crate::proc::SessionInfo {
            session_id: session.as_str().into(),
            pid: std::process::id(),
            command: vec!["isolated-rollback-fixture".into()],
            started_at: chrono::Utc::now().to_rfc3339(),
            stdout_path: String::new(),
            stderr_path: String::new(),
            group: None,
            parent: None,
            workdir: None,
            exit_code: None,
            ended_at: None,
            tier: None,
            scope: None,
            priority: None,
            caps: Some(crate::caps::CapSet::from_caps(vec![crate::caps::Cap::new(
                crate::caps::Verb::SYS_PACKAGE,
                crate::caps::Scope::Wild,
            )])),
            transient_caps: None,
            role: None,
            app_id: None,
            pending_bind: false,
            start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
            client: crate::session::SessionClient::default(),
        };
        crate::proc::with_trusted_session_override(row, async {
        let calls = std::cell::Cell::new(0);
        let confirmed = std::cell::Cell::new(false);
        let mut submit = |_: &SessionId, _, _: &str, _: Option<&str>, _| {
            calls.set(calls.get() + 1);
            Ok(serde_json::json!({"status":"accepted","accepted":true,"request_id":"a".repeat(32)}))
        };
        let mut completion = |_: &SessionId, _, _: &str, _: Option<&str>, _| {
            Ok(confirmed.get().then_some(true))
        };
        let first = rollback_with_package_restore(&session, &mut submit, &mut completion).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].seq, sequences[1]);
        assert_eq!(first[0].status, Status::Pending);
        assert!(load_rolled_back(&session).unwrap().is_empty());
        assert_eq!(load_uncertain(&session).unwrap(), Some(sequences[1]));
        assert!(rollback_with_package_restore(&session, &mut submit, &mut completion).is_err());
        assert_eq!(calls.get(), 1, "an unknown accepted inverse must not be submitted again");
        confirmed.set(true);
        let second = rollback_with_package_restore(&session, &mut submit, &mut completion).unwrap();
        assert_eq!(second[0].status, Status::AlreadyDone);
        assert_eq!(second[1].status, Status::Pending);
        assert_eq!(calls.get(), 2);
        assert!(load_rolled_back(&session).unwrap().contains(&sequences[1]));
        assert!(!load_rolled_back(&session).unwrap().contains(&sequences[0]));
        }).await;
    }
    match prev_log {
        Some(v) => std::env::set_var("COS_LOG_DIR", v),
        None => std::env::remove_var("COS_LOG_DIR"),
    }
    match prev_mode {
        Some(v) => std::env::set_var("COS_PERMS_MODE", v),
        None => std::env::remove_var("COS_PERMS_MODE"),
    }
    match prev_session {
        Some(v) => std::env::set_var("COS_SESSION", v),
        None => std::env::remove_var("COS_SESSION"),
    }
}
