/// Inter-process communication via file-based message queues.
///
/// Messages are stored as JSON files in per-session directories
/// under `$COS_DATA_DIR/ipc/<session-id>/`. Each message file is
/// named with a zero-padded counter (e.g. `0001.json`). Stateless
/// design — no daemon required; every invocation reads/writes the
/// filesystem directly.
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::policy::{self, OpType};

fn ipc_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("ipc")
}

fn session_queue_dir(session_id: &str) -> PathBuf {
    ipc_dir().join(session_id)
}

/// Return the next message ID for a session queue directory.
/// Scans existing `NNNN.json` files and returns one higher than the max.
fn next_message_id(dir: &PathBuf) -> String {
    let max = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".json")
                .and_then(|n| n.parse::<u32>().ok())
        })
        .max()
        .unwrap_or(0);
    format!("{:04}", max + 1)
}

/// List message files in a queue directory, sorted by name (oldest first).
fn sorted_messages(dir: &PathBuf) -> Vec<(String, PathBuf)> {
    let mut entries: Vec<(String, PathBuf)> = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") {
                let id = name
                    .strip_suffix(".json")
                    .expect("already checked ends_with .json")
                    .to_string();
                Some((id, e.path()))
            } else {
                None
            }
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "send" => cmd_send(args),
        "recv" => cmd_recv(args),
        "list" => cmd_list(args),
        "clear" => cmd_clear(args),
        "lock" => cmd_lock(args),
        "unlock" => cmd_unlock(args),
        "locks" => cmd_locks(args),
        "barrier" => cmd_barrier(args),
        _ => Err(format!("unknown ipc command: {command}")),
    }
}

fn cmd_send(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;
    let mut from: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--from" if i + 1 < args.len() => {
                from = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }

    if positional.len() < 2 {
        return Err("usage: cos ipc send <target-session-id> <message> [--from <id>]".into());
    }

    let target = &positional[0];
    let body = &positional[1];
    let sender = from.unwrap_or_default();
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let dir = session_queue_dir(target);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create queue dir: {e}"))?;

    let message_id = next_message_id(&dir);
    let msg = json!({
        "from": sender,
        "body": body,
        "timestamp": timestamp,
    });

    let path = dir.join(format!("{message_id}.json"));
    let data = serde_json::to_string_pretty(&msg)
        .map_err(|e| format!("failed to serialize message: {e}"))?;
    fs::write(&path, data).map_err(|e| format!("failed to write message: {e}"))?;

    Ok(json!({
        "sent": true,
        "target": target,
        "message_id": message_id,
    }))
}

fn cmd_recv(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let session_id = args
        .first()
        .ok_or("usage: cos ipc recv <session-id> [--timeout N] [--peek]")?;
    let mut timeout_secs: u64 = 0;
    let mut peek = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| "timeout must be a non-negative integer".to_string())?;
                i += 2;
            }
            "--peek" => {
                peek = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    let dir = session_queue_dir(session_id);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        let messages = sorted_messages(&dir);

        if let Some((id, path)) = messages.first() {
            let data =
                fs::read_to_string(path).map_err(|e| format!("failed to read message: {e}"))?;
            let msg: Value =
                serde_json::from_str(&data).map_err(|e| format!("failed to parse message: {e}"))?;

            if !peek {
                let _ = fs::remove_file(path);
            }

            return Ok(json!({
                "message_id": id,
                "from": msg["from"],
                "body": msg["body"],
                "timestamp": msg["timestamp"],
            }));
        }

        if std::time::Instant::now() >= deadline {
            return Ok(json!({ "empty": true }));
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn cmd_list(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let session_id = args.first().ok_or("usage: cos ipc list <session-id>")?;
    let dir = session_queue_dir(session_id);
    let messages = sorted_messages(&dir);

    let previews: Vec<Value> = messages
        .iter()
        .filter_map(|(id, path)| {
            let data = fs::read_to_string(path).ok()?;
            let msg: Value = serde_json::from_str(&data).ok()?;
            Some(json!({
                "message_id": id,
                "from": msg["from"],
                "body": msg["body"],
                "timestamp": msg["timestamp"],
            }))
        })
        .collect();

    let count = previews.len();
    Ok(json!({
        "session_id": session_id,
        "count": count,
        "messages": previews,
    }))
}

fn cmd_clear(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Delete).map_err(|v| v.to_string())?;
    let session_id = args.first().ok_or("usage: cos ipc clear <session-id>")?;
    let dir = session_queue_dir(session_id);
    let messages = sorted_messages(&dir);
    let cleared = messages.len();

    for (_id, path) in &messages {
        let _ = fs::remove_file(path);
    }

    Ok(json!({
        "session_id": session_id,
        "cleared": cleared,
    }))
}

// ---------------------------------------------------------------------------
// Locks — mutual exclusion for shared resources
// ---------------------------------------------------------------------------

fn locks_dir() -> PathBuf {
    ipc_dir().join("locks")
}

/// Check whether a process with the given PID is still alive.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 doesn't send a signal but checks if the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}

fn cmd_lock(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;
    let mut holder: Option<String> = None;
    let mut timeout_secs: u64 = 0;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--holder" if i + 1 < args.len() => {
                holder = Some(args[i + 1].clone());
                i += 2;
            }
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| "timeout must be a non-negative integer".to_string())?;
                i += 2;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }

    let resource = positional
        .first()
        .ok_or("usage: cos ipc lock <resource-name> [--holder <session-id>] [--timeout N]")?;
    let holder = holder.unwrap_or_else(|| format!("pid-{}", std::process::id()));

    let dir = locks_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create locks dir: {e}"))?;

    let lock_path = dir.join(format!("{resource}.lock"));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        // Try to read an existing lock file.
        if let Ok(data) = fs::read_to_string(&lock_path) {
            if let Ok(existing) = serde_json::from_str::<Value>(&data) {
                let existing_holder = existing["holder"].as_str().unwrap_or("");
                let existing_pid = existing["pid"].as_u64().unwrap_or(0) as u32;

                // Same holder already holds the lock.
                if existing_holder == holder {
                    return Ok(json!({
                        "locked": true,
                        "status": "already_held",
                        "resource": resource,
                        "holder": holder,
                    }));
                }

                // Stale lock detection: if the holder's PID is dead, reclaim.
                if existing_pid > 0 && !is_pid_alive(existing_pid) {
                    // Fall through to acquire — the old holder is gone.
                } else {
                    // Lock is held by a live process. Wait or timeout.
                    if std::time::Instant::now() >= deadline {
                        return Ok(json!({
                            "locked": false,
                            "status": "timeout",
                            "resource": resource,
                            "held_by": existing_holder,
                        }));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue;
                }
            }
        }

        // No lock file, or stale lock — acquire it.
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let lock_data = json!({
            "resource": resource,
            "holder": holder,
            "pid": std::process::id(),
            "acquired_at": timestamp,
        });
        let data = serde_json::to_string_pretty(&lock_data)
            .map_err(|e| format!("failed to serialize lock: {e}"))?;
        fs::write(&lock_path, data).map_err(|e| format!("failed to write lock file: {e}"))?;

        return Ok(json!({
            "locked": true,
            "status": "acquired",
            "resource": resource,
            "holder": holder,
        }));
    }
}

fn cmd_unlock(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;
    let mut holder: Option<String> = None;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--holder" if i + 1 < args.len() => {
                holder = Some(args[i + 1].clone());
                i += 2;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }

    let resource = positional
        .first()
        .ok_or("usage: cos ipc unlock <resource-name> [--holder <session-id>]")?;

    let lock_path = locks_dir().join(format!("{resource}.lock"));

    if !lock_path.exists() {
        return Ok(json!({
            "unlocked": false,
            "status": "not_locked",
            "resource": resource,
        }));
    }

    // If holder is specified, verify it matches before unlocking.
    if let Some(ref required_holder) = holder {
        let data =
            fs::read_to_string(&lock_path).map_err(|e| format!("failed to read lock file: {e}"))?;
        let existing: Value =
            serde_json::from_str(&data).map_err(|e| format!("failed to parse lock file: {e}"))?;
        let current_holder = existing["holder"].as_str().unwrap_or("");
        if current_holder != required_holder.as_str() {
            return Ok(json!({
                "unlocked": false,
                "status": "holder_mismatch",
                "resource": resource,
                "held_by": current_holder,
            }));
        }
    }

    fs::remove_file(&lock_path).map_err(|e| format!("failed to remove lock file: {e}"))?;

    Ok(json!({
        "unlocked": true,
        "status": "released",
        "resource": resource,
    }))
}

fn cmd_locks(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    let dir = locks_dir();
    if !dir.exists() {
        return Ok(json!({ "count": 0, "locks": [] }));
    }

    let mut locks: Vec<Value> = fs::read_dir(&dir)
        .map_err(|e| format!("failed to read locks dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".lock") {
                return None;
            }
            let data = fs::read_to_string(e.path()).ok()?;
            let lock: Value = serde_json::from_str(&data).ok()?;
            Some(lock)
        })
        .collect();
    locks.sort_by(|a, b| {
        let ta = a["acquired_at"].as_str().unwrap_or("");
        let tb = b["acquired_at"].as_str().unwrap_or("");
        ta.cmp(tb)
    });

    let count = locks.len();
    Ok(json!({
        "count": count,
        "locks": locks,
    }))
}

// ---------------------------------------------------------------------------
// Barriers — wait until N agents reach a synchronization point
// ---------------------------------------------------------------------------

fn barriers_dir() -> PathBuf {
    ipc_dir().join("barriers")
}

fn cmd_barrier(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Write).map_err(|v| v.to_string())?;
    let mut expect: Option<u64> = None;
    let mut session: Option<String> = None;
    let mut timeout_secs: u64 = 0;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--expect" if i + 1 < args.len() => {
                expect = Some(
                    args[i + 1]
                        .parse::<u64>()
                        .map_err(|_| "expect must be a positive integer".to_string())?,
                );
                i += 2;
            }
            "--session" if i + 1 < args.len() => {
                session = Some(args[i + 1].clone());
                i += 2;
            }
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| "timeout must be a non-negative integer".to_string())?;
                i += 2;
            }
            _ => {
                positional.push(args[i].clone());
                i += 1;
            }
        }
    }

    let name = positional
        .first()
        .ok_or("usage: cos ipc barrier <name> --expect <N> --session <session-id> [--timeout T]")?;
    let expect = expect.ok_or("--expect <N> is required for barrier")?;
    let session = session.ok_or("--session <session-id> is required for barrier")?;

    let dir = barriers_dir().join(name);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create barrier dir: {e}"))?;

    // 1. Write this session's ready file.
    let ready_path = dir.join(format!("{session}.ready"));
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    fs::write(&ready_path, &timestamp).map_err(|e| format!("failed to write ready file: {e}"))?;

    // 2. Poll until enough .ready files exist.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        let ready_sessions = list_ready_sessions(&dir);
        let ready_count = ready_sessions.len() as u64;

        if ready_count >= expect {
            return Ok(json!({
                "barrier": name,
                "status": "reached",
                "expected": expect,
                "ready_count": ready_count,
                "sessions": ready_sessions,
            }));
        }

        if std::time::Instant::now() >= deadline {
            return Ok(json!({
                "barrier": name,
                "status": "timeout",
                "expected": expect,
                "ready_count": ready_count,
                "sessions": ready_sessions,
            }));
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// List session IDs that have written a `.ready` file in a barrier directory.
fn list_ready_sessions(dir: &PathBuf) -> Vec<String> {
    let mut sessions: Vec<String> = fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".ready").map(|s| s.to_string())
        })
        .collect();
    sessions.sort();
    sessions
}

#[cfg(test)]
mod tests {
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
            let dir = env::temp_dir().join(format!("cos-ipc-test-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create test dir");
            env::set_var("COS_DATA_DIR", &dir);
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
            let dir = env::temp_dir().join(format!("cos-ipc-test-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("create test dir");
            env::set_var("COS_DATA_DIR", &dir);
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

        // Unlock it.
        let r = cmd_unlock(&vec![res.clone()]).unwrap();
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
        cmd_unlock(&vec![res]).unwrap();
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

        cmd_unlock(&vec![res]).unwrap();
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

        cmd_unlock(&vec![res]).unwrap();
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

        cmd_unlock(&vec![res1]).unwrap();
        cmd_unlock(&vec![res2]).unwrap();
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

        let r = run("unlock", &vec![res]).unwrap();
        assert_eq!(r["unlocked"], true);
    }
}
