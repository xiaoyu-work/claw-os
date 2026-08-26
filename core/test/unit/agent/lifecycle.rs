use super::*;
use std::env;

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    crate::test_env::lock_env()
}

struct DataDirGuard {
    prev: Option<std::ffi::OsString>,
    _tmp: tempfile::TempDir,
}

impl Drop for DataDirGuard {
    fn drop(&mut self) {
        match self.prev.take() {
            Some(v) => env::set_var("COS_DATA_DIR", v),
            None => env::remove_var("COS_DATA_DIR"),
        }
    }
}

fn redirect_data_dir() -> DataDirGuard {
    let tmp = tempfile::tempdir().unwrap();
    let prev = env::var_os("COS_DATA_DIR");
    env::set_var("COS_DATA_DIR", tmp.path());
    DataDirGuard { prev, _tmp: tmp }
}

#[test]
fn ls_returns_empty_when_no_sessions() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let v = ls(&[]).unwrap();
    assert_eq!(v["n"], 0);
    assert!(v["tasks"].as_array().unwrap().is_empty());
}

#[test]
fn ls_lists_all_sessions_with_status_and_lease() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let s1 = session::create("first").unwrap();
    let s2 = session::create("second").unwrap();
    // Hold a lease on s1 so we can confirm it shows up.
    let _g = session::try_acquire(&s1).unwrap();

    let v = ls(&[]).unwrap();
    assert_eq!(v["n"], 2);
    let tasks = v["tasks"].as_array().unwrap();
    let s1_row = tasks.iter().find(|r| r["id"] == s1.as_str()).unwrap();
    let s2_row = tasks.iter().find(|r| r["id"] == s2.as_str()).unwrap();
    assert_eq!(s1_row["purpose"], "first");
    assert!(s1_row["lease"].is_object(), "s1 has lease");
    assert!(s2_row["lease"].is_null(), "s2 has no lease");
}

#[test]
fn show_returns_404_style_error_for_missing_sid() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    // Valid-looking sid that does not exist.
    let bogus = "ses_0000000000000_000000000000".to_string();
    let err = show(&[bogus]).unwrap_err();
    assert!(err.contains("read meta"), "got {err}");
}

#[test]
fn show_summarises_turns_and_mutations() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("with content").unwrap();
    session::append_turn(&sid, session::Turn::text(session::TurnRole::User, "hi")).unwrap();
    session::record_mutation(
        &sid,
        session::MutationRecord::new(session::Mutation::FsRename {
            from: "/a".into(),
            to: "/b".into(),
        }),
    )
    .unwrap();

    let v = show(&[sid.as_str().into()]).unwrap();
    assert_eq!(v["turns"]["count"], 1);
    assert_eq!(v["mutations"]["count"], 1);
    assert_eq!(v["mutations"]["by_kind"]["fs.rename"], 1);
}

#[test]
fn owner_filtered_lifecycle_hides_and_blocks_other_users() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let alice = session::create("alice").unwrap();
    let bob = session::create("bob").unwrap();
    session::update_meta(&alice, |meta| meta.owner_uid = Some(1001)).unwrap();
    session::update_meta(&bob, |meta| meta.owner_uid = Some(1002)).unwrap();

    let listed = ls_for_owner(&[], 1001).unwrap();
    assert_eq!(listed["n"], 1);
    assert_eq!(listed["tasks"][0]["id"], alice.as_str());
    let error = show_for_owner(&[bob.as_str().into()], 1001).unwrap_err();
    assert_eq!(error, "task not found");
    let error = stop_for_owner(&[bob.as_str().into()], 1001).unwrap_err();
    assert_eq!(error, "task not found");
    assert!(!stop_sentinel(&bob).exists());
}

#[test]
fn stop_with_no_holder_marks_session_paused() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("idle").unwrap();
    session::update_meta(&sid, |m| m.status = Status::Running).unwrap();

    let v = stop(&[sid.as_str().into()]).unwrap();
    assert_eq!(v["action"], "marked-paused");
    let meta = session::get_meta(&sid).unwrap();
    assert_eq!(meta.status, Status::Paused);
}

#[test]
fn stop_with_live_holder_only_writes_sentinel() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("running").unwrap();
    session::update_meta(&sid, |m| m.status = Status::Running).unwrap();
    let _g = session::try_acquire(&sid).unwrap();

    let v = stop(&[sid.as_str().into()]).unwrap();
    assert_eq!(v["action"], "sentinel-written");
    // Meta status NOT flipped because someone is making progress.
    let meta = session::get_meta(&sid).unwrap();
    assert_eq!(meta.status, Status::Running);
    assert!(stop_sentinel(&sid).exists());
}

#[test]
fn undo_dry_run_lists_mutations_newest_first() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("undo dry").unwrap();
    for i in 0..3 {
        session::record_mutation(
            &sid,
            session::MutationRecord::new(session::Mutation::Opaque {
                verb: format!("step.{i}"),
                forward: json!({"i": i}),
                inverse: json!({}),
            }),
        )
        .unwrap();
    }

    let v = undo(&[sid.as_str().into(), "--dry-run".into()]).unwrap();
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["n"], 3);
    let entries = v["entries"].as_array().unwrap();
    assert_eq!(entries[0]["seq"], 2, "newest first");
    assert_eq!(entries[2]["seq"], 0);
}

#[test]
fn resume_flips_paused_to_pending_and_clears_sentinel() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("paused task").unwrap();
    session::update_meta(&sid, |m| m.status = Status::Paused).unwrap();
    // Pretend a previous stop wrote the sentinel.
    let s = stop_sentinel(&sid);
    std::fs::create_dir_all(s.parent().unwrap()).ok();
    std::fs::write(&s, b"x").unwrap();

    let v = resume(&[sid.as_str().into()]).unwrap();
    assert_eq!(v["status"], "pending");
    let meta = session::get_meta(&sid).unwrap();
    assert_eq!(meta.status, Status::Pending);
    assert!(!s.exists(), "sentinel cleared");
}

#[test]
fn resume_refuses_running_or_terminal_sessions() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("running").unwrap();
    session::update_meta(&sid, |m| m.status = Status::Running).unwrap();
    let err = resume(&[sid.as_str().into()]).unwrap_err();
    assert!(err.contains("cannot resume"), "got {err}");
}

#[test]
fn resume_refuses_when_lease_still_held() {
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("paused but locked").unwrap();
    session::update_meta(&sid, |m| m.status = Status::Paused).unwrap();
    let _g = session::try_acquire(&sid).unwrap();
    let err = resume(&[sid.as_str().into()]).unwrap_err();
    assert!(err.contains("lease"), "got {err}");
}

#[test]
fn stop_no_toctou() {
    // Regression: two concurrent `cos agent stop` calls on the
    // same session must serialize through the stop.lock flock.
    // Without it, both observe `lease.is_none() && active` from
    // their independent reads and both write a meta — one of
    // those updates is then silently lost. With the lock, the
    // second call sees Status::Paused inside the lock and
    // chooses the "sentinel-written" branch instead.
    let _l = lock_env();
    let _d = redirect_data_dir();
    let sid = session::create("toctou").unwrap();
    session::update_meta(&sid, |m| m.status = Status::Running).unwrap();
    let sid_a = sid.clone();
    let sid_b = sid.clone();
    let h1 = std::thread::spawn(move || stop(&[sid_a.as_str().into()]).unwrap());
    let h2 = std::thread::spawn(move || stop(&[sid_b.as_str().into()]).unwrap());
    let r1 = h1.join().unwrap();
    let r2 = h2.join().unwrap();
    // Exactly one caller observed the transition to Paused. The
    // other ran *after* the lock and so saw Paused already and
    // returned "sentinel-written".
    let actions: Vec<String> = [&r1, &r2]
        .iter()
        .map(|v| v["action"].as_str().unwrap_or("").to_string())
        .collect();
    let paused = actions
        .iter()
        .filter(|a| a.as_str() == "marked-paused")
        .count();
    let sentinels = actions
        .iter()
        .filter(|a| a.as_str() == "sentinel-written")
        .count();
    assert_eq!(
        paused, 1,
        "exactly one stop() should flip Paused, got actions={actions:?}"
    );
    assert_eq!(
        sentinels, 1,
        "the loser should report sentinel-written, got actions={actions:?}"
    );
    // Final state must be Paused regardless of ordering.
    let meta = session::get_meta(&sid).unwrap();
    assert_eq!(meta.status, Status::Paused);
}
