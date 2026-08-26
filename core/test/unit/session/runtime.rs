use super::*;

/// Audit fix (session/runtime.rs HIGH "resume from crashed"):
/// a session whose on-disk `meta.status` is still `Running`
/// because the previous holder crashed before it could flip to
/// `Paused` must be resumable — otherwise the session is
/// permanently wedged. The proof of crash is that
/// `lease::try_acquire` succeeds: the kernel only releases the
/// flock when the holding process exits.
#[test]
fn resume_from_crashed_state() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().expect("tempdir");
    let prev_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", dir.path());

    // Create a session and flip it to Running directly — we do
    // NOT acquire a lease, mimicking a process that died after
    // updating meta but before holding the flock long enough for
    // any orderly handoff.
    let sid = store::create("test").expect("create session");
    store::update_meta(&sid, |m| {
        m.status = Status::Running;
    })
    .expect("flip to Running");

    // Sanity: no current process holds the flock.
    assert_eq!(
        store::get_meta(&sid).unwrap().status,
        Status::Running,
        "precondition: meta should report Running"
    );

    // Resume from a "crashed Running" state. Audit fix says this
    // must succeed (not return InvalidStatus) and return a
    // handle that re-stamps the lease.
    let handle = resume(&sid, "runtime-test").expect(
        "resume must accept Status::Running when the prior lease holder is gone",
    );

    // The returned handle owns a fresh lease so a subsequent
    // resume from a competing process would now see `Held`.
    assert!(handle.lease.is_some(), "resume should re-acquire lease");

    // And the meta should still be Running (resume re-stamps).
    assert_eq!(
        store::get_meta(&sid).unwrap().status,
        Status::Running,
        "post-resume status should remain Running"
    );

    // Cleanly drop the handle without finish(); that's a
    // separate concern — what matters is that resume() didn't
    // refuse the crashed-Running state.
    drop(handle);

    match prev_data {
        Some(v) => std::env::set_var("COS_DATA_DIR", v),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
}

/// Resume from a normal `Paused` state still works (regression
/// guard so the audit fix's "also accept Running" doesn't
/// inadvertently break the canonical happy path).
#[test]
fn resume_from_paused_state() {
    let _lock = crate::test_env::lock_env();
    let dir = tempfile::tempdir().expect("tempdir");
    let prev_data = std::env::var_os("COS_DATA_DIR");
    std::env::set_var("COS_DATA_DIR", dir.path());

    let sid = store::create("test").expect("create session");
    store::update_meta(&sid, |m| {
        m.status = Status::Paused;
    })
    .expect("flip to Paused");

    let handle = resume(&sid, "runtime-test").expect("resume from Paused");
    assert!(handle.lease.is_some());
    drop(handle);

    match prev_data {
        Some(v) => std::env::set_var("COS_DATA_DIR", v),
        None => std::env::remove_var("COS_DATA_DIR"),
    }
}
