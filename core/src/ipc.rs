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

fn ipc_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()),
    )
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
                let id = name.strip_suffix(".json").unwrap().to_string();
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
    let sender = from.unwrap_or_default();
    let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let dir = session_queue_dir(target);
    fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create queue dir: {e}"))?;

    let message_id = next_message_id(&dir);
    let msg = json!({
        "from": sender,
        "body": body,
        "timestamp": timestamp,
    });

    let path = dir.join(format!("{message_id}.json"));
    let data = serde_json::to_string_pretty(&msg)
        .map_err(|e| format!("failed to serialize message: {e}"))?;
    fs::write(&path, data)
        .map_err(|e| format!("failed to write message: {e}"))?;

    Ok(json!({
        "sent": true,
        "target": target,
        "message_id": message_id,
    }))
}

fn cmd_recv(args: &[String]) -> Result<Value, String> {
    let session_id = args.first().ok_or("usage: cos ipc recv <session-id> [--timeout N] [--peek]")?;
    let mut timeout_secs: u64 = 0;
    let mut peek = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = args[i + 1].parse::<u64>()
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
            let data = fs::read_to_string(path)
                .map_err(|e| format!("failed to read message: {e}"))?;
            let msg: Value = serde_json::from_str(&data)
                .map_err(|e| format!("failed to parse message: {e}"))?;

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::{atomic::{AtomicU32, Ordering}, Once};

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
}
