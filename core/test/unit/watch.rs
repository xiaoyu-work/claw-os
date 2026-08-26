use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, Once};

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);
static INIT: Once = Once::new();
/// Serialize tests that operate on the shared watch/history.jsonl file.
static HISTORY_LOCK: Mutex<()> = Mutex::new(());

fn setup_shared_dir() {
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("cos-test-shared-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        std::env::set_var("COS_DATA_DIR", &dir);
    });
}

/// Generate a unique test directory to avoid cross-test interference.
fn unique_test_dir(prefix: &str) -> PathBuf {
    let n = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let pid = std::process::id();
    std::env::temp_dir().join(format!("cos-watch-{prefix}-{pid}-{n}"))
}

#[test]
fn watch_file_detects_creation() {
    let dir = unique_test_dir("create");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file_path = dir.join("newfile.txt");

    // Spawn a thread that creates the file after 200ms
    let fp = file_path.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        fs::write(&fp, "hello").unwrap();
    });

    let result = cmd_watch_file(&[
        file_path.to_string_lossy().to_string(),
        "--timeout".into(),
        "5".into(),
    ]);
    let val = result.unwrap();
    assert_eq!(val["status"], "changed");
    assert_eq!(val["event"], "created");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn watch_file_detects_modification() {
    let dir = unique_test_dir("modify");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file_path = dir.join("existing.txt");
    fs::write(&file_path, "original").unwrap();

    // Wait a moment so the initial stat is captured, then modify
    let fp = file_path.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(800));
        let mut f = fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(&fp)
            .unwrap();
        f.write_all(b" appended data").unwrap();
    });

    let result = cmd_watch_file(&[
        file_path.to_string_lossy().to_string(),
        "--timeout".into(),
        "5".into(),
    ]);
    let val = result.unwrap();
    assert_eq!(val["status"], "changed");
    assert_eq!(val["event"], "modified");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn watch_file_timeout() {
    let dir = unique_test_dir("timeout");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file_path = dir.join("stable.txt");
    fs::write(&file_path, "no change").unwrap();

    let result = cmd_watch_file(&[
        file_path.to_string_lossy().to_string(),
        "--timeout".into(),
        "1".into(),
    ]);
    let val = result.unwrap();
    assert_eq!(val["status"], "timeout");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn watch_dir_detects_new_file() {
    let dir = unique_test_dir("dir");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();

    let d = dir.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(200));
        fs::write(d.join("newfile.txt"), "hello").unwrap();
    });

    let result = cmd_watch_dir(&[
        dir.to_string_lossy().to_string(),
        "--timeout".into(),
        "5".into(),
    ]);
    let val = result.unwrap();
    assert_eq!(val["status"], "changed");
    let events = val["events"].as_array().unwrap();
    assert!(events.iter().any(|e| e["event"] == "created"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn parse_timeout_default() {
    assert_eq!(parse_timeout(&["somefile".into()]), DEFAULT_TIMEOUT_SECS);
}

#[test]
fn parse_timeout_custom() {
    assert_eq!(
        parse_timeout(&["somefile".into(), "--timeout".into(), "10".into()]),
        10
    );
}

// --- New tests ---

#[test]
fn test_watch_multi_timeout() {
    // Multi watch with no events should time out.
    let dir = unique_test_dir("multi-timeout");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let file_path = dir.join("stable.txt");
    fs::write(&file_path, "unchanged").unwrap();

    let result = cmd_watch_multi(&[
        "--file".into(),
        file_path.to_string_lossy().to_string(),
        "--timeout".into(),
        "1".into(),
    ]);
    let val = result.unwrap();
    assert_eq!(val["status"], "timeout");
    // Verify the watched object is present.
    assert!(val["watched"]["files"].as_array().unwrap().len() == 1);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_watch_history_empty() {
    setup_shared_dir();
    let _lock = HISTORY_LOCK.lock().unwrap();

    // Clean the watch subdir so we get a fresh history.
    let watch_dir = PathBuf::from(std::env::var("COS_DATA_DIR").unwrap()).join("watch");
    let _ = fs::remove_dir_all(&watch_dir);

    let result = cmd_watch_history(&[]);
    let val = result.unwrap();
    assert_eq!(val["count"], 0);
    assert!(val["events"].as_array().unwrap().is_empty());
}

#[test]
fn test_watch_history_write_and_read() {
    setup_shared_dir();
    let _lock = HISTORY_LOCK.lock().unwrap();

    // Clean the watch subdir so we get a fresh history.
    let data_dir = PathBuf::from(std::env::var("COS_DATA_DIR").unwrap());
    let watch_dir = data_dir.join("watch");
    let _ = fs::remove_dir_all(&watch_dir);
    fs::create_dir_all(&watch_dir).unwrap();

    let hist_file = watch_dir.join("history.jsonl");
    // Write two entries.
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&hist_file)
            .unwrap();
        writeln!(
            f,
            "{}",
            json!({"timestamp": "2026-03-25T10:00:00Z", "source": "file", "path": "/home/cos/main.py", "event": "modified"})
        )
        .unwrap();
        writeln!(
            f,
            "{}",
            json!({"timestamp": "2026-03-25T10:00:05Z", "source": "proc", "session": "w1", "event": "exited"})
        )
        .unwrap();
    }

    // Read all.
    let result = cmd_watch_history(&[]).unwrap();
    assert_eq!(result["count"], 2);

    // Filter by source.
    let result = cmd_watch_history(&["--source".into(), "file".into()]).unwrap();
    assert_eq!(result["count"], 1);
    assert_eq!(result["events"][0]["path"], "/home/cos/main.py");

    // Filter by --since.
    let result = cmd_watch_history(&["--since".into(), "2026-03-25T10:00:03Z".into()]).unwrap();
    assert_eq!(result["count"], 1);
    assert_eq!(result["events"][0]["source"], "proc");

    // Limit.
    let result = cmd_watch_history(&["--limit".into(), "1".into()]).unwrap();
    assert_eq!(result["count"], 1);
    // Should be the last entry.
    assert_eq!(result["events"][0]["source"], "proc");
}

#[test]
fn test_log_watch_event() {
    setup_shared_dir();
    let _lock = HISTORY_LOCK.lock().unwrap();

    // Clean the watch subdir so we get a fresh history.
    let data_dir = PathBuf::from(std::env::var("COS_DATA_DIR").unwrap());
    let watch_dir = data_dir.join("watch");
    let _ = fs::remove_dir_all(&watch_dir);

    let event = json!({"path": "/home/cos/test.rs", "event": "created"});
    log_watch_event("file", &event);

    let hist_file = watch_dir.join("history.jsonl");
    assert!(hist_file.exists(), "history file should be created");

    let content = fs::read_to_string(&hist_file).unwrap();
    let parsed: Value = serde_json::from_str(content.trim()).unwrap();
    assert_eq!(parsed["source"], "file");
    assert_eq!(parsed["path"], "/home/cos/test.rs");
    assert_eq!(parsed["event"], "created");
    assert!(
        parsed["timestamp"].as_str().is_some(),
        "should have timestamp"
    );
}

#[test]
fn test_parse_multi_flag() {
    let args: Vec<String> = vec![
        "--file".into(),
        "/a.txt".into(),
        "--dir".into(),
        "/b/".into(),
        "--file".into(),
        "/c.txt".into(),
    ];
    let files = parse_multi_flag(&args, "--file");
    assert_eq!(files, vec!["/a.txt", "/c.txt"]);
    let dirs = parse_multi_flag(&args, "--dir");
    assert_eq!(dirs, vec!["/b/"]);
    let procs = parse_multi_flag(&args, "--proc");
    assert!(procs.is_empty());
}

#[test]
fn test_count_ipc_messages() {
    let dir = unique_test_dir("ipc-count");
    let _ = fs::remove_dir_all(&dir);
    // Non-existent dir should return 0.
    assert_eq!(count_ipc_messages(&dir), 0);

    fs::create_dir_all(&dir).unwrap();
    assert_eq!(count_ipc_messages(&dir), 0);

    // Add some .json files.
    fs::write(dir.join("0001.json"), "{}").unwrap();
    fs::write(dir.join("0002.json"), "{}").unwrap();
    fs::write(dir.join("readme.txt"), "not a message").unwrap();
    assert_eq!(count_ipc_messages(&dir), 2);

    let _ = fs::remove_dir_all(&dir);
}

/// History rotation regression: once the active log exceeds the
/// configured byte cap, the rotation step renames it to `<path>.1`
/// so the next append starts from a fresh file. Without this the
/// log grew unbounded and `cmd_watch_history` had to load the whole
/// thing on every read.
#[test]
fn test_append_with_rotation_rotates_when_over_cap() {
    let dir = unique_test_dir("rotate");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rotating.jsonl");

    // Cap intentionally tiny so a single longish line trips it.
    let cap: u64 = 64;
    let long_line = "x".repeat(120);

    let backup = {
        let mut b = path.as_os_str().to_owned();
        b.push(".1");
        std::path::PathBuf::from(b)
    };

    // First append: writes line, size > cap → rotation moves the
    // file to `<path>.1` in the same call.
    append_with_rotation(&path, &long_line, cap).unwrap();
    assert!(
        backup.exists(),
        "rotation backup .1 should exist after first oversize append"
    );
    assert!(
        !path.exists(),
        "live file should be gone immediately after rotation"
    );
    let backup_content = fs::read_to_string(&backup).unwrap();
    assert!(
        backup_content.contains(&long_line),
        "rotated backup must hold the pre-rotation line"
    );

    // Second append: live file is recreated from scratch.
    append_with_rotation(&path, "second", cap).unwrap();
    let live_content = fs::read_to_string(&path).unwrap();
    assert_eq!(
        live_content.trim(),
        "second",
        "live file must be fresh post-rotation"
    );
    assert!(
        !live_content.contains(&long_line),
        "live file must not carry over pre-rotation content"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// History rotation regression: when the file is under the cap, no
/// rotation should happen and lines accumulate normally.
#[test]
fn test_append_with_rotation_no_rotate_under_cap() {
    let dir = unique_test_dir("no-rotate");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("small.jsonl");

    // Large cap so the small lines we append never trip rotation.
    let cap: u64 = 1024 * 1024;
    append_with_rotation(&path, "one", cap).unwrap();
    append_with_rotation(&path, "two", cap).unwrap();
    append_with_rotation(&path, "three", cap).unwrap();

    let backup = {
        let mut b = path.as_os_str().to_owned();
        b.push(".1");
        std::path::PathBuf::from(b)
    };
    assert!(!backup.exists(), "no rotation expected when below the cap");

    let content = fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines, vec!["one", "two", "three"]);

    let _ = fs::remove_dir_all(&dir);
}

/// RAII regression: dropping an `InotifyFd` must close the
/// underlying kernel inotify instance. Verified indirectly by
/// observing that a fresh fd reuses the slot — without close the
/// process would eventually hit the per-uid inotify cap.
#[cfg(target_os = "linux")]
#[test]
fn test_inotify_fd_drop_closes() {
    let raw = {
        let fd = inotify_impl::InotifyFd::new().unwrap();
        fd.as_raw_fd()
    };
    // The fd should now be closed. close(fd) on an already-closed
    // descriptor returns -1 / EBADF. We use that as the witness.
    let rc = unsafe { libc::close(raw) };
    assert_eq!(
        rc, -1,
        "InotifyFd::Drop should have closed the fd already (close()-on-closed returns -1)"
    );
    let err = std::io::Error::last_os_error().raw_os_error();
    assert_eq!(err, Some(libc::EBADF));
}
