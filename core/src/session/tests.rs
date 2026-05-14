//! End-to-end tests for the durable session module. Unit tests for
//! each sub-module live alongside their code; this file covers the
//! interactions (create + write + read + list back) plus the messier
//! crash / concurrency edge cases.

use std::env;
use std::sync::Mutex;

use serde_json::json;
use tempfile::TempDir;

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::*;

// All tests in this file mutate the global `COS_DATA_DIR` env var,
// so we serialize them on a single mutex. Per-test tempdirs keep
// state isolated within the serialized window. We recover from a
// poisoned mutex (one test panicked while holding it) so a single
// failure doesn't cascade into N "PoisonError" failures that obscure
// the real cause.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
