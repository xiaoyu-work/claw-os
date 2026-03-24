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

use crate::policy::{self, OpType};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const POLL_INTERVAL_MS: u64 = 500;

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    match command {
        "file" => cmd_watch_file(args),
        "dir" => cmd_watch_dir(args),
        "proc" => cmd_watch_proc(args),
        "on" => cmd_watch_on(args),
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

/// Unified OS event watcher — subscribe to any OS-level event.
///
/// Usage: cos watch on <event-type> [--timeout N] [event-specific args]
///
/// Event types:
///   proc.exit --session <id>         — wait for a process to exit
///   fs.change --path <dir>           — wait for file changes in a directory
///   service.health-fail --name <svc> — wait for a service health check to fail
///   checkpoint.created               — wait for a new checkpoint to be created
///   quota.exceeded                   — wait for quota to be exceeded
fn cmd_watch_on(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err(
            "usage: cos watch on <event-type> [--timeout N] [...]\n\
             event types: proc.exit, fs.change, service.health-fail, checkpoint.created, quota.exceeded"
                .into(),
        );
    }

    let event_type = &args[0];
    let rest: Vec<String> = args[1..].to_vec();
    let timeout = parse_timeout(&rest);

    match event_type.as_str() {
        "proc.exit" => watch_proc_exit(&rest, timeout),
        "fs.change" => watch_fs_change(&rest, timeout),
        "service.health-fail" => watch_service_health_fail(&rest, timeout),
        "checkpoint.created" => watch_checkpoint_created(&rest, timeout),
        "quota.exceeded" => watch_quota_exceeded(timeout),
        _ => Err(format!(
            "unknown event type: {event_type}. \
             supported: proc.exit, fs.change, service.health-fail, checkpoint.created, quota.exceeded"
        )),
    }
}

fn parse_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    for i in 0..args.len() {
        if args[i] == flag {
            if let Some(val) = args.get(i + 1) {
                return Some(val.as_str());
            }
        }
    }
    None
}

fn watch_proc_exit(args: &[String], timeout: u64) -> Result<Value, String> {
    let session_id =
        parse_flag(args, "--session").ok_or("--session <id> required for proc.exit")?;

    // Delegate to proc wait
    let wait_args = vec![
        session_id.to_string(),
        "--timeout".into(),
        timeout.to_string(),
    ];
    let result = crate::proc::run("wait", &wait_args)?;

    let status = result["status"].as_str().unwrap_or("unknown");
    Ok(json!({
        "event": "proc.exit",
        "triggered": status == "exited",
        "details": result,
    }))
}

fn watch_fs_change(args: &[String], timeout: u64) -> Result<Value, String> {
    let path = parse_flag(args, "--path").ok_or("--path <dir> required for fs.change")?;

    let watch_args = vec![path.to_string(), "--timeout".into(), timeout.to_string()];
    let result = cmd_watch_dir(&watch_args)?;

    let status = result["status"].as_str().unwrap_or("unknown");
    Ok(json!({
        "event": "fs.change",
        "triggered": status == "changed",
        "details": result,
    }))
}

fn watch_service_health_fail(args: &[String], timeout: u64) -> Result<Value, String> {
    let service_name =
        parse_flag(args, "--name").ok_or("--name <service> required for service.health-fail")?;

    let deadline = Instant::now() + Duration::from_secs(timeout);

    loop {
        // Check service health via the service module
        let health_result =
            crate::service::run("health", &[service_name.to_string(), "--no-restart".into()]);

        match health_result {
            Ok(v) => {
                if v["healthy"] == false {
                    return Ok(json!({
                        "event": "service.health-fail",
                        "triggered": true,
                        "service": service_name,
                        "details": v,
                    }));
                }
            }
            Err(e) => {
                return Ok(json!({
                    "event": "service.health-fail",
                    "triggered": true,
                    "service": service_name,
                    "error": e,
                }));
            }
        }

        if Instant::now() >= deadline {
            return Ok(json!({
                "event": "service.health-fail",
                "triggered": false,
                "service": service_name,
                "status": "timeout",
            }));
        }

        thread::sleep(Duration::from_secs(2));
    }
}

fn watch_checkpoint_created(args: &[String], timeout: u64) -> Result<Value, String> {
    let overlay_dir =
        PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
            .join("overlay")
            .join("checkpoints");

    // Snapshot current checkpoint count
    let initial_count = if overlay_dir.exists() {
        fs::read_dir(&overlay_dir)
            .map(|e| {
                e.filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };

    let _ = parse_flag(args, "--timeout"); // consumed by caller
    let deadline = Instant::now() + Duration::from_secs(timeout);

    loop {
        let current_count = if overlay_dir.exists() {
            fs::read_dir(&overlay_dir)
                .map(|e| {
                    e.filter_map(|e| e.ok())
                        .filter(|e| e.path().is_dir())
                        .count()
                })
                .unwrap_or(0)
        } else {
            0
        };

        if current_count > initial_count {
            return Ok(json!({
                "event": "checkpoint.created",
                "triggered": true,
                "previous_count": initial_count,
                "current_count": current_count,
            }));
        }

        if Instant::now() >= deadline {
            return Ok(json!({
                "event": "checkpoint.created",
                "triggered": false,
                "status": "timeout",
                "checkpoint_count": current_count,
            }));
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

fn watch_quota_exceeded(timeout: u64) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout);

    loop {
        if let Err(e) = crate::checkpoint::check_quota(0) {
            return Ok(json!({
                "event": "quota.exceeded",
                "triggered": true,
                "message": e,
            }));
        }

        if Instant::now() >= deadline {
            return Ok(json!({
                "event": "quota.exceeded",
                "triggered": false,
                "status": "timeout",
            }));
        }

        thread::sleep(Duration::from_secs(1));
    }
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
        assert_eq!(
            parse_timeout(&["somefile".into(), "--timeout".into(), "10".into()]),
            10
        );
    }
}
