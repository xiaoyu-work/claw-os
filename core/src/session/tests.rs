//! End-to-end tests for the durable session module. Unit tests for
//! each sub-module live alongside their code; this file covers the
//! interactions (create + write + read + list back) plus the messier
//! crash / concurrency edge cases.

use std::env;

use serde_json::json;
use tempfile::TempDir;

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::*;

// All tests in this file mutate the global `COS_DATA_DIR` env var,
// so we serialize them on the process-wide `crate::test_env::ENV_LOCK`.
// (Per-module mutexes are not enough: cargo runs every test module
// in the same binary on a thread pool, so two modules can race.)

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    crate::test_env::lock_env()
}

/// RAII: redirect `COS_DATA_DIR` to a fresh tempdir, restore on drop.
struct DataDirGuard {
    prev: Option<std::ffi::OsString>,
    _tmp: TempDir,
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
    let tmp = tempfile::tempdir().expect("tempdir");
    let prev = env::var_os("COS_DATA_DIR");
    env::set_var("COS_DATA_DIR", tmp.path());
    DataDirGuard { prev, _tmp: tmp }
}

#[test]
fn create_writes_layout_on_disk() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("整理发票").expect("create");

    let dir = session_dir(&sid);
    assert!(dir.is_dir(), "session dir exists");
    assert!(dir.join("meta.json").is_file(), "meta.json exists");
    assert!(dir.join("caps.json").is_file(), "caps.json exists");
    assert!(dir.join("files").is_dir(), "files/ exists");

    // JSONL logs are created lazily on first append.
    assert!(!dir.join("turns.jsonl").exists());
    assert!(!dir.join("mutations.jsonl").exists());

    // lease.json is owned by Phase 2 — must not exist yet.
    assert!(!dir.join("lease.json").exists());
}

#[test]
fn meta_round_trip_through_disk() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("hello").unwrap();
    let m = get_meta(&sid).unwrap();
    assert_eq!(m.id, sid);
    assert_eq!(m.purpose, "hello");
    assert_eq!(m.status, Status::Pending);
    assert!(!m.created_at.is_empty());
    assert!(m.ended_at.is_none());
}

#[test]
fn update_meta_persists() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("x").unwrap();
    update_meta(&sid, |m| {
        m.status = Status::Running;
        m.purpose = "renamed".into();
        m.budget = Budget {
            tokens: Some(10_000),
            wall_seconds: None,
            mutations: Some(50),
        };
    })
    .unwrap();

    let m = get_meta(&sid).unwrap();
    assert_eq!(m.status, Status::Running);
    assert_eq!(m.purpose, "renamed");
    assert_eq!(m.budget.tokens, Some(10_000));
    assert_eq!(m.budget.mutations, Some(50));
}

/// Two writers hammering `update_meta` concurrently must not lose
/// updates. Pre-fix `update_meta` released its shared lock at the
/// end of `get_meta` and reacquired an exclusive lock for the
/// write, leaving a window in which a second writer could land
/// between the two — both writers would read the same starting
/// value and one update would silently overwrite the other.
#[test]
fn update_meta_serializes_concurrent_writers() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("counter").unwrap();
    // Use `budget.tokens` as a shared counter; it's an Option<u64>
    // we can drive monotonically.
    update_meta(&sid, |m| {
        m.budget = Budget {
            tokens: Some(0),
            wall_seconds: None,
            mutations: None,
        };
    })
    .unwrap();

    let increments_per_thread = 200u64;
    let sid_a = sid.clone();
    let sid_b = sid.clone();
    let h1 = std::thread::spawn(move || {
        for _ in 0..increments_per_thread {
            update_meta(&sid_a, |m| {
                let cur = m.budget.tokens.unwrap_or(0);
                m.budget.tokens = Some(cur + 1);
            })
            .unwrap();
        }
    });
    let h2 = std::thread::spawn(move || {
        for _ in 0..increments_per_thread {
            update_meta(&sid_b, |m| {
                let cur = m.budget.tokens.unwrap_or(0);
                m.budget.tokens = Some(cur + 1);
            })
            .unwrap();
        }
    });
    h1.join().unwrap();
    h2.join().unwrap();

    let m = get_meta(&sid).unwrap();
    assert_eq!(
        m.budget.tokens,
        Some(2 * increments_per_thread),
        "concurrent update_meta must not lose updates"
    );
}

/// Two writers writing distinct runtime keys to the same
/// `state.json` concurrently must both end up in the file.
/// Pre-fix `write_state` did read_json → mutate → write_json
/// without holding a lock across both halves, so writer B could
/// land between A's read and A's write and lose A's entry.
#[test]
fn write_state_preserves_concurrent_runtime_entries() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("multi-runtime").unwrap();
    let sid_a = sid.clone();
    let sid_b = sid.clone();
    let iterations = 50u64;
    let h1 = std::thread::spawn(move || {
        for i in 0..iterations {
            write_state(&sid_a, "alpha", serde_json::json!({"i": i})).unwrap();
        }
    });
    let h2 = std::thread::spawn(move || {
        for i in 0..iterations {
            write_state(&sid_b, "beta", serde_json::json!({"i": i})).unwrap();
        }
    });
    h1.join().unwrap();
    h2.join().unwrap();

    let alpha = read_state(&sid, "alpha").unwrap();
    let beta = read_state(&sid, "beta").unwrap();
    assert!(
        !alpha.is_null(),
        "alpha runtime entry lost: concurrent writers raced"
    );
    assert!(
        !beta.is_null(),
        "beta runtime entry lost: concurrent writers raced"
    );
}

#[test]
fn caps_round_trip_through_disk() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("caps test").unwrap();
    // Fresh session starts empty.
    assert!(get_caps(&sid).unwrap().is_empty());

    let caps = CapSet::from_caps([
        Cap::new(Verb::FS_READ, Scope::path("/workspace/**")),
        Cap::new(Verb::NET_DIAL, Scope::host("*.github.com:443")),
    ]);
    set_caps(&sid, &caps).unwrap();
    let back = get_caps(&sid).unwrap();
    assert_eq!(back, caps);
}

#[test]
fn append_turn_assigns_monotonic_seq() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("turn test").unwrap();
    let s0 = append_turn(&sid, Turn::text(TurnRole::User, "hi")).unwrap();
    let s1 = append_turn(&sid, Turn::text(TurnRole::Assistant, "hello")).unwrap();
    let s2 = append_turn(&sid, Turn::text(TurnRole::User, "bye")).unwrap();
    assert_eq!((s0, s1, s2), (0, 1, 2));

    let turns = iter_turns(&sid).unwrap();
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0].seq, 0);
    assert_eq!(turns[0].content, "hi");
    assert_eq!(turns[2].seq, 2);
    assert!(!turns[0].at.is_empty(), "at auto-stamped");
}

#[test]
fn append_turn_tolerates_trailing_partial_line() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("crash test").unwrap();
    append_turn(&sid, Turn::text(TurnRole::User, "intact")).unwrap();

    // Simulate a crash mid-write: append a malformed half-line.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(session_dir(&sid).join("turns.jsonl"))
        .unwrap();
    writeln!(f, "{{\"role\":\"user\",\"content\":\"partia").unwrap();
    drop(f);

    // Iter skips the bad line, returns the intact one.
    let turns = iter_turns(&sid).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].content, "intact");

    // And the next append continues monotonically: seq counts file
    // lines, so it goes to 2 (skipping over the bad line's slot).
    // The exact seq isn't important here — what matters is that
    // append doesn't blow up.
    let next = append_turn(&sid, Turn::text(TurnRole::User, "after crash")).unwrap();
    assert!(next >= 1, "seq advanced past crash, got {next}");
}

#[test]
fn record_mutation_round_trip() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("mutation test").unwrap();
    let m0 = record_mutation(
        &sid,
        MutationRecord::new(Mutation::FsWrite {
            path: "/workspace/a.txt".into(),
            prev_blob: None,
        }),
    )
    .unwrap();
    let m1 = record_mutation(
        &sid,
        MutationRecord::new(Mutation::FsWrite {
            path: "/workspace/a.txt".into(),
            prev_blob: Some("blob-1".into()),
        })
        .with_turn(7),
    )
    .unwrap();
    assert_eq!((m0, m1), (0, 1));

    let muts = iter_mutations(&sid).unwrap();
    assert_eq!(muts.len(), 2);
    assert_eq!(muts[0].seq, 0);
    assert_eq!(muts[1].seq, 1);
    assert_eq!(muts[1].turn_seq, Some(7));
    match &muts[1].mutation {
        Mutation::FsWrite { path, prev_blob } => {
            assert_eq!(path, "/workspace/a.txt");
            assert_eq!(prev_blob.as_deref(), Some("blob-1"));
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn state_per_runtime_isolation() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("state test").unwrap();
    write_state(&sid, "cos-agent", json!({"step": 1})).unwrap();
    write_state(&sid, "langchain", json!({"chain": "rag"})).unwrap();

    assert_eq!(
        read_state(&sid, "cos-agent").unwrap(),
        json!({"step": 1})
    );
    assert_eq!(
        read_state(&sid, "langchain").unwrap(),
        json!({"chain": "rag"})
    );
    // Unknown runtime → null, not an error.
    assert_eq!(read_state(&sid, "nope").unwrap(), json!(null));

    // Updating one runtime preserves the other.
    write_state(&sid, "cos-agent", json!({"step": 2})).unwrap();
    assert_eq!(read_state(&sid, "cos-agent").unwrap(), json!({"step": 2}));
    assert_eq!(
        read_state(&sid, "langchain").unwrap(),
        json!({"chain": "rag"})
    );

    // Writing null removes the key.
    write_state(&sid, "cos-agent", json!(null)).unwrap();
    assert_eq!(read_state(&sid, "cos-agent").unwrap(), json!(null));
}

#[test]
fn list_returns_all_sessions_skipping_garbage() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let a = create("a").unwrap();
    let b = create("b").unwrap();
    let c = create("c").unwrap();

    // Plant garbage that list() should skip silently.
    let root = sessions_root();
    std::fs::create_dir_all(root.join("not-a-session")).unwrap();
    std::fs::create_dir_all(root.join("ses_garbage")).unwrap();
    std::fs::write(root.join("loose-file"), "x").unwrap();

    let listed = list().unwrap();
    let ids: std::collections::HashSet<_> = listed.into_iter().map(|m| m.id).collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&a));
    assert!(ids.contains(&b));
    assert!(ids.contains(&c));
}

#[test]
fn end_marks_terminal_and_stamps_ended_at() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("end test").unwrap();
    update_meta(&sid, |m| m.status = Status::Running).unwrap();

    end(&sid, Status::Done).unwrap();
    let m = get_meta(&sid).unwrap();
    assert_eq!(m.status, Status::Done);
    assert!(m.ended_at.is_some());

    // Second call is a no-op (idempotent).
    let first_end = m.ended_at.clone();
    end(&sid, Status::Failed).unwrap();
    let m2 = get_meta(&sid).unwrap();
    assert_eq!(m2.status, Status::Done, "terminal status preserved");
    assert_eq!(m2.ended_at, first_end);
}

#[test]
fn missing_session_returns_not_found() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let phantom = SessionId::generate();
    assert!(matches!(
        get_meta(&phantom),
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        get_caps(&phantom),
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        append_turn(&phantom, Turn::text(TurnRole::User, "")),
        Err(SessionError::NotFound(_))
    ));
    assert!(matches!(
        record_mutation(
            &phantom,
            MutationRecord::new(Mutation::FsWrite {
                path: "/x".into(),
                prev_blob: None
            })
        ),
        Err(SessionError::NotFound(_))
    ));
}

#[test]
fn concurrent_appends_do_not_lose_lines() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("concurrent").unwrap();
    let n_threads = 8;
    let per_thread = 64;
    std::thread::scope(|s| {
        for t in 0..n_threads {
            let sid = sid.clone();
            s.spawn(move || {
                for i in 0..per_thread {
                    let _ = append_turn(
                        &sid,
                        Turn::text(TurnRole::User, format!("t{t}-i{i}")),
                    );
                }
            });
        }
    });

    let turns = iter_turns(&sid).unwrap();
    assert_eq!(turns.len(), n_threads * per_thread);
    // All seqs distinct and contiguous from 0..N.
    let mut seqs: Vec<u64> = turns.iter().map(|t| t.seq).collect();
    seqs.sort_unstable();
    let expected: Vec<u64> = (0..(n_threads * per_thread) as u64).collect();
    assert_eq!(seqs, expected, "seqs should cover the contiguous range");
}

#[test]
fn iter_on_empty_session_returns_empty_vec() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("empty").unwrap();
    assert!(iter_turns(&sid).unwrap().is_empty());
    assert!(iter_mutations(&sid).unwrap().is_empty());
}

#[test]
fn sessions_root_respects_data_dir_env() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    assert!(sessions_root().starts_with(crate::paths::data_dir()));
    assert!(sessions_root().ends_with("sessions"));
}

// ---------------------------------------------------------------------------
// GC / archive (Phase 1.4)
// ---------------------------------------------------------------------------

use std::time::Duration;

/// Force `meta.ended_at` to an arbitrary RFC3339 string. Tests use this
/// to age a session past the gc threshold without sleeping.
fn force_ended_at(sid: &SessionId, rfc3339: &str) {
    update_meta(sid, |m| {
        m.status = Status::Done;
        m.ended_at = Some(rfc3339.to_string());
    })
    .expect("update_meta");
}

#[test]
fn gc_archive_zips_done_session_and_removes_dir() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("first-task").expect("create");
    append_turn(
        &sid,
        Turn::text(TurnRole::User, "hello"),
    )
    .expect("turn");
    force_ended_at(&sid, "2000-01-01T00:00:00Z");

    let stats = gc_archive(Duration::ZERO).expect("gc");
    assert_eq!(stats.archived, vec![sid.clone()]);
    assert_eq!(stats.skipped_active, 0);
    assert_eq!(stats.skipped_too_recent, 0);
    assert!(stats.errors.is_empty(), "errors: {:?}", stats.errors);

    assert!(!session_dir(&sid).exists(), "original dir gone");
    assert!(is_archived(&sid), "zip present");
    assert!(archive_path(&sid).is_file());
}

#[test]
fn gc_archive_skips_active_sessions() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("running").expect("create");
    // Status::Pending is active — never ended.

    let stats = gc_archive(Duration::ZERO).expect("gc");
    assert!(stats.archived.is_empty());
    assert_eq!(stats.skipped_active, 1);
    assert!(session_dir(&sid).exists(), "active session untouched");
    assert!(!is_archived(&sid));
}

#[test]
fn gc_archive_skips_terminal_session_younger_than_threshold() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("just-finished").expect("create");
    // ended_at = now, threshold = 1 day → should be skipped.
    end(&sid, Status::Done).expect("end");

    let stats = gc_archive(Duration::from_secs(86_400)).expect("gc");
    assert!(stats.archived.is_empty());
    assert_eq!(stats.skipped_too_recent, 1);
    assert!(session_dir(&sid).exists());
    assert!(!is_archived(&sid));
}

#[test]
fn gc_archive_skips_terminal_session_with_missing_ended_at() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("borked").expect("create");
    // Force terminal status without setting ended_at — represents a
    // hand-edited meta or a future-version meta we don't fully parse.
    update_meta(&sid, |m| {
        m.status = Status::Failed;
        m.ended_at = None;
    })
    .unwrap();

    let stats = gc_archive(Duration::ZERO).expect("gc");
    assert!(stats.archived.is_empty());
    assert_eq!(stats.skipped_too_recent, 1);
}

#[test]
fn gc_archive_runs_with_no_sessions() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let stats = gc_archive(Duration::ZERO).expect("gc");
    assert!(stats.archived.is_empty());
    assert_eq!(stats.skipped_active, 0);
    assert_eq!(stats.skipped_too_recent, 0);
}

#[test]
fn gc_archive_processes_many_sessions_independently() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let a = create("a").unwrap();
    let b = create("b").unwrap();
    let c = create("c").unwrap();

    force_ended_at(&a, "2000-01-01T00:00:00Z"); // old → archive
    end(&b, Status::Done).unwrap(); // fresh terminal → skip
    // c stays Pending → skip active

    let stats = gc_archive(Duration::from_secs(86_400)).unwrap();
    assert_eq!(stats.archived, vec![a.clone()]);
    assert_eq!(stats.skipped_active, 1);
    assert_eq!(stats.skipped_too_recent, 1);

    assert!(!session_dir(&a).exists());
    assert!(session_dir(&b).exists());
    assert!(session_dir(&c).exists());
}

#[test]
fn list_skips_archive_directory() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("to-archive").expect("create");
    force_ended_at(&sid, "2000-01-01T00:00:00Z");
    gc_archive(Duration::ZERO).expect("gc");

    // .archive/ must not appear as a session, and the archived sid must
    // not appear in list() (its dir is gone).
    let metas = list().expect("list");
    assert!(metas.is_empty(), "list() saw {:?}", metas);
}

#[test]
fn archived_zip_contains_meta_and_turns() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("trace-me").expect("create");
    append_turn(&sid, Turn::text(TurnRole::User, "ping"))
        .expect("turn");
    force_ended_at(&sid, "2000-01-01T00:00:00Z");

    gc_archive(Duration::ZERO).expect("gc");

    let zip_path = archive_path(&sid);
    let file = std::fs::File::open(&zip_path).expect("open zip");
    let mut archive = zip::ZipArchive::new(file).expect("read zip");

    let names: Vec<String> = archive.file_names().map(String::from).collect();
    assert!(names.contains(&"meta.json".to_string()), "names = {names:?}");
    assert!(names.contains(&"caps.json".to_string()), "names = {names:?}");
    assert!(names.contains(&"turns.jsonl".to_string()), "names = {names:?}");

    let mut meta_buf = String::new();
    std::io::Read::read_to_string(
        &mut archive.by_name("meta.json").unwrap(),
        &mut meta_buf,
    )
    .unwrap();
    assert!(meta_buf.contains("trace-me"), "meta.json content: {meta_buf}");
}

// ---------------------------------------------------------------------------
// Lease (Phase 2)
// ---------------------------------------------------------------------------

#[test]
fn try_acquire_returns_not_found_for_unknown_session() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let bogus: SessionId = "ses_018f4ae0c2300_a1b2c3d4e5f6".parse().unwrap();
    match try_acquire(&bogus) {
        Err(AcquireError::NotFound(s)) => assert_eq!(s, "ses_018f4ae0c2300_a1b2c3d4e5f6"),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn try_acquire_succeeds_on_fresh_session_and_writes_lease_json() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("lease-me").expect("create");
    assert!(current_lease(&sid).unwrap().is_none(), "no holder yet");

    let guard = try_acquire(&sid).expect("acquire");
    assert_eq!(guard.sid(), &sid);
    assert_eq!(guard.pid(), std::process::id());

    let lease = current_lease(&sid).expect("read").expect("present");
    assert_eq!(lease.pid, std::process::id());
    assert!(!lease.started_at.is_empty());
    assert!(!lease.heartbeat_at.is_empty());

    drop(guard);

    // After drop, lease.json is gone.
    assert!(
        current_lease(&sid).unwrap().is_none(),
        "lease.json should be removed on drop"
    );
}

#[test]
fn try_acquire_blocks_while_another_holder_lives() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("contended").expect("create");
    let first = try_acquire(&sid).expect("first acquire");

    // Second attempt in the same process must fail with Held, because
    // flock is per-fd: a new fd opened on the same file by the same
    // process is still treated as a competing locker.
    match try_acquire(&sid) {
        Err(AcquireError::Held { held_by }) => {
            assert_eq!(held_by.pid, std::process::id());
        }
        other => panic!("expected Held, got {other:?}"),
    }

    drop(first);

    // Once the first guard is dropped, another acquire succeeds.
    let second = try_acquire(&sid).expect("re-acquire after release");
    assert_eq!(second.pid(), std::process::id());
}

#[test]
fn heartbeat_updates_only_heartbeat_at() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("hb").expect("create");
    let guard = try_acquire(&sid).expect("acquire");

    let before = current_lease(&sid).unwrap().unwrap();
    // Wait long enough that the second-resolution RFC3339 stamp differs.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    guard.heartbeat().expect("heartbeat");
    let after = current_lease(&sid).unwrap().unwrap();

    assert_eq!(before.pid, after.pid, "pid unchanged");
    assert_eq!(before.started_at, after.started_at, "started_at unchanged");
    assert!(
        after.heartbeat_at >= before.heartbeat_at,
        "heartbeat_at moved forward: {} -> {}",
        before.heartbeat_at,
        after.heartbeat_at,
    );
}

#[test]
fn dropping_guard_lets_next_acquire_overwrite_holder_identity() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("rotate").expect("create");

    // First "tenant".
    let g1 = try_acquire(&sid).unwrap();
    let l1 = current_lease(&sid).unwrap().unwrap();
    drop(g1);

    // Second "tenant" — same process so same pid, but started_at must
    // bump to a fresh timestamp (or at least the lease json reappears
    // after Drop removed it).
    assert!(current_lease(&sid).unwrap().is_none());
    let _g2 = try_acquire(&sid).unwrap();
    let l2 = current_lease(&sid).unwrap().unwrap();
    assert_eq!(l1.pid, l2.pid);
    assert!(
        l2.started_at >= l1.started_at,
        "second acquire stamps a fresh started_at"
    );
}

#[test]
fn current_lease_is_none_when_session_never_acquired() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("idle").unwrap();
    assert!(current_lease(&sid).unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Phase 2.4 / 2.5 — promote_to_durable, pause, resume
// ---------------------------------------------------------------------------

/// Save/restore COS_SESSION around a closure so each promote/resume
/// test starts with a clean env regardless of test order.
fn with_cleared_session<F: FnOnce()>(f: F) {
    let prev = env::var_os("COS_SESSION");
    env::remove_var("COS_SESSION");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    match prev {
        Some(v) => env::set_var("COS_SESSION", v),
        None => env::remove_var("COS_SESSION"),
    }
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

#[test]
fn promote_creates_session_acquires_lease_and_sets_env() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let s = promote_to_durable("invoice agent", "cos-agent").unwrap();
        let sid = s.sid().clone();

        assert_eq!(env::var("COS_SESSION").unwrap(), sid.as_str());

        let meta = get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Running);
        assert_eq!(meta.creator_runtime.as_deref(), Some("cos-agent"));
        assert_eq!(meta.purpose, "invoice agent");

        let lease = current_lease(&sid).unwrap().expect("lease.json exists");
        assert_eq!(lease.pid, std::process::id());

        // Other processes must see the lease as held.
        match try_acquire(&sid) {
            Err(AcquireError::Held { .. }) => {}
            other => panic!("expected Held, got {other:?}"),
        }

        s.finish(Status::Done).unwrap();

        let meta = get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Done);
        assert!(meta.ended_at.is_some(), "ended_at stamped on finish");
        assert!(
            current_lease(&sid).unwrap().is_none(),
            "lease.json removed on finish"
        );
        assert!(env::var("COS_SESSION").is_err(), "env restored to unset");
    });
}

#[test]
fn promote_drop_without_finish_marks_failed() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let s = promote_to_durable("oops", "cos-agent").unwrap();
        let sid = s.sid().clone();
        drop(s);

        let meta = get_meta(&sid).unwrap();
        assert_eq!(
            meta.status,
            Status::Failed,
            "drop without finish marks Failed"
        );
        assert!(meta.ended_at.is_some());
        assert!(current_lease(&sid).unwrap().is_none());
    });
}

#[test]
fn promote_restores_previous_cos_session_env() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        env::set_var("COS_SESSION", "ses_outer_value");
        let s = promote_to_durable("nested", "cos-agent").unwrap();
        let sid = s.sid().clone();
        assert_eq!(env::var("COS_SESSION").unwrap(), sid.as_str());

        s.finish(Status::Done).unwrap();
        assert_eq!(
            env::var("COS_SESSION").unwrap(),
            "ses_outer_value",
            "outer COS_SESSION restored after finish"
        );
    });
}

#[test]
fn heartbeat_via_durable_session_refreshes_lease_json() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let s = promote_to_durable("hb", "cos-agent").unwrap();
        let sid = s.sid().clone();
        let before = current_lease(&sid).unwrap().unwrap();

        std::thread::sleep(std::time::Duration::from_millis(1100));
        s.heartbeat().unwrap();

        let after = current_lease(&sid).unwrap().unwrap();
        assert_eq!(after.pid, before.pid);
        assert_eq!(after.started_at, before.started_at);
        assert!(
            after.heartbeat_at > before.heartbeat_at,
            "heartbeat_at moved: {} -> {}",
            before.heartbeat_at,
            after.heartbeat_at
        );

        s.finish(Status::Done).unwrap();
    });
}

#[test]
fn pause_moves_status_to_paused_and_releases_lease() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let s = promote_to_durable("pausable", "cos-agent").unwrap();
        let sid = s.sid().clone();

        pause(s).unwrap();

        let meta = get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Paused);
        assert!(
            meta.ended_at.is_none(),
            "pause is not terminal — no ended_at"
        );
        assert!(
            current_lease(&sid).unwrap().is_none(),
            "lease.json gone after pause"
        );
        assert!(env::var("COS_SESSION").is_err(), "env restored on pause");

        // A fresh acquire from the same process must succeed.
        let g = try_acquire(&sid).unwrap();
        drop(g);
    });
}

#[test]
fn resume_picks_up_paused_session() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let s = promote_to_durable("hand-off", "cos-agent").unwrap();
        let sid = s.sid().clone();
        pause(s).unwrap();

        let s2 = resume(&sid, "langchain-py").unwrap();
        assert_eq!(s2.sid(), &sid);
        assert_eq!(s2.runtime(), "langchain-py");
        assert_eq!(env::var("COS_SESSION").unwrap(), sid.as_str());

        let meta = get_meta(&sid).unwrap();
        assert_eq!(meta.status, Status::Running);
        // creator_runtime was set by the original promote and must NOT
        // change on resume — it is the *creator*, not the *current*
        // runtime.
        assert_eq!(meta.creator_runtime.as_deref(), Some("cos-agent"));

        s2.finish(Status::Done).unwrap();
    });
}

#[test]
fn resume_rejects_non_paused_status() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let s = promote_to_durable("running", "cos-agent").unwrap();
        let sid = s.sid().clone();

        // Status is Running and the original holder `s` still owns the
        // flock. Audit fix (session/runtime.rs HIGH) explicitly
        // *allows* resuming a Running session when the prior holder
        // has crashed (proof: lease::try_acquire succeeds). Here the
        // prior holder is alive, so resume must fail — but with
        // `Lease(Held)`, not `InvalidStatus`, because the on-disk
        // status itself is a valid resume target.
        match resume(&sid, "other") {
            Err(TransitionError::Lease(_)) => {}
            other => panic!("expected Lease(Held), got {other:?}"),
        }

        s.finish(Status::Done).unwrap();

        // Status is Done. resume still refuses (terminal).
        match resume(&sid, "other") {
            Err(TransitionError::InvalidStatus {
                actual: Status::Done,
            }) => {}
            other => panic!("expected InvalidStatus(Done), got {other:?}"),
        }
    });
}

#[test]
fn resume_returns_not_found_for_missing_session() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let bogus: SessionId =
            "ses_0000000000000_000000000000".parse().unwrap();
        match resume(&bogus, "x") {
            Err(TransitionError::NotFound(s)) => assert_eq!(s, bogus.as_str()),
            other => panic!("expected NotFound, got {other:?}"),
        }
    });
}

#[test]
fn pause_then_resume_preserves_turns_and_mutations() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let s1 = promote_to_durable("with-history", "cos-agent").unwrap();
        let sid = s1.sid().clone();

        append_turn(&sid, Turn::text(TurnRole::User, "step 1")).unwrap();
        record_mutation(
            &sid,
            MutationRecord::new(Mutation::Opaque {
                verb: "step".into(),
                forward: json!({"step": 1}),
                inverse: json!({}),
            }),
        )
        .unwrap();

        pause(s1).unwrap();

        let s2 = resume(&sid, "second").unwrap();

        append_turn(&sid, Turn::text(TurnRole::User, "step 2")).unwrap();
        record_mutation(
            &sid,
            MutationRecord::new(Mutation::Opaque {
                verb: "step".into(),
                forward: json!({"step": 2}),
                inverse: json!({}),
            }),
        )
        .unwrap();

        let turns = iter_turns(&sid).unwrap();
        assert_eq!(turns.len(), 2, "turns survive pause/resume");
        assert_eq!(turns[0].content, "step 1");
        assert_eq!(turns[1].content, "step 2");
        assert_eq!(turns[0].seq, 0);
        assert_eq!(turns[1].seq, 1, "seq continues monotonically");

        let muts = iter_mutations(&sid).unwrap();
        assert_eq!(muts.len(), 2, "mutations survive pause/resume");
        assert_eq!(muts[0].seq, 0);
        assert_eq!(muts[1].seq, 1);

        s2.finish(Status::Done).unwrap();
    });
}

// ---------------------------------------------------------------------------
// Phase 2.6 — cross-process integration: lease auto-released on holder death
// ---------------------------------------------------------------------------
//
// The trick: re-exec our own test binary with a magic env var. The
// child path runs the SAME test function but checks the env at the
// top and switches into "child mode" — acquire the lease, abort. The
// kernel releases the flock on abort; the parent then asserts a fresh
// try_acquire succeeds.

const ENV_DEATH_CHILD_SID: &str = "COS_LEASE_DEATH_CHILD_SID";
const ENV_DEATH_CHILD_DATA: &str = "COS_LEASE_DEATH_CHILD_DATA";
const DEATH_CHILD_SENTINEL: &str = "CHILD-ACQUIRED-SESSION-LEASE";

#[test]
fn lease_released_when_holder_process_dies() {
    // CHILD MODE — runs in the spawned subprocess.
    if let Ok(sid_str) = env::var(ENV_DEATH_CHILD_SID) {
        // The parent passed COS_DATA_DIR via env (subprocess inherits).
        // Don't touch the global mutex — this is a different process,
        // it has its own copy.
        let sid: SessionId = sid_str.parse().expect("child sid valid");
        let _g = try_acquire(&sid).expect("child acquires lease");
        // Print a sentinel the parent can grep for, then abort. The
        // abort kills the process without running Drop (which would
        // remove lease.json), forcing the parent to verify recovery
        // when only the kernel-released flock signals lease freedom.
        eprintln!("{DEATH_CHILD_SENTINEL}");
        // Flush so the parent's stderr capture sees the sentinel.
        use std::io::Write;
        let _ = std::io::stderr().flush();
        std::process::abort();
    }

    // PARENT MODE
    let _lock = lock_env();
    let _data = redirect_data_dir();

    with_cleared_session(|| {
        let sid = create("crash-test").unwrap();
        let data_dir = env::var("COS_DATA_DIR").expect("redirect_data_dir set it");

        let exe = env::current_exe().expect("current_exe");
        let output = std::process::Command::new(&exe)
            .args([
                "--exact",
                "--nocapture",
                "session::tests::lease_released_when_holder_process_dies",
            ])
            .env(ENV_DEATH_CHILD_SID, sid.as_str())
            .env(ENV_DEATH_CHILD_DATA, &data_dir)
            .env("COS_DATA_DIR", &data_dir)
            // Belt and suspenders: clear COS_SESSION so child's bootstrap
            // (if any) doesn't try to attach to our env's value.
            .env_remove("COS_SESSION")
            .output()
            .expect("spawn child");

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(DEATH_CHILD_SENTINEL),
            "child should have acquired lease before aborting; stderr was:\n{stderr}"
        );
        assert!(
            !output.status.success(),
            "child must have aborted (non-zero exit)"
        );

        // The kernel released the flock when the child died. The
        // stale lease.json may still be on disk (child crashed before
        // Drop), but try_acquire must overwrite it and succeed.
        let g = match try_acquire(&sid) {
            Ok(g) => g,
            Err(e) => panic!(
                "parent failed to re-acquire lease after child death: {e}\nchild stderr:\n{stderr}"
            ),
        };
        assert_eq!(g.pid(), std::process::id());
        let lease = current_lease(&sid).unwrap().unwrap();
        assert_eq!(
            lease.pid,
            std::process::id(),
            "lease.json reflects new holder, not the dead child"
        );
        drop(g);
    });
}

// ---------------------------------------------------------------------------
// Phase 3 — inverse blob store, typed recorders, rollback
// ---------------------------------------------------------------------------

#[test]
fn write_blob_and_read_blob_round_trip() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("blob test").unwrap();
    let id1 = write_blob(&sid, b"hello world").unwrap();
    let id2 = write_blob(&sid, b"second blob").unwrap();
    assert_ne!(id1, id2, "fresh ids per call");
    assert_eq!(id1.len(), 32, "uuid simple format = 32 hex chars");

    assert_eq!(read_blob(&sid, &id1).unwrap(), b"hello world");
    assert_eq!(read_blob(&sid, &id2).unwrap(), b"second blob");

    let path = blob_path(&sid, &id1);
    assert!(path.starts_with(inverse_root(&sid)));
    assert!(path.is_file());
}

#[test]
fn read_blob_missing_returns_not_found() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let sid = create("blob missing").unwrap();
    let err = read_blob(&sid, "deadbeef").unwrap_err();
    assert!(matches!(err, SessionError::NotFound(_)), "got {err:?}");
}

#[test]
fn delete_blob_is_idempotent() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let sid = create("blob del").unwrap();
    let id = write_blob(&sid, b"x").unwrap();
    delete_blob(&sid, &id).unwrap();
    assert!(read_blob(&sid, &id).is_err());
    // Second delete is a no-op.
    delete_blob(&sid, &id).unwrap();
    delete_blob(&sid, "never-existed-id").unwrap();
}

#[test]
fn record_fs_write_snapshots_existing_file() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("write snapshot").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("hello.txt");
    std::fs::write(&path, b"original bytes").unwrap();

    let seq = record_fs_write(&sid, &path).unwrap();
    assert_eq!(seq, 0, "first mutation gets seq 0");

    let muts = iter_mutations(&sid).unwrap();
    assert_eq!(muts.len(), 1);
    let blob_id = match &muts[0].mutation {
        Mutation::FsWrite { path: p, prev_blob } => {
            assert_eq!(*p, path.to_string_lossy().into_owned());
            prev_blob.clone().expect("path existed, prev_blob must be Some")
        }
        other => panic!("expected FsWrite, got {other:?}"),
    };

    assert_eq!(read_blob(&sid, &blob_id).unwrap(), b"original bytes");
}

#[test]
fn record_fs_write_records_none_blob_for_missing_target() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("write fresh").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("does_not_exist.txt");

    record_fs_write(&sid, &path).unwrap();

    let muts = iter_mutations(&sid).unwrap();
    match &muts[0].mutation {
        Mutation::FsWrite { prev_blob: None, .. } => {}
        other => panic!("expected FsWrite{{prev_blob: None}}, got {other:?}"),
    }
}

#[test]
fn record_fs_delete_snapshots_bytes() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("delete snapshot").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("dies.txt");
    std::fs::write(&path, b"about to die").unwrap();

    record_fs_delete(&sid, &path).unwrap();

    let muts = iter_mutations(&sid).unwrap();
    let (recorded_path, blob_id) = match &muts[0].mutation {
        Mutation::FsDelete { path, blob_id } => (path.clone(), blob_id.clone()),
        other => panic!("expected FsDelete, got {other:?}"),
    };
    assert_eq!(recorded_path, path.to_string_lossy().into_owned());
    assert_eq!(read_blob(&sid, &blob_id).unwrap(), b"about to die");
}

#[test]
fn record_fs_delete_rejects_missing_file() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("delete missing").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("nothing.txt");
    let err = record_fs_delete(&sid, &path).unwrap_err();
    assert!(matches!(err, SessionError::NotFound(_)), "got {err:?}");
}

#[test]
fn record_fs_rename_records_both_paths() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("rename").unwrap();
    let work = tempfile::tempdir().unwrap();
    let from = work.path().join("a.txt");
    let to = work.path().join("b.txt");

    record_fs_rename(&sid, &from, &to).unwrap();

    let muts = iter_mutations(&sid).unwrap();
    match &muts[0].mutation {
        Mutation::FsRename { from: f, to: t } => {
            assert_eq!(*f, from.to_string_lossy().into_owned());
            assert_eq!(*t, to.to_string_lossy().into_owned());
        }
        other => panic!("expected FsRename, got {other:?}"),
    }
}

#[test]
fn rollback_restores_overwritten_file() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("rollback overwrite").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("doc.txt");
    std::fs::write(&path, b"v1").unwrap();

    record_fs_write(&sid, &path).unwrap();
    // Simulate the gated app actually performing the write:
    std::fs::write(&path, b"v2").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"v2");

    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].verb, "fs.write");
    assert_eq!(outcomes[0].status, RollbackStatus::Restored);
    assert_eq!(std::fs::read(&path).unwrap(), b"v1");
}

#[test]
fn rollback_deletes_file_created_from_nothing() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("rollback create").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("new.txt");

    record_fs_write(&sid, &path).unwrap();
    std::fs::write(&path, b"created").unwrap();
    assert!(path.exists());

    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes[0].status, RollbackStatus::Restored);
    assert!(!path.exists(), "rollback removed the path");
}

#[test]
fn rollback_undeletes_file_with_saved_bytes() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("rollback delete").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("important.txt");
    std::fs::write(&path, b"keep me").unwrap();

    record_fs_delete(&sid, &path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(!path.exists());

    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes[0].status, RollbackStatus::Restored);
    assert_eq!(std::fs::read(&path).unwrap(), b"keep me");
}

#[test]
fn rollback_reverses_rename() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("rollback rename").unwrap();
    let work = tempfile::tempdir().unwrap();
    let from = work.path().join("old.txt");
    let to = work.path().join("new.txt");
    std::fs::write(&from, b"x").unwrap();

    record_fs_rename(&sid, &from, &to).unwrap();
    std::fs::rename(&from, &to).unwrap();
    assert!(!from.exists() && to.exists());

    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes[0].status, RollbackStatus::Restored);
    assert!(from.exists() && !to.exists());
    assert_eq!(std::fs::read(&from).unwrap(), b"x");
}

#[test]
fn rollback_replays_in_reverse_seq_order() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("rollback order").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("doc.txt");
    std::fs::write(&path, b"v1").unwrap();

    // Two successive writes — the agent did v1 -> v2 -> v3.
    record_fs_write(&sid, &path).unwrap();
    std::fs::write(&path, b"v2").unwrap();
    record_fs_write(&sid, &path).unwrap();
    std::fs::write(&path, b"v3").unwrap();

    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes.len(), 2);
    // Newest-first: seq 1 then seq 0.
    assert_eq!(outcomes[0].seq, 1);
    assert_eq!(outcomes[1].seq, 0);
    // Both restored cleanly.
    assert!(outcomes.iter().all(|o| o.status == RollbackStatus::Restored));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"v1",
        "fully unwound back to original"
    );
}

#[test]
fn rollback_marks_already_done_when_path_already_absent() {
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("already done").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("created.txt");

    record_fs_write(&sid, &path).unwrap();
    // Imagine the user manually cleaned up the file before rollback.
    // Don't write anything to `path` — it never appeared.

    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes[0].status, RollbackStatus::AlreadyDone);
}

#[test]
fn rollback_skips_opaque_with_helpful_detail() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("opaque").unwrap();
    record_mutation(
        &sid,
        MutationRecord::new(Mutation::Opaque {
            verb: "calendar.event.create".into(),
            forward: json!({"event_id": "abc"}),
            inverse: json!({"verb": "calendar.event.delete", "event_id": "abc"}),
        }),
    )
    .unwrap();

    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes[0].status, RollbackStatus::Skipped);
    assert!(
        outcomes[0].detail.contains("calendar.event.create"),
        "detail names the verb: {}",
        outcomes[0].detail
    );
}

#[test]
fn rollback_on_empty_session_is_empty_vec() {
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("empty").unwrap();
    let outcomes = rollback(&sid).unwrap();
    assert!(outcomes.is_empty());
}

// ---------------------------------------------------------------------------
// Phase 3b — Python `cos-runtime/python/src/cos_runtime/snapshot.py` mirrors into mutations.jsonl
// ---------------------------------------------------------------------------
//
// These tests prove that the Python helper and the Rust kernel agree
// on the file-schema contract. We literally invoke `python3 -c "..."`
// with snapshot.py on the import path, then read the resulting
// mutations.jsonl back through Rust's iter_mutations + rollback.
//
// If `python3` is unavailable on the build host we skip the test —
// some CI containers strip python out, and this is a contract test,
// not a unit test.

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn apps_lib_dir() -> std::path::PathBuf {
    // tests run from the package dir (`core/`); the runtime helper
    // lives at `<repo>/cos-runtime/python/src/cos_runtime`, two
    // levels up.
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::PathBuf::from(manifest)
        .parent()
        .unwrap()
        .join("cos-runtime")
        .join("python")
        .join("src")
        .join("cos_runtime")
}

fn sdk_lib_dir() -> std::path::PathBuf {
    // Public SDK Python package at
    // `<repo>/claw-os-sdk/python/src/claw_os_sdk`.
    let manifest = env!("CARGO_MANIFEST_DIR");
    std::path::PathBuf::from(manifest)
        .parent()
        .unwrap()
        .join("claw-os-sdk")
        .join("python")
        .join("src")
        .join("claw_os_sdk")
}

fn run_python_snapshot(sid: &SessionId, path: &std::path::Path, op: &str) {
    let lib = apps_lib_dir();
    assert!(
        lib.join("snapshot.py").is_file(),
        "snapshot.py missing at {}",
        lib.display()
    );
    let data_dir = env::var("COS_DATA_DIR").expect("COS_DATA_DIR set");
    let script = format!(
        "import sys; sys.path.insert(0, {lib:?}); import snapshot; \
         snapshot.snapshot({path:?}, {op:?})",
        lib = lib.to_string_lossy(),
        path = path.to_string_lossy(),
        op = op,
    );
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .env("COS_DATA_DIR", &data_dir)
        .env("COS_SESSION", sid.as_str())
        .env("COS_SNAPSHOT", "1")
        .output()
        .expect("spawn python3");
    assert!(
        out.status.success(),
        "python snapshot.py failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

#[test]
fn python_snapshot_mirrors_into_durable_mutations_log() {
    if !python3_available() {
        eprintln!("python3 unavailable; skipping cross-language snapshot test");
        return;
    }
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("python bridge").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("doc.txt");
    std::fs::write(&path, b"original").unwrap();

    // Python side records the snapshot for an upcoming overwrite.
    run_python_snapshot(&sid, &path, "write");

    // Rust side reads it back from mutations.jsonl — proves the
    // schema (kebab-case kind, prev_blob field name, etc.) matches.
    let muts = iter_mutations(&sid).unwrap();
    assert_eq!(muts.len(), 1, "exactly one record");
    let blob_id = match &muts[0].mutation {
        Mutation::FsWrite { path: p, prev_blob } => {
            assert_eq!(*p, path.to_string_lossy().into_owned());
            prev_blob.clone().expect("file existed, blob recorded")
        }
        other => panic!("expected FsWrite, got {other:?}"),
    };
    assert_eq!(blob_id.len(), 32, "uuid simple hex");
    assert_eq!(read_blob(&sid, &blob_id).unwrap(), b"original");

    // End-to-end: simulate the gated app overwriting the file, then
    // run Rust rollback and verify the original bytes come back.
    std::fs::write(&path, b"overwritten").unwrap();
    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes[0].status, RollbackStatus::Restored);
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
}

#[test]
fn python_snapshot_rm_records_fs_delete_with_blob() {
    if !python3_available() {
        eprintln!("python3 unavailable; skipping cross-language snapshot test");
        return;
    }
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("python rm").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("bye.txt");
    std::fs::write(&path, b"farewell").unwrap();

    run_python_snapshot(&sid, &path, "rm");

    let muts = iter_mutations(&sid).unwrap();
    let (recorded_path, blob_id) = match &muts[0].mutation {
        Mutation::FsDelete { path, blob_id } => (path.clone(), blob_id.clone()),
        other => panic!("expected FsDelete, got {other:?}"),
    };
    assert_eq!(recorded_path, path.to_string_lossy().into_owned());
    assert_eq!(read_blob(&sid, &blob_id).unwrap(), b"farewell");

    // Simulate the actual delete and roll back.
    std::fs::remove_file(&path).unwrap();
    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes[0].status, RollbackStatus::Restored);
    assert_eq!(std::fs::read(&path).unwrap(), b"farewell");
}

#[test]
fn python_snapshot_records_absent_for_new_path() {
    if !python3_available() {
        eprintln!("python3 unavailable; skipping cross-language snapshot test");
        return;
    }
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let sid = create("python absent").unwrap();
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("brand_new.txt");

    run_python_snapshot(&sid, &path, "write");

    let muts = iter_mutations(&sid).unwrap();
    match &muts[0].mutation {
        Mutation::FsWrite {
            path: _,
            prev_blob: None,
        } => {}
        other => panic!("expected FsWrite{{prev_blob: None}}, got {other:?}"),
    }
}

#[test]
fn python_snapshot_no_mirror_for_ephemeral_session() {
    if !python3_available() {
        eprintln!("python3 unavailable; skipping cross-language snapshot test");
        return;
    }
    let _lock = lock_env();
    let _data = redirect_data_dir();

    // Don't create a durable session — pretend COS_SESSION is the
    // ephemeral CLI session id (`cli-1234-abcd`). Python should still
    // write the trash dir but skip the durable mirror because no
    // <sessions>/<sid>/meta.json exists.
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("legacy.txt");
    std::fs::write(&path, b"legacy bytes").unwrap();

    let lib = apps_lib_dir();
    let data_dir = env::var("COS_DATA_DIR").unwrap();
    let script = format!(
        "import sys; sys.path.insert(0, {lib:?}); import snapshot; \
         snapshot.snapshot({path:?}, 'write')",
        lib = lib.to_string_lossy(),
        path = path.to_string_lossy(),
    );
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(&script)
        .env("COS_DATA_DIR", &data_dir)
        .env("COS_SESSION", "cli-9999-abcd-not-durable")
        .output()
        .expect("python");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Trash dir got written.
    let trash = std::path::Path::new(&data_dir)
        .join("trash")
        .join("cli-9999-abcd-not-durable");
    assert!(trash.is_dir(), "trash dir created");

    // Sessions dir does NOT have a mutations.jsonl for this fake sid.
    let sessions_dir = std::path::Path::new(&data_dir)
        .join("sessions")
        .join("cli-9999-abcd-not-durable");
    assert!(
        !sessions_dir.join("mutations.jsonl").exists(),
        "no mirror written for ephemeral CLI session"
    );
}

// =====================================================================
// Phase 6 — cross-runtime handover via the shared `claw_os_session.py`
// helper. These tests are the "smoking gun" that the file schema is
// the only thing two runtimes need to agree on. We:
//   1. Open a session from Rust, append a turn from Rust.
//   2. Hand off to a Python "fake agent" via `claw_os_session.py` —
//      it lists sessions, reads the turn we wrote, appends one of
//      its own (with a different `runtime` label).
//   3. Hand back to Rust: re-read turns.jsonl, confirm both turns
//      appear in order with their original runtimes preserved.
// If this passes, an out-of-tree Python / Node / Go agent can pick up
// where ours left off without ever shelling out to `cos`.
// =====================================================================

fn run_python(script: &str) -> std::process::Output {
    let lib = sdk_lib_dir();
    assert!(
        lib.join("claw_os_session.py").is_file(),
        "claw_os_session.py missing at {}",
        lib.display()
    );
    let data_dir = env::var("COS_DATA_DIR").expect("COS_DATA_DIR set");
    let preamble = format!(
        "import sys; sys.path.insert(0, {lib:?})\n",
        lib = lib.to_string_lossy(),
    );
    std::process::Command::new("python3")
        .arg("-c")
        .arg(format!("{preamble}{script}"))
        .env("COS_DATA_DIR", &data_dir)
        .output()
        .expect("spawn python3")
}

#[test]
fn cross_runtime_python_appends_turn_rust_reads_it_back() {
    if !python3_available() {
        eprintln!("python3 unavailable; skipping cross-runtime turn handover test");
        return;
    }
    let _lock = lock_env();
    let _data = redirect_data_dir();

    // Rust: create session + write the first turn (the "system agent" run).
    let sid = create("cross-runtime").unwrap();
    let mut t1 = Turn::text(TurnRole::User, "list my reports");
    t1.runtime = Some("cos-agent".into());
    append_turn(&sid, t1).unwrap();

    // Python: open the session, read the user turn, append an
    // assistant reply tagged with a *different* runtime label.
    let script = format!(
        r#"
from claw_os_session import Session
s = Session.open({sid:?})
turns = s.turns()
assert len(turns) == 1, f"expected 1 turn, got {{turns}}"
assert turns[0]["role"] == "user"
assert turns[0]["content"] == "list my reports"
assert turns[0]["runtime"] == "cos-agent"
seq = s.append_turn("assistant", "Sure — opening reports/.", runtime="third-party-bot-py")
print("py-seq:", seq)
"#,
        sid = sid.as_str()
    );
    let out = run_python(&script);
    assert!(
        out.status.success(),
        "python helper failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("py-seq: 1"),
        "python should have been assigned seq=1; got {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // Rust: re-read both turns. Schema agreement (kebab roles, snake
    // field names, optional `runtime`) means we deserialize what
    // Python wrote with no special-casing.
    let turns = iter_turns(&sid).unwrap();
    assert_eq!(turns.len(), 2, "got {turns:?}");
    assert_eq!(turns[0].seq, 0);
    assert_eq!(turns[0].role, TurnRole::User);
    assert_eq!(turns[0].runtime.as_deref(), Some("cos-agent"));
    assert_eq!(turns[1].seq, 1);
    assert_eq!(turns[1].role, TurnRole::Assistant);
    assert_eq!(turns[1].content, "Sure — opening reports/.");
    assert_eq!(turns[1].runtime.as_deref(), Some("third-party-bot-py"));
}

#[test]
fn cross_runtime_python_records_mutation_rollback_restores_it() {
    // The session module's rollback engine doesn't care which runtime
    // wrote the FsWrite mutation — it operates on the file schema.
    // Demonstrate by having Python record the mutation and Rust
    // replay the inverse.
    if !python3_available() {
        eprintln!("python3 unavailable; skipping cross-runtime mutation handover test");
        return;
    }
    let _lock = lock_env();
    let _data = redirect_data_dir();
    let _perms = crate::test_env::PermissiveModeGuard::new();

    let sid = create("cross-runtime mutation").unwrap();

    // Set up a real workspace file so the rollback engine has bytes
    // to compare against.
    let work = tempfile::tempdir().unwrap();
    let target = work.path().join("README.md");
    let original = b"v1: original content\n";
    std::fs::write(&target, original).unwrap();

    // Pretend the third-party agent edited the file: change its
    // bytes AND record the inverse via the Python helper. The
    // helper stashes `original` as a blob and writes the FsWrite
    // mutation that points at it.
    let modified = b"v2: modified by third-party agent\n";
    std::fs::write(&target, modified).unwrap();

    let script = format!(
        r#"
from claw_os_session import Session
s = Session.open({sid:?})
seq = s.record_fs_write({path:?}, prev_bytes=b"{original}", runtime="third-party-bot-py")
print("py-mut-seq:", seq)
"#,
        sid = sid.as_str(),
        path = target.to_string_lossy(),
        original = original.iter().map(|b| format!("\\x{:02x}", b)).collect::<String>(),
    );
    let out = run_python(&script);
    assert!(
        out.status.success(),
        "python helper failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );

    // Rust: confirm the mutation is on disk with the right shape.
    let muts = iter_mutations(&sid).unwrap();
    assert_eq!(muts.len(), 1);
    assert_eq!(muts[0].seq, 0);
    assert_eq!(muts[0].runtime.as_deref(), Some("third-party-bot-py"));
    match &muts[0].mutation {
        Mutation::FsWrite { path, prev_blob } => {
            assert_eq!(path, &target);
            assert!(prev_blob.is_some(), "blob id missing");
        }
        other => panic!("expected FsWrite, got {other:?}"),
    }

    // Rust: rollback restores the original bytes.
    let outcomes = rollback(&sid).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(matches!(outcomes[0].status, RollbackStatus::Restored));
    let after = std::fs::read(&target).unwrap();
    assert_eq!(after, original, "rollback must restore exact bytes");
}

#[test]
fn cross_runtime_python_lists_only_durable_sessions() {
    // Session::list() in the Python helper has to mirror the Rust
    // list() rules: skip dotfiles, skip non-matching ids, skip dirs
    // missing meta.json. Otherwise a third-party agent could stumble
    // over half-created or archived sessions.
    if !python3_available() {
        eprintln!("python3 unavailable; skipping cross-runtime list test");
        return;
    }
    let _lock = lock_env();
    let _data = redirect_data_dir();

    let s1 = create("first").unwrap();
    let _s2 = create("second").unwrap();

    // Add foreign / corrupt entries that should be ignored:
    let root = sessions_root();
    std::fs::create_dir_all(root.join(".archive")).unwrap();
    std::fs::create_dir_all(root.join("not-a-session-id")).unwrap();
    let half = root.join("ses_0019e25600000_aaaaaaaaaaaa");
    std::fs::create_dir_all(&half).unwrap();
    // No meta.json under `half/`.

    let script = format!(
        r#"
from claw_os_session import Session
sids = sorted(s.sid for s in Session.list())
print("py-listed:", ",".join(sids))
"#
    );
    let out = run_python(&script);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(s1.as_str()),
        "first session missing from python list; got {stdout}"
    );
    assert!(
        !stdout.contains("not-a-session-id"),
        "python list leaked invalid id: {stdout}"
    );
    assert!(
        !stdout.contains(".archive"),
        "python list leaked archive dir: {stdout}"
    );
    assert!(
        !stdout.contains("ses_0019e25600000_aaaaaaaaaaaa"),
        "python list returned half-created session: {stdout}"
    );
}
