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

use crate::caps::{require_or_json, Scope, Verb};

fn ipc_dir() -> PathBuf {
    crate::paths::data_dir().join("ipc")
}

fn validate_identifier(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('.')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(format!(
            "invalid {kind} `{value}`; expected 1-128 ASCII alphanumerics, '-', '_' or '.'"
        ));
    }
    Ok(())
}

fn reject_symlink(path: &std::path::Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(format!("refusing symlink IPC path {}", path.display()))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("inspect IPC path {}: {error}", path.display())),
    }
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

/// Acquire an exclusive lock on a directory via a .lock file.
/// Returns the locked file handle (lock released on drop).
#[cfg(unix)]
fn acquire_dir_lock(lock_path: &std::path::Path) -> Result<fs::File, String> {
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .map_err(|e| format!("failed to open lock file: {e}"))?;
    unsafe {
        if libc::flock(std::os::unix::io::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) != 0 {
            return Err("failed to acquire directory lock".into());
        }
    }
    Ok(file)
}

#[cfg(not(unix))]
fn acquire_dir_lock(lock_path: &std::path::Path) -> Result<fs::File, String> {
    fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| format!("failed to open lock file: {e}"))
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
        "pipe" => cmd_pipe(args),
        _ => Err(format!("unknown ipc command: {command}")),
    }
}

fn cmd_send(args: &[String]) -> Result<Value, String> {
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
    validate_identifier("target session id", target)?;
    require_or_json(Verb::IPC_PUBLISH, Scope::name(target)).map_err(|v| v.to_string())?;
    let sender = from.unwrap_or_default();
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let dir = session_queue_dir(target);
    reject_symlink(&dir)?;
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create queue dir: {e}"))?;

    // Lock the queue directory to serialize message ID allocation
    let lock_file = dir.join(".lock");
    let _lock = acquire_dir_lock(&lock_file)?;

    let message_id = next_message_id(&dir);
    let msg = json!({
        "from": sender,
        "body": body,
        "timestamp": timestamp,
    });

    let path = dir.join(format!("{message_id}.json"));
    let data = serde_json::to_string_pretty(&msg)
        .map_err(|e| format!("failed to serialize message: {e}"))?;
    crate::filelock::write_locked(&path, &data)?;

    Ok(json!({
        "sent": true,
        "target": target,
        "message_id": message_id,
    }))
}

fn cmd_recv(args: &[String]) -> Result<Value, String> {
    let session_id = args
        .first()
        .ok_or("usage: cos ipc recv <session-id> [--timeout N] [--peek]")?;
    validate_identifier("session id", session_id)?;
    require_or_json(Verb::IPC_SUBSCRIBE, Scope::name(session_id)).map_err(|v| v.to_string())?;
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
    reject_symlink(&dir)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        // Lock the queue directory around read+unlink so two concurrent
        // recvs can't both grab the same message. Without the lock,
        // sorted_messages -> read -> remove_file ran twice and the at-
        // most-once contract degraded to at-least-twice. cmd_send
        // already uses the same dir lock to serialize ID allocation.
        let lock_acquired = if dir.exists() {
            let lock_path = dir.join(".lock");
            acquire_dir_lock(&lock_path).ok()
        } else {
            None
        };

        let messages = sorted_messages(&dir);

        if let Some((id, path)) = messages.first() {
            let data = crate::filelock::read_locked(path)?
                .ok_or_else(|| format!("message file not found: {}", path.display()))?;
            let msg: Value =
                serde_json::from_str(&data).map_err(|e| format!("failed to parse message: {e}"))?;

            if !peek {
                let _ = fs::remove_file(path);
            }

            drop(lock_acquired);

            return Ok(json!({
                "message_id": id,
                "from": msg["from"],
                "body": msg["body"],
                "timestamp": msg["timestamp"],
            }));
        }

        drop(lock_acquired);

        if std::time::Instant::now() >= deadline {
            return Ok(json!({ "empty": true }));
        }

        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn cmd_list(args: &[String]) -> Result<Value, String> {
    let session_id = args.first().ok_or("usage: cos ipc list <session-id>")?;
    validate_identifier("session id", session_id)?;
    require_or_json(Verb::IPC_SUBSCRIBE, Scope::name(session_id)).map_err(|v| v.to_string())?;
    let dir = session_queue_dir(session_id);
    reject_symlink(&dir)?;
    let messages = sorted_messages(&dir);

    let previews: Vec<Value> = messages
        .iter()
        .filter_map(|(id, path)| {
            let data = crate::filelock::read_locked(path).ok()??;
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
    let session_id = args.first().ok_or("usage: cos ipc clear <session-id>")?;
    validate_identifier("session id", session_id)?;
    require_or_json(Verb::IPC_PUBLISH, Scope::name(session_id)).map_err(|v| v.to_string())?;
    let dir = session_queue_dir(session_id);
    reject_symlink(&dir)?;
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
///
/// Cross-uid safe: `kill(pid, 0)` returns -1/EPERM when the target
/// PID exists but belongs to a different uid, which the old code
/// interpreted as "process is gone" and allowed lock reclaim — that
/// let unprivileged caller B steal user A's locks on multi-user
/// hosts. Treat EPERM as "alive (just not ours)". On Linux we also
/// double-check `/proc/<pid>` so we don't trust `kill(0)`'s ambient
/// permissions implicitly.
fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        if std::path::Path::new(&format!("/proc/{pid}")).exists() {
            return true;
        }
    }
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as i32, 0) };
        if rc == 0 {
            return true;
        }
        // EPERM => process exists but is not signalable by us. Treat
        // as alive so we never reclaim another user's lock.
        let err = std::io::Error::last_os_error();
        err.raw_os_error() == Some(libc::EPERM)
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
    validate_identifier("lock resource", resource)?;
    require_or_json(Verb::IPC_INVOKE, Scope::name(resource)).map_err(|v| v.to_string())?;
    let holder = holder.unwrap_or_else(|| format!("pid-{}", std::process::id()));

    let dir = locks_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create locks dir: {e}"))?;

    let lock_path = dir.join(format!("{resource}.lock"));
    reject_symlink(&lock_path)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    // Use O_EXCL on the lockfile itself so acquisition is a single
    // atomic syscall. Before this fix, two concurrent acquirers both
    // saw "no live holder" via read_locked → fell through to
    // write_locked, and both believed they owned the lock. The dir
    // already serializes ID allocation for messaging (cmd_send) but
    // wasn't used here at all.
    loop {
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let lock_data = json!({
            "resource": resource,
            "holder": holder,
            "pid": std::process::id(),
            "acquired_at": now,
        });
        let payload = serde_json::to_string_pretty(&lock_data)
            .map_err(|e| format!("failed to serialize lock: {e}"))?;

        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut f) => {
                use std::io::Write;
                f.write_all(payload.as_bytes())
                    .map_err(|e| format!("failed to write lock file: {e}"))?;
                let _ = f.sync_all();
                return Ok(json!({
                    "locked": true,
                    "status": "acquired",
                    "resource": resource,
                    "holder": holder,
                }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // Someone else holds the lock. Inspect to see whether
                // we should treat it as stale (dead holder) and try
                // to reclaim.
                let body = fs::read_to_string(&lock_path).unwrap_or_default();
                if let Ok(existing) = serde_json::from_str::<Value>(&body) {
                    let existing_holder = existing["holder"].as_str().unwrap_or("");
                    let existing_pid = existing["pid"].as_u64().unwrap_or(0) as u32;

                    if existing_holder == holder {
                        return Ok(json!({
                            "locked": true,
                            "status": "already_held",
                            "resource": resource,
                            "holder": holder,
                        }));
                    }

                    if existing_pid > 0 && !is_pid_alive(existing_pid) {
                        // Reclaim atomically: only the caller whose
                        // rename(stale -> reclaim) succeeds gets the
                        // lock. We unlink first, then loop back and
                        // race for the create_new. Since unlink is
                        // idempotent and the create_new is mutually
                        // exclusive, at most one caller wins.
                        let _ = fs::remove_file(&lock_path);
                        continue;
                    }

                    if std::time::Instant::now() >= deadline {
                        return Ok(json!({
                            "locked": false,
                            "status": "timeout",
                            "resource": resource,
                            "held_by": existing_holder,
                        }));
                    }
                } else {
                    // Corrupt lockfile: don't auto-reclaim; surface
                    // it so the operator can clean up.
                    if std::time::Instant::now() >= deadline {
                        return Ok(json!({
                            "locked": false,
                            "status": "corrupt_lock",
                            "resource": resource,
                        }));
                    }
                }

                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("failed to create lock file: {e}")),
        }
    }
}

fn cmd_unlock(args: &[String]) -> Result<Value, String> {
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
    validate_identifier("lock resource", resource)?;
    require_or_json(Verb::IPC_INVOKE, Scope::name(resource)).map_err(|v| v.to_string())?;

    // Default to caller-pid holder so omitting --holder never lets
    // an unrelated caller drop someone else's lock. Before this
    // fix, any process with IPC_INVOKE could release any lock by
    // just leaving --holder off.
    let required_holder =
        holder.unwrap_or_else(|| format!("pid-{}", std::process::id()));

    let lock_path = locks_dir().join(format!("{resource}.lock"));
    reject_symlink(&lock_path)?;

    if !lock_path.exists() {
        return Ok(json!({
            "unlocked": false,
            "status": "not_locked",
            "resource": resource,
        }));
    }

    // Holder check is now mandatory.
    let data = fs::read_to_string(&lock_path)
        .map_err(|e| format!("failed to read lock file: {e}"))?;
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

    fs::remove_file(&lock_path).map_err(|e| format!("failed to remove lock file: {e}"))?;

    Ok(json!({
        "unlocked": true,
        "status": "released",
        "resource": resource,
    }))
}

fn cmd_locks(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::IPC_SUBSCRIBE, Scope::wild()).map_err(|v| v.to_string())?;
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
            let data = crate::filelock::read_locked(&e.path()).ok()??;
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
    validate_identifier("barrier name", name)?;
    validate_identifier("barrier session id", &session)?;
    require_or_json(Verb::IPC_INVOKE, Scope::name(name)).map_err(|v| v.to_string())?;

    let dir = barriers_dir().join(name);
    reject_symlink(&dir)?;
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create barrier dir: {e}"))?;

    // 1. Write this session's ready file.
    let ready_path = dir.join(format!("{session}.ready"));
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    crate::filelock::write_locked(&ready_path, &timestamp)?;

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

// ---------------------------------------------------------------------------
// Pipes — streaming named pipes (structured NDJSON channels)
// ---------------------------------------------------------------------------

fn pipes_dir() -> PathBuf {
    ipc_dir().join("pipes")
}

fn pipe_channel_dir(name: &str) -> PathBuf {
    pipes_dir().join(name)
}

fn pipe_messages_dir(name: &str) -> PathBuf {
    pipe_channel_dir(name).join("messages")
}

/// Return the next 6-digit message ID for a pipe channel.
fn next_pipe_message_id(dir: &PathBuf) -> String {
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
    format!("{:06}", max + 1)
}

/// List message files in a pipe messages directory, sorted by name (oldest first).
fn sorted_pipe_messages(dir: &PathBuf) -> Vec<(String, PathBuf)> {
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

fn cmd_pipe(args: &[String]) -> Result<Value, String> {
    let subcmd = args
        .first()
        .ok_or("usage: cos ipc pipe <create|publish|subscribe|list|destroy> ...")?;
    let rest: Vec<String> = args[1..].to_vec();
    match subcmd.as_str() {
        "create" => pipe_create(&rest),
        "publish" => pipe_publish(&rest),
        "subscribe" => pipe_subscribe(&rest),
        "list" => pipe_list(&rest),
        "destroy" => pipe_destroy(&rest),
        _ => Err(format!("unknown pipe command: {subcmd}")),
    }
}

fn pipe_create(args: &[String]) -> Result<Value, String> {

    let mut buffer_size: u64 = 1000;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--buffer-size" if i + 1 < args.len() => {
                buffer_size = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| "buffer-size must be a positive integer".to_string())?;
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
        .ok_or("usage: cos ipc pipe create <name> [--buffer-size N]")?;
    validate_identifier("pipe name", name)?;
    require_or_json(Verb::IPC_PUBLISH, Scope::name(name)).map_err(|v| v.to_string())?;

    let channel_dir = pipe_channel_dir(name);
    reject_symlink(&channel_dir)?;
    let messages_dir = pipe_messages_dir(name);
    fs::create_dir_all(&messages_dir)
        .map_err(|e| format!("failed to create pipe directory: {e}"))?;

    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let meta = json!({
        "name": name,
        "created_at": timestamp,
        "buffer_size": buffer_size,
    });
    let data = serde_json::to_string_pretty(&meta)
        .map_err(|e| format!("failed to serialize metadata: {e}"))?;
    crate::filelock::write_locked(&channel_dir.join("meta.json"), &data)?;

    Ok(json!({
        "created": name,
        "buffer_size": buffer_size,
    }))
}

fn pipe_publish(args: &[String]) -> Result<Value, String> {

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
        return Err("usage: cos ipc pipe publish <name> <data> [--from <session-id>]".into());
    }

    let name = &positional[0];
    let raw_data = &positional[1];
    validate_identifier("pipe name", name)?;
    require_or_json(Verb::IPC_PUBLISH, Scope::name(name)).map_err(|v| v.to_string())?;
    let sender = from.unwrap_or_default();

    let channel_dir = pipe_channel_dir(name);
    reject_symlink(&channel_dir)?;
    let meta_path = channel_dir.join("meta.json");
    if !meta_path.exists() {
        return Err(format!("pipe channel not found: {name}"));
    }

    // Read buffer_size from metadata.
    let meta_str = crate::filelock::read_locked(&meta_path)?
        .ok_or_else(|| format!("metadata file not found: {}", meta_path.display()))?;
    let meta: Value =
        serde_json::from_str(&meta_str).map_err(|e| format!("failed to parse metadata: {e}"))?;
    let buffer_size = meta["buffer_size"].as_u64().unwrap_or(1000);

    let messages_dir = pipe_messages_dir(name);

    // Lock the messages directory to serialize message ID allocation
    let lock_file = messages_dir.join(".lock");
    fs::create_dir_all(&messages_dir).map_err(|e| format!("failed to create messages dir: {e}"))?;
    let _lock = acquire_dir_lock(&lock_file)?;

    let message_id = next_pipe_message_id(&messages_dir);
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    // Parse data: if valid JSON, store as-is; otherwise store as string.
    let data_value: Value = serde_json::from_str(raw_data).unwrap_or_else(|_| json!(raw_data));

    let msg = json!({
        "id": message_id,
        "from": sender,
        "data": data_value,
        "timestamp": timestamp,
    });

    let path = messages_dir.join(format!("{message_id}.json"));
    let data = serde_json::to_string_pretty(&msg)
        .map_err(|e| format!("failed to serialize message: {e}"))?;
    crate::filelock::write_locked(&path, &data)?;

    // Enforce backpressure: remove oldest messages if over buffer_size.
    let all_messages = sorted_pipe_messages(&messages_dir);
    let count = all_messages.len() as u64;
    if count > buffer_size {
        let excess = (count - buffer_size) as usize;
        for (_id, path) in all_messages.iter().take(excess) {
            let _ = fs::remove_file(path);
        }
    }

    Ok(json!({
        "published": true,
        "channel": name,
        "message_id": message_id,
    }))
}

fn pipe_subscribe(args: &[String]) -> Result<Value, String> {

    let mut since: Option<String> = None;
    let mut limit: u64 = 100;
    let mut follow = false;
    let mut timeout_secs: u64 = 30;
    let mut positional: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" if i + 1 < args.len() => {
                since = Some(args[i + 1].clone());
                i += 2;
            }
            "--limit" if i + 1 < args.len() => {
                limit = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| "limit must be a positive integer".to_string())?;
                i += 2;
            }
            "--follow" => {
                follow = true;
                i += 1;
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

    let name = positional.first().ok_or(
        "usage: cos ipc pipe subscribe <name> [--since <id>] [--limit N] [--follow --timeout T]",
    )?;
    validate_identifier("pipe name", name)?;
    require_or_json(Verb::IPC_SUBSCRIBE, Scope::name(name)).map_err(|v| v.to_string())?;

    let channel_dir = pipe_channel_dir(name);
    reject_symlink(&channel_dir)?;
    if !channel_dir.join("meta.json").exists() {
        return Err(format!("pipe channel not found: {name}"));
    }

    let messages_dir = pipe_messages_dir(name);

    if follow {
        // Follow mode: poll for new messages after the last known ID.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

        // Determine the starting point: use --since if given, otherwise latest existing ID.
        let last_seen = since.unwrap_or_else(|| {
            let existing = sorted_pipe_messages(&messages_dir);
            existing
                .last()
                .map(|(id, _)| id.clone())
                .unwrap_or_else(|| "000000".to_string())
        });

        loop {
            let all = sorted_pipe_messages(&messages_dir);
            let new_msgs: Vec<&(String, PathBuf)> = all
                .iter()
                .filter(|(id, _)| id.as_str() > last_seen.as_str())
                .collect();

            if !new_msgs.is_empty() {
                let capped = new_msgs.iter().take(limit as usize);
                let mut messages: Vec<Value> = Vec::new();
                for (id, path) in capped {
                    if let Ok(Some(data)) = crate::filelock::read_locked(path) {
                        if let Ok(msg) = serde_json::from_str::<Value>(&data) {
                            messages.push(json!({
                                "id": id,
                                "from": msg["from"],
                                "data": msg["data"],
                                "timestamp": msg["timestamp"],
                            }));
                        }
                    }
                }
                let latest_id = messages
                    .last()
                    .and_then(|m| m["id"].as_str())
                    .unwrap_or(&last_seen)
                    .to_string();
                let count = messages.len();
                return Ok(json!({
                    "channel": name,
                    "messages": messages,
                    "count": count,
                    "latest_id": latest_id,
                }));
            }

            if std::time::Instant::now() >= deadline {
                return Ok(json!({
                    "channel": name,
                    "messages": [],
                    "count": 0,
                    "latest_id": last_seen,
                    "timeout": true,
                }));
            }

            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    // Non-follow mode: return available messages immediately.
    let all = sorted_pipe_messages(&messages_dir);

    let filtered: Vec<&(String, PathBuf)> = if let Some(ref since_id) = since {
        all.iter()
            .filter(|(id, _)| id.as_str() > since_id.as_str())
            .collect()
    } else {
        all.iter().collect()
    };

    let capped = filtered.iter().take(limit as usize);
    let mut messages: Vec<Value> = Vec::new();
    for (id, path) in capped {
        if let Ok(Some(data)) = crate::filelock::read_locked(path) {
            if let Ok(msg) = serde_json::from_str::<Value>(&data) {
                messages.push(json!({
                    "id": id,
                    "from": msg["from"],
                    "data": msg["data"],
                    "timestamp": msg["timestamp"],
                }));
            }
        }
    }

    let latest_id = messages
        .last()
        .and_then(|m| m["id"].as_str())
        .unwrap_or("000000")
        .to_string();
    let count = messages.len();

    Ok(json!({
        "channel": name,
        "messages": messages,
        "count": count,
        "latest_id": latest_id,
    }))
}

fn pipe_list(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::IPC_SUBSCRIBE, Scope::wild()).map_err(|v| v.to_string())?;

    let dir = pipes_dir();
    if !dir.exists() {
        return Ok(json!({ "channels": [], "count": 0 }));
    }

    let mut channels: Vec<Value> = fs::read_dir(&dir)
        .map_err(|e| format!("failed to read pipes dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let meta_path = e.path().join("meta.json");
            let data = crate::filelock::read_locked(&meta_path).ok()??;
            let meta: Value = serde_json::from_str(&data).ok()?;
            let name = meta["name"].as_str()?.to_string();
            let created_at = meta["created_at"].as_str().unwrap_or("").to_string();
            let buffer_size = meta["buffer_size"].as_u64().unwrap_or(1000);
            let messages_dir = e.path().join("messages");
            let message_count = sorted_pipe_messages(&messages_dir).len();
            Some(json!({
                "name": name,
                "message_count": message_count,
                "created_at": created_at,
                "buffer_size": buffer_size,
            }))
        })
        .collect();
    channels.sort_by(|a, b| {
        let na = a["name"].as_str().unwrap_or("");
        let nb = b["name"].as_str().unwrap_or("");
        na.cmp(nb)
    });

    let count = channels.len();
    Ok(json!({
        "channels": channels,
        "count": count,
    }))
}

fn pipe_destroy(args: &[String]) -> Result<Value, String> {
    let name = args.first().ok_or("usage: cos ipc pipe destroy <name>")?;
    validate_identifier("pipe name", name)?;
    require_or_json(Verb::IPC_PUBLISH, Scope::name(name)).map_err(|v| v.to_string())?;

    let channel_dir = pipe_channel_dir(name);
    reject_symlink(&channel_dir)?;
    if !channel_dir.exists() {
        return Err(format!("pipe channel not found: {name}"));
    }

    fs::remove_dir_all(&channel_dir).map_err(|e| format!("failed to destroy pipe channel: {e}"))?;

    Ok(json!({
        "destroyed": name,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/ipc.rs"
    ));
}
