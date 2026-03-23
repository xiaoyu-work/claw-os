/// File and directory watcher — polling-based, no daemon required.
///
/// Agents need to react to changes (file modified, process exited, new files
/// created) without running a background daemon. This module provides
/// stat-based polling that blocks until a change is detected or timeout
/// expires, then returns structured JSON describing what changed.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const POLL_INTERVAL_MS: u64 = 500;

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "file" => cmd_watch_file(args),
        "dir" => cmd_watch_dir(args),
        "proc" => cmd_watch_proc(args),
        _ => Err(format!("unknown watch command: {command}")),
    }
}

/// Snapshot of a file's metadata for change detection.
#[derive(Debug, Clone)]
struct FileStat {
    modified: Option<SystemTime>,
    size: u64,
    exists: bool,
}

fn stat_file(path: &PathBuf) -> FileStat {
    match fs::metadata(path) {
        Ok(meta) => FileStat {
            modified: meta.modified().ok(),
            size: meta.len(),
            exists: true,
        },
        Err(_) => FileStat {
            modified: None,
            size: 0,
            exists: false,
        },
    }
}

/// Watch a single file for changes.
///
/// Usage: cos watch file <path> [--timeout N]
///
/// Returns when the file is created, modified, or deleted.
fn cmd_watch_file(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos watch file <path> [--timeout N]".into());
    }

    let path = PathBuf::from(&args[0]);
    let timeout = parse_timeout(args);
    let deadline = Instant::now() + Duration::from_secs(timeout);

    let initial = stat_file(&path);

    loop {
        if Instant::now() >= deadline {
            return Ok(json!({
                "status": "timeout",
                "path": path.to_string_lossy(),
                "timeout_secs": timeout,
                "message": "no changes detected within timeout",
            }));
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        let current = stat_file(&path);

        // Detect changes
        if !initial.exists && current.exists {
            return Ok(json!({
                "status": "changed",
                "path": path.to_string_lossy(),
                "event": "created",
                "size": current.size,
            }));
        }
        if initial.exists && !current.exists {
            return Ok(json!({
                "status": "changed",
                "path": path.to_string_lossy(),
                "event": "deleted",
            }));
        }
        if initial.exists && current.exists {
            let size_changed = initial.size != current.size;
            let time_changed = match (initial.modified, current.modified) {
                (Some(a), Some(b)) => a != b,
                _ => false,
            };
            if size_changed || time_changed {
                return Ok(json!({
                    "status": "changed",
                    "path": path.to_string_lossy(),
                    "event": "modified",
                    "old_size": initial.size,
                    "new_size": current.size,
                }));
            }
        }
    }
}

/// Watch a directory for changes (new files, deleted files, modified files).
///
/// Usage: cos watch dir <path> [--timeout N]
///
/// Returns when any file in the directory is created, modified, or deleted.
fn cmd_watch_dir(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos watch dir <path> [--timeout N]".into());
    }

    let path = PathBuf::from(&args[0]);
    if !path.is_dir() {
        return Err(format!("not a directory: {}", path.display()));
    }

    let timeout = parse_timeout(args);
    let deadline = Instant::now() + Duration::from_secs(timeout);

    let initial = snapshot_dir(&path);

    loop {
        if Instant::now() >= deadline {
            return Ok(json!({
                "status": "timeout",
                "path": path.to_string_lossy(),
                "timeout_secs": timeout,
                "message": "no changes detected within timeout",
            }));
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        let current = snapshot_dir(&path);

        let mut events = Vec::new();

        // Check for new files
        for (name, stat) in &current {
            if !initial.contains_key(name) {
                events.push(json!({
                    "event": "created",
                    "name": name,
                    "size": stat.size,
                }));
            }
        }

        // Check for deleted files
        for name in initial.keys() {
            if !current.contains_key(name) {
                events.push(json!({
                    "event": "deleted",
                    "name": name,
                }));
            }
        }

        // Check for modified files
        for (name, curr_stat) in &current {
            if let Some(init_stat) = initial.get(name) {
                let size_changed = init_stat.size != curr_stat.size;
                let time_changed = match (init_stat.modified, curr_stat.modified) {
                    (Some(a), Some(b)) => a != b,
                    _ => false,
                };
                if size_changed || time_changed {
                    events.push(json!({
                        "event": "modified",
                        "name": name,
                        "old_size": init_stat.size,
                        "new_size": curr_stat.size,
                    }));
                }
            }
        }

        if !events.is_empty() {
            return Ok(json!({
                "status": "changed",
                "path": path.to_string_lossy(),
                "events": events,
                "count": events.len(),
            }));
        }
    }
}

/// Watch a process session for exit.
///
/// Usage: cos watch proc <session-id> [--timeout N]
///
/// Delegates to `cos proc wait` but wrapped in the watch interface.
fn cmd_watch_proc(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos watch proc <session-id> [--timeout N]".into());
    }

    // Delegate to proc wait
    crate::proc::run("wait", args)
}

/// Take a snapshot of all files in a directory (non-recursive, one level).
fn snapshot_dir(path: &PathBuf) -> HashMap<String, FileStat> {
    let mut map = HashMap::new();
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                let stat = stat_file(&entry.path());
                map.insert(name.to_string(), stat);
            }
        }
    }
    map
}

/// Parse --timeout N from args, defaulting to DEFAULT_TIMEOUT_SECS.
fn parse_timeout(args: &[String]) -> u64 {
    for i in 0..args.len() {
        if args[i] == "--timeout" {
            if let Some(val) = args.get(i + 1) {
                return val.parse().unwrap_or(DEFAULT_TIMEOUT_SECS);
            }
        }
    }
    DEFAULT_TIMEOUT_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn watch_file_detects_creation() {
        let dir = std::env::temp_dir().join("cos-watch-test-create");
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
        let dir = std::env::temp_dir().join("cos-watch-test-modify");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file_path = dir.join("existing.txt");
        fs::write(&file_path, "original").unwrap();

        // Wait a moment so the initial stat is captured, then modify
        let fp = file_path.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(800));
            let mut f = fs::OpenOptions::new().write(true).append(true).open(&fp).unwrap();
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
        let dir = std::env::temp_dir().join("cos-watch-test-timeout");
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
        let dir = std::env::temp_dir().join("cos-watch-test-dir");
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
        assert_eq!(parse_timeout(&["somefile".into(), "--timeout".into(), "10".into()]), 10);
    }
}
