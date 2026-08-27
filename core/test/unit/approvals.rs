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
        Scope::path("/tmp/approved/**"),
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
        Scope::Wild,
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
        ("app-launch:uid=1000:pid=43:start=9", Verb::SYS_IDENTITY, Scope::name("accounts"), Some(1000)),
        (LAUNCHER, Verb::SYS_CONFIG, Scope::name("accounts"), Some(1000)),
        (LAUNCHER, Verb::SYS_IDENTITY, Scope::name("other"), Some(1000)),
        (LAUNCHER, Verb::SYS_IDENTITY, Scope::name("accounts"), Some(1001)),
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
    assert_eq!(status_for_owner(&pending, Some(1000)), RequestStatus::Pending);
    assert_eq!(status_for_owner(&pending, Some(1001)), RequestStatus::Unknown);
    assert_eq!(status_for_owner("ap-missing", Some(1000)), RequestStatus::Unknown);
    assert_eq!(status_for_owner("../escape", Some(1000)), RequestStatus::Unknown);

    approve_for_owner(&pending, GrantDuration::Once, Some("uid:0".into()), None, Some(1000))
        .unwrap();
    assert_eq!(status_for_owner(&pending, Some(1000)), RequestStatus::Approved);

    let required = vec![Cap::new(Verb::SYS_IDENTITY, Scope::name("accounts"))];
    assert!(consume_grant_set_once_for_owner(LAUNCHER, &required, Some(1000)).unwrap());
    assert_eq!(status_for_owner(&pending, Some(1000)), RequestStatus::Consumed);

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
