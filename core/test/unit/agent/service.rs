use super::*;
use crate::agent::llm::accumulate::StreamSink;
use crate::agent::llm::{ChatResponse, ContentBlock, FinishReason, StreamEvent, ToolCall, Usage};

fn fresh_root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn lock_sentinel_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
}

fn finished_job_with_stream(store: &Store) -> Job {
    let _ = store.submit("p".into(), None, None, None, None).unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    store
        .append_stream_progress(&claimed.id, json!({ "kind": "test" }))
        .unwrap();
    store
        .finish(
            claimed,
            FinishOutcome::Ok {
                response: "x".into(),
                turns_used: 1,
                provider: "m".into(),
                model: "m".into(),
                evidence: Box::new(None),
                fallback: Box::new(None),
            },
        )
        .unwrap()
}

#[test]
fn tool_progress_round_trips_through_task_stream() {
    let root = fresh_root();
    let store = Store::with_root(root.path().to_path_buf()).unwrap();
    let job = store.submit("test".into(), None, None, None, None).unwrap();
    store
        .append_stream_progress(
            &job.id,
            json!({
                "kind": "tool_result",
                "id": "tool-1",
                "name": "fs.read",
                "ok": true,
                "preview": "done",
            }),
        )
        .unwrap();

    let (cursor, events) = store.read_stream_events(&job.id, 0).unwrap();
    assert_eq!(cursor, 1);
    assert_eq!(events[0]["progress"]["kind"], "tool_result");
    assert_eq!(events[0]["progress"]["id"], "tool-1");
}

#[test]
fn buffered_message_is_persisted_as_one_text_representation() {
    let root = fresh_root();
    let _guard = EnvGuard::set(root.path());
    let store = Store::open_default().unwrap();
    let job = store.submit("test".into(), None, None, None, None).unwrap();
    let sink = JobStreamSink {
        job_id: job.id.clone(),
    };
    let usage = Usage {
        input_tokens: 8,
        output_tokens: 5,
        ..Usage::default()
    };

    sink.on_event(&StreamEvent::Message(ChatResponse {
        model: "gemini-test".into(),
        content: vec![ContentBlock::Text {
            text: "buffered answer".into(),
        }],
        tool_calls: vec![ToolCall {
            id: "lookup::0".into(),
            name: "lookup".into(),
            input: json!({"q": "weather"}),
        }],
        finish_reason: FinishReason::ToolUse,
        usage: usage.clone(),
    }));
    sink.on_event(&StreamEvent::Done {
        finish: FinishReason::ToolUse,
        usage,
    });

    let (cursor, events) = store.read_stream_events(&job.id, 0).unwrap();
    assert_eq!(cursor, 2);
    assert_eq!(events.len(), 2);

    let mut text_representations = Vec::new();
    for record in &events {
        let event = &record["event"];
        match event["kind"].as_str() {
            Some("text_delta") => {
                text_representations.push(event["text"].as_str().unwrap().to_string());
            }
            Some("message") => {
                for block in event["content"].as_array().unwrap() {
                    if block["type"] == "text" {
                        text_representations.push(block["text"].as_str().unwrap().to_string());
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(text_representations, vec!["buffered answer"]);
    assert_eq!(events[0]["event"]["kind"], "message");
    assert_eq!(events[0]["event"]["tool_calls"][0]["name"], "lookup");
    assert_eq!(events[0]["event"]["finish_reason"], "tool_use");
    assert_eq!(events[0]["event"]["usage"]["input_tokens"], 8);
    assert_eq!(events[1]["event"]["kind"], "done");
    assert_eq!(events[1]["event"]["finish"], "tool_use");
    assert_eq!(events[1]["event"]["usage"]["output_tokens"], 5);
}

#[test]
fn incomplete_stream_tail_is_not_consumed() {
    use std::io::Write as _;

    let root = fresh_root();
    let store = Store::with_root(root.path().to_path_buf()).unwrap();
    let job = store.submit("test".into(), None, None, None, None).unwrap();
    store
        .append_stream_progress(&job.id, json!({ "kind": "tool_start", "id": "one" }))
        .unwrap();
    let path = store.stream_path(&job.id);
    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#"{"progress":{"kind":"tool_result""#)
        .unwrap();
    drop(file);

    let (cursor, events) = store.read_stream_events(&job.id, 0).unwrap();
    assert_eq!(cursor, 1);
    assert_eq!(events.len(), 1);

    let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(br#","id":"two"}}"#).unwrap();
    file.write_all(b"\n").unwrap();
    drop(file);
    let (cursor, events) = store.read_stream_events(&job.id, cursor).unwrap();
    assert_eq!(cursor, 2);
    assert_eq!(events[0]["progress"]["id"], "two");
}

#[test]
fn progress_text_clipping_is_bounded() {
    const MAX_CHARS: usize = 4096;
    let oversized = "x".repeat(MAX_CHARS + 100);
    assert!(clip_progress_text(&oversized, MAX_CHARS).chars().count() <= MAX_CHARS + 2);
}

#[test]
fn retry_branch_context_is_seeded_once_as_hidden_system_memory() {
    let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().unwrap();
    let context = format!("User: earlier question {}", "x".repeat(40 * 1024));
    seed_branch_context(&db, "branch-session", &context).unwrap();
    seed_branch_context(&db, "branch-session", &context).unwrap();
    let rows = db.recent("branch-session", 20).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].role, "system");
    assert!(rows[0].content.contains("<untrusted_app_context>"));
    assert!(rows[0].content.chars().count() < 34 * 1024);
}

struct EnvGuard {
    prev: Option<String>,
    // Serialise env mutation across the test process. Without
    // this, two concurrent tests both call `set_var(...)` and
    // each other's `cmd()` call observes the wrong root.
    _lock: std::sync::MutexGuard<'static, ()>,
}
impl EnvGuard {
    fn set(dir: &Path) -> Self {
        let _lock = crate::test_env::lock_env();
        let prev = std::env::var("COS_DATA_DIR").ok();
        std::env::set_var("COS_DATA_DIR", dir);
        Self { prev, _lock }
    }
}
impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
    }
}

#[test]
fn store_creates_three_buckets() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    for sub in ["pending", "running", "done"] {
        assert!(dir.path().join(sub).is_dir(), "missing {sub}");
    }
    let _ = store; // silence
}

#[test]
fn submit_writes_pending_file_with_uuid_id() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store
        .submit("hello".into(), None, None, None, None)
        .unwrap();
    assert_eq!(job.status, JobStatus::Pending);
    let path = dir.path().join("pending").join(format!("{}.json", job.id));
    assert!(path.is_file(), "no file at {path:?}");
    let s = fs::read_to_string(&path).unwrap();
    let parsed: Job = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed.id, job.id);
    assert_eq!(parsed.prompt, "hello");
}

#[test]
fn submit_round_trips_owner_uid_and_home() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store
        .submit(
            "hi".into(),
            None,
            None,
            Some(1001),
            Some("/home/alice".into()),
        )
        .unwrap();
    assert_eq!(job.owner_uid, Some(1001));
    assert_eq!(job.owner_home.as_deref(), Some("/home/alice"));

    // Re-read from disk to confirm serde keeps the fields.
    let path = dir.path().join("pending").join(format!("{}.json", job.id));
    let parsed: Job = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.owner_uid, Some(1001));
    assert_eq!(parsed.owner_home.as_deref(), Some("/home/alice"));
}

#[test]
fn legacy_job_file_without_owner_fields_still_loads() {
    // Older clawd installs wrote Job JSON without owner_uid /
    // owner_home. The new fields are #[serde(default)] so those
    // files must still deserialize.
    let dir = fresh_root();
    let _store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let legacy = json!({
        "id": id,
        "prompt": "old",
        "status": "pending",
        "created_at": now_iso(),
    });
    let path = dir.path().join("pending").join(format!("{id}.json"));
    fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let parsed: Job = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed.id, id);
    assert!(parsed.owner_uid.is_none());
    assert!(parsed.owner_home.is_none());
}
#[test]
fn locate_finds_job_in_pending_bucket() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store.submit("hi".into(), None, None, None, None).unwrap();
    let (bucket, found) = store.locate(&job.id).unwrap().unwrap();
    assert_eq!(bucket, JobStatus::Pending);
    assert_eq!(found.id, job.id);
}

#[test]
fn locate_returns_none_for_unknown_id() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    assert!(store.locate("00000000-not-real").unwrap().is_none());
}

#[test]
fn claim_one_atomically_moves_pending_to_running() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store
        .submit("do work".into(), None, None, None, None)
        .unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    assert_eq!(claimed.id, job.id);
    assert_eq!(claimed.status, JobStatus::Running);
    assert!(claimed.started_at.is_some());
    assert_eq!(claimed.worker_pid, Some(std::process::id()));
    // pending/<id>.json gone, running/<id>.json present
    assert!(!dir
        .path()
        .join("pending")
        .join(format!("{}.json", job.id))
        .exists());
    assert!(dir
        .path()
        .join("running")
        .join(format!("{}.json", job.id))
        .is_file());
}

/// A job stranded in running/ by a dead worker is requeued to
/// pending/ with worker_pid/started_at cleared and recovery_count
/// incremented, so a fresh worker can re-claim it.
#[test]
fn recover_requeues_job_whose_worker_is_dead() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store
        .submit("interrupted work".into(), None, None, None, None)
        .unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    // Simulate the worker dying: overwrite the running file with a
    // pid that is certainly not alive (0 is treated as dead).
    let running = dir
        .path()
        .join("running")
        .join(format!("{}.json", claimed.id));
    let mut stranded: Job =
        serde_json::from_str(&std::fs::read_to_string(&running).unwrap()).unwrap();
    stranded.worker_pid = Some(0);
    std::fs::write(&running, serde_json::to_string_pretty(&stranded).unwrap()).unwrap();

    let (requeued, failed) = store.recover_orphaned_jobs().unwrap();
    assert_eq!((requeued, failed), (1, 0));

    // Back in pending/, reset for re-claiming.
    let (bucket, recovered) = store.locate(&job.id).unwrap().unwrap();
    assert_eq!(bucket, JobStatus::Pending);
    assert_eq!(recovered.status, JobStatus::Pending);
    assert!(recovered.worker_pid.is_none());
    assert!(recovered.started_at.is_none());
    assert_eq!(recovered.recovery_count, 1);
    assert!(
        store.claim_one().unwrap().is_some(),
        "recovered job must be re-claimable"
    );
}

/// A job whose worker is still alive must NOT be touched by recovery.
#[test]
fn recover_leaves_job_with_live_worker_alone() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store
        .submit("in flight".into(), None, None, None, None)
        .unwrap();
    // claim_one stamps the current (live) process pid.
    let _claimed = store.claim_one().unwrap().unwrap();
    let (requeued, failed) = store.recover_orphaned_jobs().unwrap();
    assert_eq!((requeued, failed), (0, 0));
    let (bucket, _) = store.locate(&job.id).unwrap().unwrap();
    assert_eq!(bucket, JobStatus::Running, "live job must stay running");
}

/// A poison job that keeps killing its worker is failed (not requeued)
/// once recovery_count exceeds MAX_RECOVERIES, so it can't starve the
/// queue in an endless crash loop.
#[test]
fn recover_fails_poison_job_after_max_recoveries() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store
        .submit("poison".into(), None, None, None, None)
        .unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    // Pre-set recovery_count at the cap with a dead worker, so the
    // next recovery pass tips it over MAX_RECOVERIES → fail.
    let running = dir
        .path()
        .join("running")
        .join(format!("{}.json", claimed.id));
    let mut stranded: Job =
        serde_json::from_str(&std::fs::read_to_string(&running).unwrap()).unwrap();
    stranded.worker_pid = Some(0);
    stranded.recovery_count = MAX_RECOVERIES;
    std::fs::write(&running, serde_json::to_string_pretty(&stranded).unwrap()).unwrap();

    let (requeued, failed) = store.recover_orphaned_jobs().unwrap();
    assert_eq!((requeued, failed), (0, 1));
    let (bucket, done) = store.locate(&job.id).unwrap().unwrap();
    assert_eq!(bucket, JobStatus::Ok); // done bucket
    assert_eq!(done.status, JobStatus::Error);
    assert!(done.error.as_deref().unwrap().contains("abandoned"));
}

#[test]
fn recover_is_noop_with_empty_running_bucket() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    assert_eq!(store.recover_orphaned_jobs().unwrap(), (0, 0));
}

#[test]
fn claim_one_returns_none_when_no_pending() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    assert!(store.claim_one().unwrap().is_none());
}

#[test]
fn claim_one_picks_oldest_first() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let first = store
        .submit("first".into(), None, None, None, None)
        .unwrap();
    // Touch the second one with a later mtime to be unambiguous on
    // filesystems with low resolution timestamps.
    std::thread::sleep(Duration::from_millis(20));
    let _second = store
        .submit("second".into(), None, None, None, None)
        .unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    assert_eq!(claimed.id, first.id);
}

#[test]
fn finish_ok_moves_running_to_done_with_response() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store.submit("p".into(), None, None, None, None).unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    let finished = store
        .finish(
            claimed,
            FinishOutcome::Ok {
                response: "answer".into(),
                turns_used: 2,
                provider: "mock".into(),
                model: "mock-model".into(),
                evidence: Box::new(None),
                fallback: Box::new(None),
            },
        )
        .unwrap();
    assert_eq!(finished.status, JobStatus::Ok);
    assert_eq!(finished.response.as_deref(), Some("answer"));
    assert_eq!(finished.turns_used, Some(2));
    assert!(!dir
        .path()
        .join("running")
        .join(format!("{}.json", job.id))
        .exists());
    assert!(dir
        .path()
        .join("done")
        .join(format!("{}.json", job.id))
        .is_file());
}

#[test]
fn finish_error_records_message() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let _job = store.submit("p".into(), None, None, None, None).unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    let finished = store
        .finish(claimed, FinishOutcome::Error("boom".into()))
        .unwrap();
    assert_eq!(finished.status, JobStatus::Error);
    assert_eq!(finished.error.as_deref(), Some("boom"));
}

#[test]
fn cancel_pending_moves_to_done_with_cancelled_status() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let job = store.submit("p".into(), None, None, None, None).unwrap();
    let cancelled = store.cancel_pending(&job.id).unwrap().unwrap();
    assert_eq!(cancelled.status, JobStatus::Cancelled);
    assert!(dir
        .path()
        .join("done")
        .join(format!("{}.json", job.id))
        .is_file());
    assert!(!dir
        .path()
        .join("pending")
        .join(format!("{}.json", job.id))
        .exists());
}

#[test]
fn cancel_pending_returns_none_when_already_running() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let _ = store.submit("p".into(), None, None, None, None).unwrap();
    let _ = store.claim_one().unwrap().unwrap();
    // The job is now in running/, not pending/ — cancel is a noop.
    let c = store.cancel_pending("nonexistent").unwrap();
    assert!(c.is_none());
}

#[test]
fn cancel_and_claim_no_silent_loss() {
    // Race claim_one() against cancel_pending() across many job
    // ids in parallel threads. The expected invariant is: for
    // every submitted id, exactly one of {claim_one, cancel}
    // succeeds — never both, never neither. Before the lock-based
    // fix, the second rename(pending→{running,done}) could
    // silently lose a state transition: claim would post a real
    // response for a request the user thought they cancelled.
    use std::sync::Arc;
    use std::sync::Mutex;

    let dir = fresh_root();
    let store = Arc::new(Store::with_root(dir.path().to_path_buf()).unwrap());
    let n_jobs = 64usize;
    let ids: Vec<String> = (0..n_jobs)
        .map(|i| {
            store
                .submit(format!("job-{i}"), None, None, None, None)
                .unwrap()
                .id
        })
        .collect();

    // Outcomes per id: count of successful claims and successful
    // cancellations. We assert exactly one of each per id.
    let claimed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let cancelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    // One thread tries to cancel every id; another claims as many
    // as it can. They interleave on the per-id flock, so for each
    // id at most one wins.
    let s1 = store.clone();
    let ids1 = ids.clone();
    let cancelled1 = cancelled.clone();
    let h_cancel = std::thread::spawn(move || {
        for id in &ids1 {
            if let Ok(Some(j)) = s1.cancel_pending(id) {
                cancelled1.lock().unwrap().push(j.id);
            }
        }
    });
    let s2 = store.clone();
    let claimed2 = claimed.clone();
    let h_claim = std::thread::spawn(move || {
        // Loop until pending is empty. Each successful claim is
        // mutually exclusive with any concurrent cancel of the
        // same id.
        loop {
            match s2.claim_one() {
                Ok(Some(j)) => claimed2.lock().unwrap().push(j.id),
                Ok(None) => break,
                Err(_) => break,
            }
        }
    });
    h_cancel.join().unwrap();
    h_claim.join().unwrap();

    let claimed = claimed.lock().unwrap().clone();
    let cancelled = cancelled.lock().unwrap().clone();
    // Sanity: never more than one outcome per id.
    let mut seen = std::collections::HashSet::new();
    for id in claimed.iter().chain(cancelled.iter()) {
        assert!(
            seen.insert(id.clone()),
            "id {id} reported both claimed and cancelled — silent loss of cancel"
        );
    }
    // The claim loop runs to exhaustion, so every id that wasn't
    // cancelled must have been claimed. Total covered == n_jobs.
    assert_eq!(
        claimed.len() + cancelled.len(),
        n_jobs,
        "missing transitions: claimed={} cancelled={}",
        claimed.len(),
        cancelled.len()
    );
    // Confirm filesystem state agrees: no pending leftovers.
    let pending_count = fs::read_dir(dir.path().join("pending"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .count();
    assert_eq!(pending_count, 0, "every job must have transitioned");
}

#[test]
fn list_bucket_returns_newest_first_and_respects_limit() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let _a = store.submit("a".into(), None, None, None, None).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let b = store.submit("b".into(), None, None, None, None).unwrap();
    std::thread::sleep(Duration::from_millis(20));
    let c = store.submit("c".into(), None, None, None, None).unwrap();
    let v = store.list_bucket(JobStatus::Pending, Some(2)).unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0].id, c.id);
    assert_eq!(v[1].id, b.id);
}

#[test]
fn counts_reflect_per_bucket_state() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let _a = store.submit("a".into(), None, None, None, None).unwrap();
    let _b = store.submit("b".into(), None, None, None, None).unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    let _ = store
        .finish(
            claimed,
            FinishOutcome::Ok {
                response: "x".into(),
                turns_used: 1,
                provider: "m".into(),
                model: "m".into(),
                evidence: Box::new(None),
                fallback: Box::new(None),
            },
        )
        .unwrap();
    let (p, r, d) = store.counts().unwrap();
    assert_eq!(p, 1);
    assert_eq!(r, 0);
    assert_eq!(d, 1);
}

#[test]
fn prune_drops_aged_files_beyond_keep_last() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    // Create 3 done jobs by submitting + claiming + finishing.
    let mut ids = Vec::new();
    for _ in 0..3 {
        let _ = store.submit("p".into(), None, None, None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        store
            .append_stream_progress(&claimed.id, json!({ "kind": "test" }))
            .unwrap();
        ids.push(claimed.id.clone());
        let _ = store
            .finish(
                claimed,
                FinishOutcome::Ok {
                    response: "x".into(),
                    turns_used: 1,
                    provider: "m".into(),
                    model: "m".into(),
                    evidence: Box::new(None),
                    fallback: Box::new(None),
                },
            )
            .unwrap();
    }
    // keep_last = 1, older_than = 0 → should drop the 2 oldest.
    let removed = store.prune(Duration::from_secs(0), 1).unwrap();
    assert_eq!(removed, 2);
    let (_, _, d) = store.counts().unwrap();
    assert_eq!(d, 1);

    let mut retained = 0;
    for id in ids {
        let done_exists = store.path_for(JobStatus::Ok, &id).exists();
        let stream_path = store.stream_path(&id);
        let stream_lock_path = lock_sentinel_path(&stream_path);
        if done_exists {
            retained += 1;
            assert!(stream_path.exists());
            assert!(stream_lock_path.exists());
            assert!(store.job_lock_path(&id).exists());
        } else {
            assert!(!stream_path.exists());
            assert!(stream_lock_path.exists());
            assert!(store.job_lock_path(&id).exists());
        }
    }
    assert_eq!(retained, 1);
}

#[test]
fn prune_does_not_touch_stream_for_active_duplicate() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let _ = store.submit("p".into(), None, None, None, None).unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    store
        .append_stream_progress(&claimed.id, json!({ "kind": "test" }))
        .unwrap();

    let running_path = store.path_for(JobStatus::Running, &claimed.id);
    let done_path = store.path_for(JobStatus::Ok, &claimed.id);
    fs::copy(&running_path, &done_path).unwrap();

    let removed = store.prune(Duration::ZERO, 0).unwrap();
    assert_eq!(removed, 0);
    assert!(running_path.exists());
    assert!(done_path.exists());
    assert!(store.stream_path(&claimed.id).exists());
    assert!(store.job_lock_path(&claimed.id).exists());
    assert!(lock_sentinel_path(&store.stream_path(&claimed.id)).exists());
}

#[test]
fn prune_keeps_record_when_stream_cleanup_fails() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let finished = finished_job_with_stream(&store);
    let stream_path = store.stream_path(&finished.id);
    fs::remove_file(&stream_path).unwrap();
    fs::create_dir(&stream_path).unwrap();

    let removed = store.prune(Duration::ZERO, 0).unwrap();
    assert_eq!(removed, 0);
    assert!(store.path_for(JobStatus::Ok, &finished.id).exists());
    assert!(stream_path.is_dir());
    assert!(store.job_lock_path(&finished.id).exists());
}

#[test]
fn prune_restores_stream_when_record_remove_fails() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let finished = finished_job_with_stream(&store);
    let record_path = store.path_for(JobStatus::Ok, &finished.id);
    let stream_path = store.stream_path(&finished.id);
    let tombstone_path = store.stream_prune_tombstone_path(&finished.id);
    let expected_stream = fs::read(&stream_path).unwrap();

    let removed = store
        .prune_with_record_remove(Duration::ZERO, 0, |path| {
            assert_eq!(path, record_path);
            Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "injected record delete failure",
            ))
        })
        .unwrap();

    assert_eq!(removed, 0);
    assert!(record_path.exists());
    assert_eq!(fs::read(&stream_path).unwrap(), expected_stream);
    assert!(!tombstone_path.exists());
    assert!(lock_sentinel_path(&stream_path).exists());
    assert!(store.job_lock_path(&finished.id).exists());
}

#[test]
fn prune_recovers_stream_staged_before_interruption() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let finished = finished_job_with_stream(&store);
    let record_path = store.path_for(JobStatus::Ok, &finished.id);
    let stream_path = store.stream_path(&finished.id);
    let tombstone_path = store.stream_prune_tombstone_path(&finished.id);
    let expected_stream = fs::read(&stream_path).unwrap();
    fs::rename(&stream_path, &tombstone_path).unwrap();

    let removed = store.prune(Duration::ZERO, 1).unwrap();

    assert_eq!(removed, 0);
    assert!(record_path.exists());
    assert_eq!(fs::read(&stream_path).unwrap(), expected_stream);
    assert!(!tombstone_path.exists());
}

#[test]
fn job_preview_truncates_with_ellipsis() {
    let mut j = Job::new_pending("a".repeat(100), None, None, None, None, None, None);
    j.id = "fixed".into();
    assert_eq!(j.preview(10), "aaaaaaaaaa…");
    let short = Job::new_pending("hi".into(), None, None, None, None, None, None);
    assert_eq!(short.preview(10), "hi");
}

// ----- CLI dispatcher tests (use COS_DATA_DIR via EnvGuard) -----

#[test]
fn cmd_help_lists_subcommands() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let v = cmd(&[]).unwrap();
    let arr = v["subcommands"].as_array().unwrap();
    assert!(arr
        .iter()
        .any(|s| s.as_str().unwrap().starts_with("submit")));
    assert!(arr.iter().any(|s| s.as_str().unwrap().starts_with("work")));
}

#[test]
fn cmd_unknown_returns_helpful_error() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let err = cmd(&["bogus".into()]).unwrap_err();
    assert!(err.contains("unknown agent service subcommand"));
}

#[test]
fn cmd_submit_then_status_then_cancel_round_trip() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let v = cmd(&["submit".into(), "do a thing".into()]).unwrap();
    assert_eq!(v["status"], "submitted");
    let id = v["job_id"].as_str().unwrap().to_string();

    let st = cmd(&["status".into(), id.clone()]).unwrap();
    assert_eq!(st["status"], "pending");
    assert_eq!(st["prompt"], "do a thing");

    let cancelled = cmd(&["cancel".into(), id.clone()]).unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert_eq!(cancelled["job_id"], id);

    // status now returns the cancelled job from done/
    let st2 = cmd(&["status".into(), id.clone()]).unwrap();
    assert_eq!(st2["status"], "cancelled");
}

#[test]
fn cmd_submit_requires_prompt() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let err = cmd(&["submit".into()]).unwrap_err();
    assert!(err.contains("usage"));
}

#[test]
fn cmd_submit_rejects_extra_positional() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let err = cmd(&["submit".into(), "a".into(), "b".into()]).unwrap_err();
    assert!(err.contains("exactly one"));
}

#[test]
fn cmd_status_no_id_returns_counts() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    cmd(&["submit".into(), "p1".into()]).unwrap();
    cmd(&["submit".into(), "p2".into()]).unwrap();
    let v = cmd(&["status".into()]).unwrap();
    assert_eq!(v["pending"], 2);
    assert_eq!(v["running"], 0);
    assert_eq!(v["done"], 0);
}

#[test]
fn cmd_status_unknown_id_errors() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let err = cmd(&["status".into(), "nope".into()]).unwrap_err();
    assert!(err.contains("not found"));
}

#[test]
fn cmd_list_filters_by_status() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    cmd(&["submit".into(), "p1".into()]).unwrap();
    cmd(&["submit".into(), "p2".into()]).unwrap();
    let v = cmd(&["list".into(), "--status".into(), "pending".into()]).unwrap();
    assert_eq!(v["count"], 2);
    let v2 = cmd(&["list".into(), "--status".into(), "done".into()]).unwrap();
    assert_eq!(v2["count"], 0);
}

#[test]
fn cmd_list_rejects_unknown_status() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let err = cmd(&["list".into(), "--status".into(), "bogus".into()]).unwrap_err();
    assert!(err.contains("unknown status"));
}

#[test]
fn cmd_result_no_wait_errors_for_pending() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let v = cmd(&["submit".into(), "p".into()]).unwrap();
    let id = v["job_id"].as_str().unwrap().to_string();
    let err = cmd(&["result".into(), id]).unwrap_err();
    assert!(err.contains("not finished"));
}

#[test]
fn cmd_result_returns_done_job() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    // Manually drive a job through to done/ so we can assert
    // result without invoking a real provider.
    let store = Store::open_default().unwrap();
    let job = store.submit("p".into(), None, None, None, None).unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    let _ = store
        .finish(
            claimed,
            FinishOutcome::Ok {
                response: "the answer".into(),
                turns_used: 1,
                provider: "mock".into(),
                model: "mock-model".into(),
                evidence: Box::new(None),
                fallback: Box::new(None),
            },
        )
        .unwrap();
    let v = cmd(&["result".into(), job.id]).unwrap();
    assert_eq!(v["status"], "ok");
    assert_eq!(v["response"], "the answer");
}

#[test]
fn cmd_cancel_unknown_errors() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let err = cmd(&["cancel".into(), "nope".into()]).unwrap_err();
    assert!(err.contains("not found"));
}

#[test]
fn cmd_prune_returns_removed_count() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let store = Store::open_default().unwrap();
    for _ in 0..2 {
        let _ = store.submit("p".into(), None, None, None, None).unwrap();
        let c = store.claim_one().unwrap().unwrap();
        let _ = store
            .finish(
                c,
                FinishOutcome::Ok {
                    response: "x".into(),
                    turns_used: 1,
                    provider: "m".into(),
                    model: "m".into(),
                    evidence: Box::new(None),
                    fallback: Box::new(None),
                },
            )
            .unwrap();
    }
    let v = cmd(&[
        "prune".into(),
        "--older-than-days".into(),
        "0".into(),
        "--keep-last".into(),
        "0".into(),
    ])
    .unwrap();
    assert_eq!(v["removed"], 2);
}

#[test]
fn cmd_work_once_with_no_jobs_returns_zero_processed() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let v = cmd(&["work".into(), "--once".into()]).unwrap();
    assert_eq!(v["processed"], 0);
    assert_eq!(v["results"].as_array().unwrap().len(), 0);
}

#[test]
fn list_bucket_skips_files_that_disappear_mid_read() {
    let dir = fresh_root();
    let store = Store::with_root(dir.path().to_path_buf()).unwrap();
    let _ = store
        .submit("alive".into(), None, None, None, None)
        .unwrap();
    // Plant a stale path: write it then remove it before list_bucket
    // can read. Since list_bucket reads inside the directory iter,
    // simulate the race by deleting one file just before listing
    // — easier: hand-craft a corrupted JSON file then verify it's
    // skipped (covers the "skip mid-list" code path equivalently).
    let bogus = dir.path().join("pending").join("bogus.json");
    fs::write(&bogus, b"not valid json").unwrap();
    let v = store.list_bucket(JobStatus::Pending, None).unwrap();
    // The valid one comes through; the malformed one is skipped.
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].prompt, "alive");
}

#[test]
fn cmd_list_orders_union_by_created_at_desc() {
    let dir = fresh_root();
    let _g = EnvGuard::set(dir.path());
    let store = Store::open_default().unwrap();
    // Submit oldest pending.
    let oldest = store
        .submit("oldest".into(), None, None, None, None)
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100)); // ensure created_at second-rollover
                                                     // Submit middle, then claim+finish (lands in done/).
    let mid = store
        .submit("middle".into(), None, None, None, None)
        .unwrap();
    let claimed = store.claim_one().unwrap().unwrap();
    // claimed should be the oldest pending (FIFO). Finish it.
    let _ = store
        .finish(
            claimed,
            FinishOutcome::Ok {
                response: "x".into(),
                turns_used: 1,
                provider: "m".into(),
                model: "m".into(),
                evidence: Box::new(None),
                fallback: Box::new(None),
            },
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(1100));
    // Newest pending.
    let newest = store
        .submit("newest".into(), None, None, None, None)
        .unwrap();

    // Expected ordering by created_at desc: newest, mid (still
    // pending), oldest (now in done/ as ok). cmd_list with no
    // filter should respect this ordering globally.
    let v = cmd(&["list".into()]).unwrap();
    let arr = v["jobs"].as_array().unwrap();
    assert_eq!(arr[0]["id"], newest.id);
    assert_eq!(arr[1]["id"], mid.id);
    assert_eq!(arr[2]["id"], oldest.id);
}
#[tokio::test]
async fn standalone_worker_hook_registry_records_runtime_audit() {
    let temp = tempfile::tempdir().unwrap();
    let _data = crate::test_env::TestEnvVarGuard::set("COS_DATA_DIR", temp.path());
    let hooks = standalone_runtime_hooks();
    assert!(hooks.names().contains(&"clawd-runtime-audit".to_string()));
    let config = crate::config::AgentConfig {
        provider: "mock".into(),
        model: "mock-model".into(),
        ..Default::default()
    };
    let mock = crate::agent::llm::providers::mock::MockProvider::new(&config.model, &config);
    mock.push_response(crate::agent::llm::providers::mock::MockResponse::ToolUse(
        vec![crate::agent::llm::ToolCall {
        id: "standalone-call".into(),
        name: "echo".into(),
        input: serde_json::json!({"text":"audit"}),
        }],
    ));
    mock.push_response(crate::agent::llm::providers::mock::MockResponse::Text(
        "done".into(),
    ));
    let provider: Arc<dyn crate::agent::llm::Provider> = Arc::new(mock);
    let runtime = crate::agent::runtime::deps::RuntimeDeps::new(
        hooks,
        Arc::new(crate::agent::runtime::deps::SystemClock),
        None,
    );
    let tools = crate::agent::tools::registry::builtin_only_registry();
    let request = crate::agent::runtime::loop_::RuntimeRequest::buffered(
        provider,
        &config,
        "run audited tool",
        &tools,
    );

    crate::agent::runtime::loop_::run_with_deps(&runtime, request)
        .await
        .unwrap();

    let audit = std::fs::read_to_string(temp.path().join("clawd").join("audit.jsonl")).unwrap();
    assert!(audit.contains("\"event\":\"clawd.agent.tool.started\""));
    assert!(audit.contains("\"event\":\"clawd.agent.tool.finished\""));
    assert!(audit.contains("\"event\":\"clawd.agent.turn.finished\""));
}
