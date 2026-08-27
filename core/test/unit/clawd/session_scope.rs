use super::*;
use crate::caps::{Cap, Scope, Verb};
use crate::session::SessionId;
use crate::test_env::{lock_env, TestEnvVarGuard};

/// Owner context for the execution-time clamp: a canonical home the
/// test controls plus a second account's home alongside it.
struct Owner {
    _temp: tempfile::TempDir,
    _data: TestEnvVarGuard,
    uid: u32,
    home: std::path::PathBuf,
    other_home: std::path::PathBuf,
}

fn owner() -> Owner {
    let temp = tempfile::tempdir().expect("tempdir");
    let data = TestEnvVarGuard::set("COS_DATA_DIR", temp.path().join("var"));
    let home = temp.path().join("home").join("owner");
    let other_home = temp.path().join("home").join("neighbour");
    std::fs::create_dir_all(&home).expect("owner home");
    std::fs::create_dir_all(&other_home).expect("neighbour home");
    Owner {
        _temp: temp,
        _data: data,
        uid: 1001,
        home,
        other_home,
    }
}

fn meta_with_origin(owner_uid: u32, origin: Option<SessionOrigin>) -> SessionMeta {
    let mut meta = SessionMeta::fresh(SessionId::generate(), "test");
    meta.owner_uid = Some(owner_uid);
    meta.origin = origin;
    meta
}

/// The snapshot `clawd::scheduler` persists for an approved trigger:
/// the executor verb it proved, one exactly named credential, and the
/// rest of whatever the owner's session happened to hold at creation.
fn approved_trigger_snapshot(owner: &Owner) -> CapSet {
    let mut stored = CapSet::new();
    // Proven or approved at creation.
    stored.insert(Cap::new(Verb::AGENT_SPAWN, Scope::Wild));
    stored.insert(Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SLACK_TOKEN"),
    ));
    // Carried along by the creating session's role, never reviewed for
    // unattended execution.
    stored.insert(Cap::new(Verb::PROC_SPAWN, Scope::Wild));
    stored.insert(Cap::new(Verb::SECRET_READ, Scope::name("**")));
    stored.insert(Cap::new(Verb::NET_DIAL, Scope::host("**")));
    stored.insert(Cap::new(Verb::SYS_PACKAGE, Scope::name("**")));
    stored.insert(Cap::new(Verb::FS_DELETE, Scope::path("/**")));
    stored.insert(Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/**", owner.home.display())),
    ));
    stored
}

// -----------------------------------------------------------------
// Provenance
// -----------------------------------------------------------------

/// A delegation marker is authority, so it counts only on a record the
/// delegated account could not have written.
#[test]
fn delegation_provenance_is_believed_only_on_a_root_owned_record() {
    for origin in [
        SessionOrigin::CronDelegation,
        SessionOrigin::TriggerDelegation,
    ] {
        let meta = meta_with_origin(1001, Some(origin));
        assert_eq!(trusted_origin(&meta, true), origin);
        assert_eq!(
            trusted_origin(&meta, false),
            SessionOrigin::SystemAgentTask,
            "a forged {origin:?} marker must fall back to the baseline"
        );
    }

    // An ambient task, and a record with no marker at all, are never
    // promoted — not even when the record is root-owned.
    let ambient = meta_with_origin(1001, Some(SessionOrigin::SystemAgentTask));
    assert_eq!(
        trusted_origin(&ambient, true),
        SessionOrigin::SystemAgentTask
    );
    let unmarked = meta_with_origin(1001, None);
    assert_eq!(
        trusted_origin(&unmarked, true),
        SessionOrigin::SystemAgentTask
    );
}

// -----------------------------------------------------------------
// Execution-time clamp
// -----------------------------------------------------------------

/// End to end for an unattended trigger: the snapshot keeps exactly the
/// executor verb and credential the owner approved, and loses
/// everything the creating session merely happened to hold.
#[test]
fn approved_trigger_snapshot_keeps_only_its_delegated_authority() {
    let _lock = lock_env();
    let owner = owner();
    let stored = approved_trigger_snapshot(&owner);
    let meta = meta_with_origin(owner.uid, Some(SessionOrigin::TriggerDelegation));

    let caps = scoped_caps(&stored, &meta, true, owner.uid, &owner.home);

    // Delegated: the trigger executor and the one named credential.
    assert!(caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));
    assert!(caps.covers(&Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SLACK_TOKEN")
    )));
    // Baseline still applies underneath.
    assert!(caps.covers(&Cap::new(
        Verb::FS_READ,
        Scope::path(owner.home.join("notes.md").to_string_lossy().into_owned())
    )));

    // Never the other subsystem's executor, a credential glob, a
    // sibling credential, egress, package mutation or global deletes.
    assert!(!caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::SECRET_READ, Scope::name("default/OTHER"))));
    assert!(!caps.covers(&Cap::new(Verb::SECRET_READ, Scope::name("**"))));
    assert!(!caps.covers(&Cap::new(Verb::NET_DIAL, Scope::host("example.com"))));
    assert!(!caps.covers(&Cap::new(Verb::SYS_PACKAGE, Scope::name("curl"))));
    assert!(!caps.covers(&Cap::new(Verb::FS_DELETE, Scope::path("/etc/hosts"))));
    assert!(!caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/etc/shadow"))));
}

/// A cron snapshot keeps `proc.spawn` and not `agent.spawn`; the
/// mapping is one-to-one so a job can never borrow the other
/// subsystem's executor.
#[test]
fn cron_and_trigger_delegations_do_not_borrow_each_other() {
    let _lock = lock_env();
    let owner = owner();
    let mut stored = CapSet::new();
    stored.insert(Cap::new(Verb::PROC_SPAWN, Scope::Wild));
    stored.insert(Cap::new(Verb::AGENT_SPAWN, Scope::Wild));

    let cron = meta_with_origin(owner.uid, Some(SessionOrigin::CronDelegation));
    let caps = scoped_caps(&stored, &cron, true, owner.uid, &owner.home);
    assert!(caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));

    let trigger = meta_with_origin(owner.uid, Some(SessionOrigin::TriggerDelegation));
    let caps = scoped_caps(&stored, &trigger, true, owner.uid, &owner.home);
    assert!(caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)));
}

/// The same snapshot on a record the owner could have authored, or
/// replayed as an ambient task, delegates nothing.
#[test]
fn a_forged_or_ambient_snapshot_delegates_nothing() {
    let _lock = lock_env();
    let owner = owner();
    let stored = approved_trigger_snapshot(&owner);

    let forged = meta_with_origin(owner.uid, Some(SessionOrigin::TriggerDelegation));
    let caps = scoped_caps(&stored, &forged, false, owner.uid, &owner.home);
    assert!(!caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SLACK_TOKEN")
    )));

    let ambient = meta_with_origin(owner.uid, Some(SessionOrigin::SystemAgentTask));
    let caps = scoped_caps(&stored, &ambient, true, owner.uid, &owner.home);
    assert!(!caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(
        Verb::SECRET_READ,
        Scope::name("default/SLACK_TOKEN")
    )));
}

/// The path half of a delegated snapshot is re-derived from the account
/// the clamp runs for, so a snapshot naming another home — or root's —
/// gains nothing from it.
#[test]
fn a_delegated_snapshot_never_reaches_another_account() {
    let _lock = lock_env();
    let owner = owner();
    let neighbour_file = owner
        .other_home
        .join("private.txt")
        .to_string_lossy()
        .into_owned();

    let mut stored = CapSet::new();
    stored.insert(Cap::new(Verb::AGENT_SPAWN, Scope::Wild));
    stored.insert(Cap::new(
        Verb::FS_READ,
        Scope::path(format!("{}/**", owner.other_home.display())),
    ));
    stored.insert(Cap::new(Verb::FS_READ, Scope::path("/root/**")));
    stored.insert(Cap::new(Verb::FS_WRITE, Scope::path("/**")));

    let meta = meta_with_origin(owner.uid, Some(SessionOrigin::TriggerDelegation));
    let caps = scoped_caps(&stored, &meta, true, owner.uid, &owner.home);

    assert!(caps.covers(&Cap::new(Verb::AGENT_SPAWN, Scope::Wild)));
    assert!(!caps.covers(&Cap::new(Verb::FS_READ, Scope::path(&neighbour_file))));
    assert!(!caps.covers(&Cap::new(Verb::FS_READ, Scope::path("/root/.bashrc"))));
    assert!(!caps.covers(&Cap::new(Verb::FS_WRITE, Scope::path("/etc/passwd"))));
}
