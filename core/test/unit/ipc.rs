use super::*;
use std::env;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Once,
};

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);
static INIT: Once = Once::new();

/// All tests share one COS_DATA_DIR (set once). Each test uses a unique
/// session-id prefix so there is no cross-test interference.
fn unique_session(prefix: &str) -> String {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    INIT.call_once(|| {
        let dir = env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        env::set_var("COS_DATA_DIR", &dir);
        // Tests don't set COS_SESSION; flip the caps gate to its
        // opt-in permissive mode so the gated dispatchers don't
        // deny every call.
        env::set_var("COS_PERMS_MODE", "permissive");
    });
    format!("{prefix}-{n}")
}

#[test]
fn send_creates_message_file() {
    let sid = unique_session("send-create");
    let args = vec![
        sid.clone(),
        "hello world".to_string(),
        "--from".to_string(),
        "agent-a".to_string(),
    ];
    let result = cmd_send(&args).unwrap();
    assert_eq!(result["sent"], true);
    assert_eq!(result["target"], sid.as_str());
    assert_eq!(result["message_id"], "0001");

    let dir = session_queue_dir(&sid);
    assert!(dir.join("0001.json").exists());
}

#[test]
fn send_increments_message_id() {
    let sid = unique_session("send-incr");
    let r1 = cmd_send(&vec![sid.clone(), "msg1".to_string()]).unwrap();
    assert_eq!(r1["message_id"], "0001");

    let r2 = cmd_send(&vec![sid.clone(), "msg2".to_string()]).unwrap();
    assert_eq!(r2["message_id"], "0002");
}

#[test]
fn recv_returns_oldest_and_removes() {
    let sid = unique_session("recv-oldest");
    cmd_send(&vec![sid.clone(), "first".to_string()]).unwrap();
    cmd_send(&vec![sid.clone(), "second".to_string()]).unwrap();

    let r = cmd_recv(&vec![sid.clone()]).unwrap();
    assert_eq!(r["body"], "first");
    assert_eq!(r["message_id"], "0001");

    let dir = session_queue_dir(&sid);
    assert!(!dir.join("0001.json").exists());
    assert!(dir.join("0002.json").exists());
}

#[test]
fn recv_peek_does_not_remove() {
    let sid = unique_session("recv-peek");
    cmd_send(&vec![sid.clone(), "peekme".to_string()]).unwrap();

    let r = cmd_recv(&vec![sid.clone(), "--peek".to_string()]).unwrap();
    assert_eq!(r["body"], "peekme");

    let dir = session_queue_dir(&sid);
    assert!(dir.join("0001.json").exists());
}

#[test]
fn recv_empty_queue_returns_empty() {
    let sid = unique_session("recv-empty");
    let r = cmd_recv(&vec![sid]).unwrap();
    assert_eq!(r["empty"], true);
}

#[test]
fn list_shows_all_messages() {
    let sid = unique_session("list-all");
    cmd_send(&vec![sid.clone(), "a".to_string()]).unwrap();
    cmd_send(&vec![sid.clone(), "b".to_string()]).unwrap();

    let r = cmd_list(&vec![sid.clone()]).unwrap();
    assert_eq!(r["session_id"], sid.as_str());
    assert_eq!(r["count"], 2);
    let msgs = r["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["body"], "a");
    assert_eq!(msgs[1]["body"], "b");
}

#[test]
fn clear_removes_all_messages() {
    let sid = unique_session("clear-all");
    cmd_send(&vec![sid.clone(), "x".to_string()]).unwrap();
    cmd_send(&vec![sid.clone(), "y".to_string()]).unwrap();

    let r = cmd_clear(&vec![sid.clone()]).unwrap();
    assert_eq!(r["session_id"], sid.as_str());
    assert_eq!(r["cleared"], 2);

    let r2 = cmd_list(&vec![sid]).unwrap();
    assert_eq!(r2["count"], 0);
}

#[test]
fn run_dispatches_correctly() {
    let sid = unique_session("dispatch");
    let r = run("send", &vec![sid.clone(), "hi".to_string()]).unwrap();
    assert_eq!(r["sent"], true);

    let r = run("list", &vec![sid]).unwrap();
    assert_eq!(r["count"], 1);
}

#[test]
fn run_unknown_command() {
    let r = run("bogus", &vec![]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("unknown ipc command"));
}

#[test]
fn send_missing_args_returns_error() {
    let r = cmd_send(&vec!["only-one-arg".to_string()]);
    assert!(r.is_err());
}

// -----------------------------------------------------------------------
// Lock tests
// -----------------------------------------------------------------------

/// Helper: generate a unique resource name for lock/barrier tests.
fn unique_resource(prefix: &str) -> String {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    INIT.call_once(|| {
        let dir = env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        env::set_var("COS_DATA_DIR", &dir);
        env::set_var("COS_PERMS_MODE", "permissive");
    });
    format!("{prefix}-{n}")
}

#[test]
fn lock_acquire_and_release() {
    let res = unique_resource("lock-basic");
    let r = cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-1".to_string(),
    ])
    .unwrap();
    assert_eq!(r["locked"], true);
    assert_eq!(r["status"], "acquired");
    assert_eq!(r["resource"], res.as_str());
    assert_eq!(r["holder"], "agent-1");

    // Lock file should exist.
    let lock_path = locks_dir().join(format!("{res}.lock"));
    assert!(lock_path.exists());

    // Unlock it — holder is mandatory; supply the matching one.
    let r = cmd_unlock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-1".to_string(),
    ])
    .unwrap();
    assert_eq!(r["unlocked"], true);
    assert_eq!(r["status"], "released");
    assert!(!lock_path.exists());
}

#[test]
fn lock_already_held_by_same_holder() {
    let res = unique_resource("lock-same");
    cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-x".to_string(),
    ])
    .unwrap();

    // Same holder tries again — should get already_held.
    let r = cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-x".to_string(),
    ])
    .unwrap();
    assert_eq!(r["locked"], true);
    assert_eq!(r["status"], "already_held");

    // Clean up.
    cmd_unlock(&vec![
        res,
        "--holder".to_string(),
        "agent-x".to_string(),
    ])
    .unwrap();
}

#[test]
fn lock_holder_mismatch_prevents_unlock() {
    let res = unique_resource("lock-mismatch");
    cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-owner".to_string(),
    ])
    .unwrap();

    // Another holder tries to unlock.
    let r = cmd_unlock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-intruder".to_string(),
    ])
    .unwrap();
    assert_eq!(r["unlocked"], false);
    assert_eq!(r["status"], "holder_mismatch");
    assert_eq!(r["held_by"], "agent-owner");

    // Correct holder can unlock.
    let r = cmd_unlock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-owner".to_string(),
    ])
    .unwrap();
    assert_eq!(r["unlocked"], true);
}

#[test]
fn lock_timeout_when_held_by_another() {
    let res = unique_resource("lock-timeout");
    // Lock with current PID (alive), so it won't be reclaimed as stale.
    cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-a".to_string(),
    ])
    .unwrap();

    // Another holder tries to lock with a very short timeout.
    let r = cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "agent-b".to_string(),
        "--timeout".to_string(),
        "0".to_string(),
    ])
    .unwrap();
    assert_eq!(r["locked"], false);
    assert_eq!(r["status"], "timeout");
    assert_eq!(r["held_by"], "agent-a");

    cmd_unlock(&vec![
        res,
        "--holder".to_string(),
        "agent-a".to_string(),
    ])
    .unwrap();
}

#[test]
fn lock_stale_detection_reclaims() {
    let res = unique_resource("lock-stale");
    let dir = locks_dir();
    fs::create_dir_all(&dir).unwrap();

    // Manually write a lock file with a dead PID.
    let lock_path = dir.join(format!("{res}.lock"));
    let stale = json!({
        "resource": res,
        "holder": "dead-agent",
        "pid": 999999999_u64,
        "acquired_at": "2024-01-01T00:00:00Z",
    });
    fs::write(&lock_path, serde_json::to_string_pretty(&stale).unwrap()).unwrap();

    // New agent should reclaim the stale lock.
    let r = cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "alive-agent".to_string(),
    ])
    .unwrap();
    assert_eq!(r["locked"], true);
    assert_eq!(r["status"], "acquired");
    assert_eq!(r["holder"], "alive-agent");

    cmd_unlock(&vec![
        res,
        "--holder".to_string(),
        "alive-agent".to_string(),
    ])
    .unwrap();
}

#[test]
fn unlock_not_locked_returns_not_locked() {
    let res = unique_resource("unlock-none");
    let r = cmd_unlock(&vec![res.clone()]).unwrap();
    assert_eq!(r["unlocked"], false);
    assert_eq!(r["status"], "not_locked");
}

#[test]
fn locks_lists_active() {
    let res1 = unique_resource("locks-list-a");
    let res2 = unique_resource("locks-list-b");
    cmd_lock(&vec![
        res1.clone(),
        "--holder".to_string(),
        "h1".to_string(),
    ])
    .unwrap();
    cmd_lock(&vec![
        res2.clone(),
        "--holder".to_string(),
        "h2".to_string(),
    ])
    .unwrap();

    let r = cmd_locks(&vec![]).unwrap();
    let count = r["count"].as_u64().unwrap();
    assert!(count >= 2);

    let locks = r["locks"].as_array().unwrap();
    let resources: Vec<&str> = locks
        .iter()
        .filter_map(|l| l["resource"].as_str())
        .collect();
    assert!(resources.contains(&res1.as_str()));
    assert!(resources.contains(&res2.as_str()));

    cmd_unlock(&vec![res1, "--holder".to_string(), "h1".to_string()]).unwrap();
    cmd_unlock(&vec![res2, "--holder".to_string(), "h2".to_string()]).unwrap();
}

#[test]
fn lock_missing_args_returns_error() {
    let r = cmd_lock(&vec![]);
    assert!(r.is_err());
}

#[test]
fn unlock_missing_args_returns_error() {
    let r = cmd_unlock(&vec![]);
    assert!(r.is_err());
}

/// Without a holder check on every unlock, any caller holding
/// `IPC_INVOKE` could release somebody else's lock by just
/// omitting `--holder`. Confirms that's no longer possible.
#[test]
fn unlock_requires_holder_match() {
    let res = unique_resource("unlock-holder-required");
    cmd_lock(&vec![
        res.clone(),
        "--holder".to_string(),
        "owner-agent".to_string(),
    ])
    .unwrap();

    // Attacker omits --holder. Default falls back to the caller
    // pid which can never match the owner's holder string.
    let r = cmd_unlock(&vec![res.clone()]).unwrap();
    assert_eq!(r["unlocked"], false);
    assert_eq!(r["status"], "holder_mismatch");
    assert_eq!(r["held_by"], "owner-agent");

    // Lock file must still exist.
    let lock_path = locks_dir().join(format!("{res}.lock"));
    assert!(lock_path.exists());

    // Owner can release it normally.
    let r = cmd_unlock(&vec![
        res,
        "--holder".to_string(),
        "owner-agent".to_string(),
    ])
    .unwrap();
    assert_eq!(r["unlocked"], true);
}

/// Two threads racing for the same resource MUST end with
/// exactly one acquisition; the other must time out / be denied.
/// Before the O_EXCL rewrite, both `read_locked → write_locked`
/// callers saw "no live holder" and both wrote the lock file —
/// the user-facing IPC lock primitive had no mutual exclusion.
#[test]
fn lock_is_atomic_under_concurrency() {
    use std::sync::atomic::{AtomicUsize, Ordering as AOrd};
    use std::sync::Arc;

    let res = unique_resource("lock-concurrent");
    let acquired = Arc::new(AtomicUsize::new(0));
    let denied = Arc::new(AtomicUsize::new(0));

    let mut handles = vec![];
    for i in 0..16 {
        let res = res.clone();
        let acq = acquired.clone();
        let den = denied.clone();
        let holder = format!("racer-{i}");
        handles.push(std::thread::spawn(move || {
            let r = cmd_lock(&vec![
                res,
                "--holder".to_string(),
                holder,
                "--timeout".to_string(),
                "0".to_string(),
            ])
            .unwrap();
            if r["locked"] == true && r["status"] == "acquired" {
                acq.fetch_add(1, AOrd::SeqCst);
            } else {
                den.fetch_add(1, AOrd::SeqCst);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        acquired.load(AOrd::SeqCst),
        1,
        "exactly one acquirer must win under concurrency"
    );
    assert_eq!(
        denied.load(AOrd::SeqCst),
        15,
        "every other concurrent caller must be denied"
    );

    // The winner's lock file is still in place; we don't try to
    // unlock it here because we don't know which racer won.
    let lock_path = locks_dir().join(format!("{res}.lock"));
    assert!(lock_path.exists());
    let _ = fs::remove_file(&lock_path);
}

// -----------------------------------------------------------------------
// Barrier tests
// -----------------------------------------------------------------------

#[test]
fn barrier_reached_immediately() {
    let name = unique_resource("barrier-imm");
    // Pre-seed a ready file for session-1.
    let dir = barriers_dir().join(&name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("session-1.ready"), "ready").unwrap();

    // session-2 arrives and expects 2.
    let r = cmd_barrier(&vec![
        name.clone(),
        "--expect".to_string(),
        "2".to_string(),
        "--session".to_string(),
        "session-2".to_string(),
    ])
    .unwrap();
    assert_eq!(r["status"], "reached");
    assert_eq!(r["expected"], 2);
    assert_eq!(r["ready_count"], 2);
    let sessions = r["sessions"].as_array().unwrap();
    let names: Vec<&str> = sessions.iter().filter_map(|s| s.as_str()).collect();
    assert!(names.contains(&"session-1"));
    assert!(names.contains(&"session-2"));
}

#[test]
fn barrier_timeout_when_not_enough() {
    let name = unique_resource("barrier-tmout");
    let r = cmd_barrier(&vec![
        name.clone(),
        "--expect".to_string(),
        "5".to_string(),
        "--session".to_string(),
        "only-me".to_string(),
        "--timeout".to_string(),
        "0".to_string(),
    ])
    .unwrap();
    assert_eq!(r["status"], "timeout");
    assert_eq!(r["expected"], 5);
    assert_eq!(r["ready_count"], 1);
}

#[test]
fn barrier_missing_expect_returns_error() {
    let name = unique_resource("barrier-noexpect");
    let r = cmd_barrier(&vec![name, "--session".to_string(), "s1".to_string()]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("--expect"));
}

#[test]
fn barrier_missing_session_returns_error() {
    let name = unique_resource("barrier-nosess");
    let r = cmd_barrier(&vec![name, "--expect".to_string(), "2".to_string()]);
    assert!(r.is_err());
    assert!(r.unwrap_err().contains("--session"));
}

#[test]
fn barrier_missing_name_returns_error() {
    let r = cmd_barrier(&vec![
        "--expect".to_string(),
        "2".to_string(),
        "--session".to_string(),
        "s1".to_string(),
    ]);
    assert!(r.is_err());
}

#[test]
fn run_dispatches_lock_unlock_barrier() {
    let res = unique_resource("dispatch-lock");
    let r = run(
        "lock",
        &vec![res.clone(), "--holder".to_string(), "h1".to_string()],
    )
    .unwrap();
    assert_eq!(r["locked"], true);

    let r = run("locks", &vec![]).unwrap();
    assert!(r["count"].as_u64().unwrap() >= 1);

    let r = run(
        "unlock",
        &vec![res, "--holder".to_string(), "h1".to_string()],
    )
    .unwrap();
    assert_eq!(r["unlocked"], true);
}

// -----------------------------------------------------------------------
// Pipe tests
// -----------------------------------------------------------------------

#[test]
fn pipe_create_and_list() {
    let name = unique_resource("pipe-create");
    let r = pipe_create(&vec![name.clone()]).unwrap();
    assert_eq!(r["created"], name.as_str());
    assert_eq!(r["buffer_size"], 1000);

    // Verify directory and meta.json exist.
    let channel_dir = pipe_channel_dir(&name);
    assert!(channel_dir.join("meta.json").exists());
    assert!(channel_dir.join("messages").exists());

    // Verify it appears in list.
    let r = pipe_list(&vec![]).unwrap();
    let channels = r["channels"].as_array().unwrap();
    let names: Vec<&str> = channels.iter().filter_map(|c| c["name"].as_str()).collect();
    assert!(names.contains(&name.as_str()));

    // Clean up.
    pipe_destroy(&vec![name]).unwrap();
}

#[test]
fn pipe_publish_and_subscribe() {
    let name = unique_resource("pipe-pubsub");
    pipe_create(&vec![name.clone()]).unwrap();

    let r = pipe_publish(&vec![
        name.clone(),
        "hello".to_string(),
        "--from".to_string(),
        "agent-a".to_string(),
    ])
    .unwrap();
    assert_eq!(r["published"], true);
    assert_eq!(r["channel"], name.as_str());
    assert_eq!(r["message_id"], "000001");

    pipe_publish(&vec![name.clone(), "world".to_string()]).unwrap();

    let r = pipe_subscribe(&vec![name.clone()]).unwrap();
    assert_eq!(r["channel"], name.as_str());
    assert_eq!(r["count"], 2);
    let msgs = r["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["data"], "hello");
    assert_eq!(msgs[0]["from"], "agent-a");
    assert_eq!(msgs[1]["data"], "world");
    assert_eq!(r["latest_id"], "000002");

    // Clean up.
    pipe_destroy(&vec![name]).unwrap();
}

#[test]
fn pipe_subscribe_since() {
    let name = unique_resource("pipe-since");
    pipe_create(&vec![name.clone()]).unwrap();

    pipe_publish(&vec![name.clone(), "msg1".to_string()]).unwrap();
    pipe_publish(&vec![name.clone(), "msg2".to_string()]).unwrap();
    pipe_publish(&vec![name.clone(), "msg3".to_string()]).unwrap();

    // Subscribe since 000001 → should get 000002 and 000003 only.
    let r = pipe_subscribe(&vec![
        name.clone(),
        "--since".to_string(),
        "000001".to_string(),
    ])
    .unwrap();
    assert_eq!(r["count"], 2);
    let msgs = r["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["id"], "000002");
    assert_eq!(msgs[1]["id"], "000003");

    // Clean up.
    pipe_destroy(&vec![name]).unwrap();
}

#[test]
fn pipe_backpressure() {
    let name = unique_resource("pipe-backpr");
    pipe_create(&vec![
        name.clone(),
        "--buffer-size".to_string(),
        "3".to_string(),
    ])
    .unwrap();

    // Publish 5 messages.
    for i in 1..=5 {
        pipe_publish(&vec![name.clone(), format!("msg{i}")]).unwrap();
    }

    // Only 3 should remain (the newest 3).
    let r = pipe_subscribe(&vec![name.clone()]).unwrap();
    assert_eq!(r["count"], 3);
    let msgs = r["messages"].as_array().unwrap();
    assert_eq!(msgs[0]["data"], "msg3");
    assert_eq!(msgs[1]["data"], "msg4");
    assert_eq!(msgs[2]["data"], "msg5");

    // Clean up.
    pipe_destroy(&vec![name]).unwrap();
}

#[test]
fn pipe_destroy_removes_channel() {
    let name = unique_resource("pipe-destroy");
    pipe_create(&vec![name.clone()]).unwrap();
    assert!(pipe_channel_dir(&name).exists());

    let r = pipe_destroy(&vec![name.clone()]).unwrap();
    assert_eq!(r["destroyed"], name.as_str());
    assert!(!pipe_channel_dir(&name).exists());

    // Destroy again should error.
    let r = pipe_destroy(&vec![name]);
    assert!(r.is_err());
}

#[test]
fn pipe_publish_json_data() {
    let name = unique_resource("pipe-json");
    pipe_create(&vec![name.clone()]).unwrap();

    // Publish valid JSON data — should be stored as object, not string.
    let json_data = r#"{"key":"value","num":42}"#;
    pipe_publish(&vec![name.clone(), json_data.to_string()]).unwrap();

    let r = pipe_subscribe(&vec![name.clone()]).unwrap();
    let msgs = r["messages"].as_array().unwrap();
    assert!(msgs[0]["data"].is_object());
    assert_eq!(msgs[0]["data"]["key"], "value");
    assert_eq!(msgs[0]["data"]["num"], 42);

    // Clean up.
    pipe_destroy(&vec![name]).unwrap();
}

#[test]
fn pipe_subscribe_follow_timeout() {
    let name = unique_resource("pipe-follow");
    pipe_create(&vec![name.clone()]).unwrap();

    // Subscribe with --follow and a very short timeout; no new messages → timeout.
    let r = pipe_subscribe(&vec![
        name.clone(),
        "--follow".to_string(),
        "--timeout".to_string(),
        "1".to_string(),
    ])
    .unwrap();
    assert_eq!(r["channel"], name.as_str());
    assert_eq!(r["count"], 0);
    assert_eq!(r["timeout"], true);
    assert!(r["messages"].as_array().unwrap().is_empty());

    // Clean up.
    pipe_destroy(&vec![name]).unwrap();
}
