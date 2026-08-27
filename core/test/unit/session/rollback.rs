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
