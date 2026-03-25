/// Event watcher — inotify-based file watching (Linux), multi-source
/// aggregation, and event history.
///
/// Agents need to react to changes (file modified, process exited, new files
/// created) without running a background daemon. On Linux, this module uses
/// inotify for efficient kernel-driven file/dir watching. On other platforms,
/// it falls back to stat-based polling. Multi-source aggregation (`multi`
/// command) lets agents watch files, dirs, procs, and services simultaneously,
/// returning on the first event. All watch events are logged to a JSONL
/// history file for later inspection.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::policy::{self, OpType};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const POLL_INTERVAL_MS: u64 = 500;
const SERVICE_CHECK_INTERVAL_MS: u64 = 2000;

// ---------------------------------------------------------------------------
// inotify implementation (Linux only)
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod inotify_impl {
    use std::os::unix::io::RawFd;

    /// A decoded inotify event.
    #[derive(Debug, Clone)]
    pub struct InotifyEvent {
        /// Watch descriptor that triggered.
        pub wd: i32,
        /// Bitmask of event flags.
        pub mask: u32,
        /// Filename within the watched directory (empty for direct file watches).
        pub name: String,
    }

    /// Create a non-blocking, close-on-exec inotify file descriptor.
    pub fn inotify_init() -> Result<RawFd, String> {
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if fd < 0 {
            Err("inotify_init failed".into())
        } else {
            Ok(fd)
        }
    }

    /// Add a watch for `path` with the given event `mask`. Returns a watch
    /// descriptor (wd).
    pub fn inotify_add_watch(fd: RawFd, path: &str, mask: u32) -> Result<i32, String> {
        let c_path = std::ffi::CString::new(path).map_err(|e| e.to_string())?;
        let wd = unsafe { libc::inotify_add_watch(fd, c_path.as_ptr(), mask) };
        if wd < 0 {
            Err(format!("inotify_add_watch failed for {path}"))
        } else {
            Ok(wd)
        }
    }

    /// Close an inotify file descriptor.
    pub fn inotify_close(fd: RawFd) {
        unsafe {
            libc::close(fd);
        }
    }

    /// Wait up to `timeout_ms` for events on `fd`, then read and decode them.
    /// A negative `timeout_ms` means wait indefinitely.
    pub fn read_events(fd: RawFd, timeout_ms: i32) -> Vec<InotifyEvent> {
        // Use poll() to wait for readability with a timeout.
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ret <= 0 {
            return Vec::new(); // timeout or error
        }

        // Read raw bytes from the inotify fd.
        let mut buf = [0u8; 4096];
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            return Vec::new();
        }
        let n = n as usize;

        // Decode inotify_event structs from the buffer.
        let mut events = Vec::new();
        let mut offset = 0usize;
        let event_hdr_size = std::mem::size_of::<libc::inotify_event>();
        while offset + event_hdr_size <= n {
            let ev_ptr = unsafe { &*(buf.as_ptr().add(offset) as *const libc::inotify_event) };
            let name_len = ev_ptr.len as usize;
            let name = if name_len > 0 && offset + event_hdr_size + name_len <= n {
                let name_bytes = &buf[offset + event_hdr_size..offset + event_hdr_size + name_len];
                // The name is NUL-padded.
                let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_len);
                String::from_utf8_lossy(&name_bytes[..end]).to_string()
            } else {
                String::new()
            };
            events.push(InotifyEvent {
                wd: ev_ptr.wd,
                mask: ev_ptr.mask,
                name,
            });
            offset += event_hdr_size + name_len;
        }
        events
    }

    /// Translate an inotify event mask into a human-readable event name.
    pub fn mask_to_event_name(mask: u32) -> &'static str {
        if mask & libc::IN_CREATE != 0 {
            "created"
        } else if mask & libc::IN_DELETE != 0 || mask & libc::IN_DELETE_SELF != 0 {
            "deleted"
        } else if mask & libc::IN_MODIFY != 0 {
            "modified"
        } else if mask & (libc::IN_MOVED_FROM | libc::IN_MOVED_TO) != 0 {
            "moved"
        } else if mask & libc::IN_ATTRIB != 0 {
            "attrib_changed"
        } else {
            "unknown"
        }
    }

    /// Standard mask for watching a single file (modify, delete-self, attrib).
    pub fn file_watch_mask() -> u32 {
        (libc::IN_MODIFY | libc::IN_DELETE_SELF | libc::IN_ATTRIB) as u32
    }

    /// Standard mask for watching a directory (create, delete, modify, move).
    pub fn dir_watch_mask() -> u32 {
        (libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_MODIFY
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO
            | libc::IN_ATTRIB) as u32
    }
}

// ---------------------------------------------------------------------------
// Data dir helper
// ---------------------------------------------------------------------------

fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
}

// ---------------------------------------------------------------------------
// Event history — append-only JSONL log
// ---------------------------------------------------------------------------

fn history_path() -> PathBuf {
    data_dir().join("watch").join("history.jsonl")
}

/// Append a watch event to the JSONL history log.
fn log_watch_event(source: &str, event: &Value) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut entry = event.clone();
    if let Some(obj) = entry.as_object_mut() {
        obj.insert("timestamp".into(), json!(now));
        obj.insert("source".into(), json!(source));
    }
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{}", serde_json::to_string(&entry).unwrap_or_default());
    }
}

// ---------------------------------------------------------------------------
// Public dispatch
// ---------------------------------------------------------------------------

pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;
    match command {
        "file" => cmd_watch_file(args),
        "dir" => cmd_watch_dir(args),
        "proc" => cmd_watch_proc(args),
        "on" => cmd_watch_on(args),
        "multi" => cmd_watch_multi(args),
        "history" => cmd_watch_history(args),
        _ => Err(format!("unknown watch command: {command}")),
    }
}

// ---------------------------------------------------------------------------
// FileStat — metadata snapshot for change detection
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// cmd_watch_file — inotify on Linux, polling fallback on others
// ---------------------------------------------------------------------------

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

    let result = watch_file_inotify(&path, timeout)?;
    log_watch_event("file", &result);
    Ok(result)
}

/// inotify-based file watcher (Linux).
#[cfg(target_os = "linux")]
fn watch_file_inotify(path: &PathBuf, timeout_secs: u64) -> Result<Value, String> {
    let path_str = path.to_string_lossy().to_string();
    let initial = stat_file(path);

    // If the file doesn't exist yet, watch the parent directory for creation.
    let (watch_path, watching_parent) = if !initial.exists {
        let parent = path.parent().ok_or("cannot determine parent directory")?;
        (parent.to_string_lossy().to_string(), true)
    } else {
        (path_str.clone(), false)
    };

    let fd = inotify_impl::inotify_init()?;
    let mask = if watching_parent {
        inotify_impl::dir_watch_mask()
    } else {
        inotify_impl::file_watch_mask()
    };
    let _wd = inotify_impl::inotify_add_watch(fd, &watch_path, mask)?;

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        let remaining_ms = {
            let rem = deadline.saturating_duration_since(Instant::now());
            if rem.is_zero() {
                inotify_impl::inotify_close(fd);
                return Ok(json!({
                    "status": "timeout",
                    "path": path_str,
                    "timeout_secs": timeout_secs,
                    "message": "no changes detected within timeout",
                }));
            }
            rem.as_millis() as i32
        };

        let events = inotify_impl::read_events(fd, remaining_ms.min(500));
        for ev in &events {
            if watching_parent {
                // Only care about the target filename appearing.
                let target_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if ev.name == target_name && ev.mask & libc::IN_CREATE as u32 != 0 {
                    inotify_impl::inotify_close(fd);
                    let cur = stat_file(path);
                    return Ok(json!({
                        "status": "changed",
                        "path": path_str,
                        "event": "created",
                        "size": cur.size,
                    }));
                }
            } else {
                let event_name = inotify_impl::mask_to_event_name(ev.mask);
                inotify_impl::inotify_close(fd);
                if event_name == "deleted" {
                    return Ok(json!({
                        "status": "changed",
                        "path": path_str,
                        "event": "deleted",
                    }));
                }
                let cur = stat_file(path);
                return Ok(json!({
                    "status": "changed",
                    "path": path_str,
                    "event": event_name,
                    "old_size": initial.size,
                    "new_size": cur.size,
                }));
            }
        }
    }
}

/// Polling fallback for file watching (non-Linux).
#[cfg(not(target_os = "linux"))]
fn watch_file_inotify(path: &PathBuf, timeout_secs: u64) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let initial = stat_file(path);

    loop {
        if Instant::now() >= deadline {
            return Ok(json!({
                "status": "timeout",
                "path": path.to_string_lossy(),
                "timeout_secs": timeout_secs,
                "message": "no changes detected within timeout",
            }));
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        let current = stat_file(path);

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

// ---------------------------------------------------------------------------
// cmd_watch_dir — inotify on Linux, polling fallback on others
// ---------------------------------------------------------------------------

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
    let result = watch_dir_inotify(&path, timeout)?;
    log_watch_event("dir", &result);
    Ok(result)
}

/// inotify-based directory watcher (Linux).
#[cfg(target_os = "linux")]
fn watch_dir_inotify(path: &PathBuf, timeout_secs: u64) -> Result<Value, String> {
    let path_str = path.to_string_lossy().to_string();
    let fd = inotify_impl::inotify_init()?;
    let _wd = inotify_impl::inotify_add_watch(fd, &path_str, inotify_impl::dir_watch_mask())?;

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    loop {
        let remaining_ms = {
            let rem = deadline.saturating_duration_since(Instant::now());
            if rem.is_zero() {
                inotify_impl::inotify_close(fd);
                return Ok(json!({
                    "status": "timeout",
                    "path": path_str,
                    "timeout_secs": timeout_secs,
                    "message": "no changes detected within timeout",
                }));
            }
            rem.as_millis() as i32
        };

        let events = inotify_impl::read_events(fd, remaining_ms.min(500));
        if !events.is_empty() {
            let json_events: Vec<Value> = events
                .iter()
                .map(|ev| {
                    let event_name = inotify_impl::mask_to_event_name(ev.mask);
                    if ev.name.is_empty() {
                        json!({ "event": event_name })
                    } else {
                        json!({ "event": event_name, "name": ev.name })
                    }
                })
                .collect();
            let count = json_events.len();
            inotify_impl::inotify_close(fd);
            return Ok(json!({
                "status": "changed",
                "path": path_str,
                "events": json_events,
                "count": count,
            }));
        }
    }
}

/// Polling fallback for directory watching (non-Linux).
#[cfg(not(target_os = "linux"))]
fn watch_dir_inotify(path: &PathBuf, timeout_secs: u64) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let initial = snapshot_dir(path);

    loop {
        if Instant::now() >= deadline {
            return Ok(json!({
                "status": "timeout",
                "path": path.to_string_lossy(),
                "timeout_secs": timeout_secs,
                "message": "no changes detected within timeout",
            }));
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
        let current = snapshot_dir(path);

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

// ---------------------------------------------------------------------------
// cmd_watch_proc
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// cmd_watch_multi — multi-source event aggregation
// ---------------------------------------------------------------------------

/// Watch multiple sources simultaneously — files, dirs, procs, services.
///
/// Usage: cos watch multi [--file PATH]... [--dir PATH]... [--proc SESSION]...
///        [--service NAME]... [--timeout N]
///
/// Returns when ANY source fires an event.
fn cmd_watch_multi(args: &[String]) -> Result<Value, String> {
    let files = parse_multi_flag(args, "--file");
    let dirs = parse_multi_flag(args, "--dir");
    let procs = parse_multi_flag(args, "--proc");
    let services = parse_multi_flag(args, "--service");
    let timeout = parse_timeout(args);

    if files.is_empty() && dirs.is_empty() && procs.is_empty() && services.is_empty() {
        return Err(
            "usage: cos watch multi --file PATH [--dir PATH] [--proc SESSION] \
             [--service NAME] [--timeout N]"
                .into(),
        );
    }

    let watched = json!({
        "files": files,
        "dirs": dirs,
        "procs": procs,
        "services": services,
    });

    let result = watch_multi_poll(&files, &dirs, &procs, &services, timeout)?;

    // Merge the watched info into the result.
    if let Some(obj) = result.as_object() {
        let mut out = obj.clone();
        out.insert("watched".into(), watched);
        let val = Value::Object(out);
        log_watch_event("multi", &val);
        Ok(val)
    } else {
        Ok(result)
    }
}

/// Parse a flag that can appear multiple times, collecting all its values.
fn parse_multi_flag(args: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == flag {
            if let Some(val) = args.get(i + 1) {
                values.push(val.clone());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    values
}

/// Polling-based multi-source watcher. On Linux, file/dir watches could use
/// inotify internally, but the outer loop still polls proc/service sources.
fn watch_multi_poll(
    files: &[String],
    dirs: &[String],
    procs: &[String],
    services: &[String],
    timeout_secs: u64,
) -> Result<Value, String> {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);

    // Snapshot initial file stats.
    let file_paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
    let file_initials: Vec<FileStat> = file_paths.iter().map(|p| stat_file(p)).collect();

    // Snapshot initial dir contents.
    let dir_paths: Vec<PathBuf> = dirs.iter().map(PathBuf::from).collect();
    let dir_initials: Vec<HashMap<String, FileStat>> =
        dir_paths.iter().map(|p| snapshot_dir(p)).collect();

    let mut last_service_check =
        Instant::now() - Duration::from_secs(SERVICE_CHECK_INTERVAL_MS / 1000 + 1);

    loop {
        if Instant::now() >= deadline {
            return Ok(json!({
                "status": "timeout",
                "timeout_secs": timeout_secs,
                "message": "no events detected within timeout",
            }));
        }

        // --- Check files ---
        for (idx, path) in file_paths.iter().enumerate() {
            let current = stat_file(path);
            let initial = &file_initials[idx];

            if !initial.exists && current.exists {
                return Ok(json!({
                    "status": "triggered",
                    "source": "file",
                    "path": path.to_string_lossy(),
                    "event": "created",
                }));
            }
            if initial.exists && !current.exists {
                return Ok(json!({
                    "status": "triggered",
                    "source": "file",
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
                        "status": "triggered",
                        "source": "file",
                        "path": path.to_string_lossy(),
                        "event": "modified",
                    }));
                }
            }
        }

        // --- Check dirs ---
        for (idx, path) in dir_paths.iter().enumerate() {
            let current = snapshot_dir(path);
            let initial = &dir_initials[idx];

            for name in current.keys() {
                if !initial.contains_key(name) {
                    return Ok(json!({
                        "status": "triggered",
                        "source": "dir",
                        "path": path.to_string_lossy(),
                        "event": "created",
                        "name": name,
                    }));
                }
            }
            for name in initial.keys() {
                if !current.contains_key(name) {
                    return Ok(json!({
                        "status": "triggered",
                        "source": "dir",
                        "path": path.to_string_lossy(),
                        "event": "deleted",
                        "name": name,
                    }));
                }
            }
            for (name, curr_stat) in &current {
                if let Some(init_stat) = initial.get(name) {
                    let size_changed = init_stat.size != curr_stat.size;
                    let time_changed = match (init_stat.modified, curr_stat.modified) {
                        (Some(a), Some(b)) => a != b,
                        _ => false,
                    };
                    if size_changed || time_changed {
                        return Ok(json!({
                            "status": "triggered",
                            "source": "dir",
                            "path": path.to_string_lossy(),
                            "event": "modified",
                            "name": name,
                        }));
                    }
                }
            }
        }

        // --- Check procs (every poll cycle) ---
        for proc_session in procs {
            let status_result = crate::proc::run("status", &[proc_session.clone()]);
            match status_result {
                Ok(v) => {
                    let st = v["status"].as_str().unwrap_or("");
                    if st == "exited" || st == "not_found" {
                        return Ok(json!({
                            "status": "triggered",
                            "source": "proc",
                            "session": proc_session,
                            "event": "exited",
                        }));
                    }
                }
                Err(_) => {
                    return Ok(json!({
                        "status": "triggered",
                        "source": "proc",
                        "session": proc_session,
                        "event": "exited",
                    }));
                }
            }
        }

        // --- Check services (every SERVICE_CHECK_INTERVAL_MS) ---
        if !services.is_empty()
            && last_service_check.elapsed() >= Duration::from_millis(SERVICE_CHECK_INTERVAL_MS)
        {
            last_service_check = Instant::now();
            for svc in services {
                let health = crate::service::run("health", &[svc.clone(), "--no-restart".into()]);
                match health {
                    Ok(v) => {
                        if v["healthy"] == false {
                            return Ok(json!({
                                "status": "triggered",
                                "source": "service",
                                "service": svc,
                                "event": "health-fail",
                            }));
                        }
                    }
                    Err(e) => {
                        return Ok(json!({
                            "status": "triggered",
                            "source": "service",
                            "service": svc,
                            "event": "health-fail",
                            "error": e,
                        }));
                    }
                }
            }
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

// ---------------------------------------------------------------------------
// cmd_watch_history — read and filter event history
// ---------------------------------------------------------------------------

/// View past watch events from the JSONL history log.
///
/// Usage: cos watch history [--limit N] [--since TIMESTAMP] [--source TYPE]
fn cmd_watch_history(args: &[String]) -> Result<Value, String> {
    let limit: usize = parse_flag(args, "--limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let since = parse_flag(args, "--since").map(|s| s.to_string());
    let source_filter = parse_flag(args, "--source").map(|s| s.to_string());

    let path = history_path();
    let entries = if path.exists() {
        let content = fs::read_to_string(&path).map_err(|e| format!("read history: {e}"))?;
        let mut events: Vec<Value> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();

        // Filter by --since
        if let Some(ref since_ts) = since {
            events.retain(|ev| {
                ev["timestamp"]
                    .as_str()
                    .map(|ts| ts >= since_ts.as_str())
                    .unwrap_or(false)
            });
        }

        // Filter by --source
        if let Some(ref src) = source_filter {
            events.retain(|ev| ev["source"].as_str() == Some(src.as_str()));
        }

        // Take last N entries.
        let skip = events.len().saturating_sub(limit);
        events.into_iter().skip(skip).collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let count = entries.len();
    Ok(json!({
        "events": entries,
        "count": count,
    }))
}

// ---------------------------------------------------------------------------
// Helpers: snapshot, parse
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// cmd_watch_on — unified OS event watcher
// ---------------------------------------------------------------------------

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
///   ipc.message --session <id>       — wait for a new IPC message to arrive
///   credential.expired --name <name> — wait for a credential to expire
fn cmd_watch_on(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("usage: cos watch on <event-type> [--timeout N] [...]\n\
             event types: proc.exit, fs.change, service.health-fail, checkpoint.created, \
             quota.exceeded, ipc.message, credential.expired"
            .into());
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
        "ipc.message" => watch_ipc_message(&rest, timeout),
        "credential.expired" => watch_credential_expired(&rest, timeout),
        _ => Err(format!(
            "unknown event type: {event_type}. \
             supported: proc.exit, fs.change, service.health-fail, checkpoint.created, \
             quota.exceeded, ipc.message, credential.expired"
        )),
    }
}

// ---------------------------------------------------------------------------
// Event watchers (on sub-commands)
// ---------------------------------------------------------------------------

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

/// Wait for a new IPC message to arrive for a session.
///
/// Usage: cos watch on ipc.message --session <id> [--timeout N]
///
/// Polls the IPC queue directory, returning when a new message appears.
fn watch_ipc_message(args: &[String], timeout: u64) -> Result<Value, String> {
    let session_id =
        parse_flag(args, "--session").ok_or("--session <id> required for ipc.message")?;

    let queue_dir = data_dir().join("ipc").join(session_id);

    // Snapshot current message count.
    let initial_count = count_ipc_messages(&queue_dir);

    let deadline = Instant::now() + Duration::from_secs(timeout);

    loop {
        let current_count = count_ipc_messages(&queue_dir);
        if current_count > initial_count {
            return Ok(json!({
                "event": "ipc.message",
                "triggered": true,
                "session": session_id,
                "previous_count": initial_count,
                "current_count": current_count,
            }));
        }

        if Instant::now() >= deadline {
            return Ok(json!({
                "event": "ipc.message",
                "triggered": false,
                "session": session_id,
                "status": "timeout",
                "message_count": current_count,
            }));
        }

        thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));
    }
}

/// Count `.json` message files in an IPC queue directory.
fn count_ipc_messages(dir: &PathBuf) -> usize {
    if !dir.exists() {
        return 0;
    }
    fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .map(|n| n.ends_with(".json"))
                        .unwrap_or(false)
                })
                .count()
        })
        .unwrap_or(0)
}

/// Wait for a credential to expire (cease to exist or become unloadable).
///
/// Usage: cos watch on credential.expired --name <name> [--timeout N]
///
/// Polls the credential store, returning when the named credential no longer
/// exists or can no longer be loaded.
fn watch_credential_expired(args: &[String], timeout: u64) -> Result<Value, String> {
    let cred_name =
        parse_flag(args, "--name").ok_or("--name <name> required for credential.expired")?;

    let cred_file = data_dir()
        .join("credentials")
        .join(format!("{cred_name}.json"));

    // Only meaningful if the credential currently exists.
    if !cred_file.exists() {
        return Ok(json!({
            "event": "credential.expired",
            "triggered": true,
            "name": cred_name,
            "reason": "credential does not exist",
        }));
    }

    let deadline = Instant::now() + Duration::from_secs(timeout);

    loop {
        if !cred_file.exists() {
            return Ok(json!({
                "event": "credential.expired",
                "triggered": true,
                "name": cred_name,
                "reason": "credential removed",
            }));
        }

        if Instant::now() >= deadline {
            return Ok(json!({
                "event": "credential.expired",
                "triggered": false,
                "name": cred_name,
                "status": "timeout",
            }));
        }

        thread::sleep(Duration::from_secs(1));
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU32, Ordering};

    static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

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
        // History with no events should return empty list.
        let dir = unique_test_dir("hist-empty");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("COS_DATA_DIR", dir.to_string_lossy().to_string());

        let result = cmd_watch_history(&[]);
        let val = result.unwrap();
        assert_eq!(val["count"], 0);
        assert!(val["events"].as_array().unwrap().is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_watch_history_write_and_read() {
        // Manually write a history entry and verify it can be read back.
        let dir = unique_test_dir("hist-rw");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("watch")).unwrap();
        std::env::set_var("COS_DATA_DIR", dir.to_string_lossy().to_string());

        let hist_file = dir.join("watch").join("history.jsonl");
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
                json!({"timestamp": "2026-03-25T10:00:00Z", "source": "file", "path": "/den/main.py", "event": "modified"})
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
        assert_eq!(result["events"][0]["path"], "/den/main.py");

        // Filter by --since.
        let result = cmd_watch_history(&["--since".into(), "2026-03-25T10:00:03Z".into()]).unwrap();
        assert_eq!(result["count"], 1);
        assert_eq!(result["events"][0]["source"], "proc");

        // Limit.
        let result = cmd_watch_history(&["--limit".into(), "1".into()]).unwrap();
        assert_eq!(result["count"], 1);
        // Should be the last entry.
        assert_eq!(result["events"][0]["source"], "proc");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_log_watch_event() {
        // Verify log_watch_event appends to the history file.
        let dir = unique_test_dir("log-event");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        std::env::set_var("COS_DATA_DIR", dir.to_string_lossy().to_string());

        let event = json!({"path": "/den/test.rs", "event": "created"});
        log_watch_event("file", &event);

        let hist_file = dir.join("watch").join("history.jsonl");
        assert!(hist_file.exists(), "history file should be created");

        let content = fs::read_to_string(&hist_file).unwrap();
        let parsed: Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["source"], "file");
        assert_eq!(parsed["path"], "/den/test.rs");
        assert_eq!(parsed["event"], "created");
        assert!(
            parsed["timestamp"].as_str().is_some(),
            "should have timestamp"
        );

        let _ = fs::remove_dir_all(&dir);
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
}
