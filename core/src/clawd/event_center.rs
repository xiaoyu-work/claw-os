use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::os::fd::{FromRawFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const MAX_EVENT_LINE: usize = 1024 * 1024;
const MAX_EVENT_DATA_TEXT: usize = 4096;
const MAX_EVENT_LOG_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PID_WATCHES: usize = 256;
const RESTART_DELAY: Duration = Duration::from_secs(10);
static EVENT_CENTER: OnceLock<Arc<EventCenter>> = OnceLock::new();
static EVENT_FILE_LOCK: OnceLock<StdMutex<()>> = OnceLock::new();

#[derive(Clone)]
pub struct EventCenter {
    sender: broadcast::Sender<Value>,
    statuses: Arc<RwLock<BTreeMap<String, Value>>>,
    pid_watches: Arc<Mutex<BTreeMap<(u32, u64), String>>>,
}

pub fn start() -> Arc<EventCenter> {
    EVENT_CENTER
        .get_or_init(|| {
            let (sender, _) = broadcast::channel(1024);
            let center = Arc::new(EventCenter {
                sender,
                statuses: Arc::new(RwLock::new(BTreeMap::new())),
                pid_watches: Arc::new(Mutex::new(BTreeMap::new())),
            });
            tokio::spawn(run_udev(center.clone()));
            tokio::spawn(run_systemd_monitor(center.clone()));
            tokio::spawn(run_journal(center.clone()));
            center
        })
        .clone()
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Event Center requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Event Center requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let source = optional_string(&params, "source")?;
        let limit = optional_u64(&params, "limit")?.unwrap_or(100);
        let pid = optional_u64(&params, "pid")?;
        validate_action(&action, source.as_deref(), limit, pid)?;
        crate::paths::with_user_override(uid, home, async {
            authorize_session(&session_id, peer_pid)
        })
        .await?;
        let center = start();
        match action.as_str() {
            "status" => center.status().await,
            "recent" => {
                let source = source.clone();
                tokio::task::spawn_blocking(move || {
                    recent_events(source.as_deref(), limit as usize)
                })
                .await
                .map_err(|error| format!("Event Center query worker failed: {error}"))?
            }
            "watch-pid" => center.watch_pid(pid.unwrap() as u32).await,
            _ => unreachable!("validated event action"),
        }
    }
}

fn authorize_session(session_id: &str, peer_pid: u32) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("event-center session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("event-center") {
        return Err("event subscriptions are restricted to the event-center App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("event-center session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "event-center session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("event-center session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("event request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    let requested = Cap::new(Verb::SYS_EVENTS, Scope::name("observe"));
    if !caps.covers(&requested) {
        return Err("event-center session lacks sys.events:observe".to_string());
    }
    Ok(())
}

impl EventCenter {
    async fn status(&self) -> Result<Value, String> {
        let statuses = self.statuses.read().await.clone();
        let watches = self.pid_watches.lock().await;
        Ok(json!({
            "sources": statuses,
            "active_pid_watches": watches.len(),
            "event_log": event_log_path(),
            "subscribers": self.sender.receiver_count(),
        }))
    }

    async fn watch_pid(self: &Arc<Self>, pid: u32) -> Result<Value, String> {
        if pid == 0 {
            return Err("pid must be positive".to_string());
        }
        let start_time = crate::proc::read_start_time_ticks_pub(pid)
            .ok_or_else(|| format!("process not found: {pid}"))?;
        let key = (pid, start_time);
        let watch_id = uuid::Uuid::new_v4().simple().to_string();
        {
            let mut watches = self.pid_watches.lock().await;
            if let Some(id) = watches.get(&key) {
                return Ok(json!({
                    "watch_id": id,
                    "pid": pid,
                    "start_time_ticks": start_time,
                    "already_watching": true,
                }));
            }
            if watches.len() >= MAX_PID_WATCHES {
                return Err(format!("pidfd watch limit reached: {MAX_PID_WATCHES}"));
            }
            watches.insert(key, watch_id.clone());
        }
        let fd = match pidfd_open(pid) {
            Ok(fd) => fd,
            Err(error) => {
                self.pid_watches.lock().await.remove(&key);
                return Err(error);
            }
        };
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let pidfd = match AsyncFd::new(file) {
            Ok(pidfd) => pidfd,
            Err(error) => {
                self.pid_watches.lock().await.remove(&key);
                return Err(format!("register pidfd: {error}"));
            }
        };
        let snapshot = process_snapshot(pid, start_time);
        let center = self.clone();
        let task_watch_id = watch_id.clone();
        tokio::spawn(async move {
            let result = pidfd.readable().await;
            let data = match result {
                Ok(_) => json!({
                    "watch_id": task_watch_id,
                    "pid": pid,
                    "start_time_ticks": start_time,
                    "process": snapshot,
                }),
                Err(error) => json!({
                    "watch_id": task_watch_id,
                    "pid": pid,
                    "start_time_ticks": start_time,
                    "error": error.to_string(),
                }),
            };
            center.emit("process", "process.exit", data).await;
            center.pid_watches.lock().await.remove(&key);
        });
        Ok(json!({
            "watch_id": watch_id,
            "pid": pid,
            "start_time_ticks": start_time,
            "already_watching": false,
        }))
    }

    async fn set_status(&self, source: &str, status: &str, detail: Option<String>) {
        self.statuses.write().await.insert(
            source.to_string(),
            json!({
                "status": status,
                "detail": detail,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }

    async fn emit(&self, source: &str, kind: &str, data: Value) {
        let record = json!({
            "id": uuid::Uuid::new_v4().simple().to_string(),
            "ts": chrono::Utc::now().to_rfc3339(),
            "source": source,
            "kind": kind,
            "data": data,
        });
        let _ = self.sender.send(record.clone());
        if let Err(error) = persist_event(record).await {
            tracing::error!(error = %error, "failed to persist Event Center record");
        }
    }
}

async fn run_udev(center: Arc<EventCenter>) {
    loop {
        center.set_status("udev", "starting", None).await;
        let result = monitor_udev(center.clone()).await;
        center.set_status("udev", "error", result.err()).await;
        tokio::time::sleep(RESTART_DELAY).await;
    }
}

async fn monitor_udev(center: Arc<EventCenter>) -> Result<(), String> {
    let udevadm = tool_path(&["/usr/bin/udevadm", "/bin/udevadm"])
        .ok_or_else(|| "udevadm is not installed".to_string())?;
    let mut child = monitor_command(udevadm, &["monitor", "--udev", "--property"])?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "udevadm stdout is unavailable".to_string())?;
    drain_stderr(child.stderr.take());
    center.set_status("udev", "running", None).await;
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let mut fields = serde_json::Map::new();
    loop {
        let Some(truncated) = read_bounded_line(&mut reader, &mut line)
            .await
            .map_err(|error| format!("read udev monitor: {error}"))?
        else {
            flush_udev(&center, &mut fields).await;
            break;
        };
        if truncated {
            fields.clear();
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.is_empty() {
            flush_udev(&center, &mut fields).await;
        } else if let Some((key, value)) = text.split_once('=') {
            fields.insert(
                key.to_ascii_lowercase(),
                Value::String(truncate_text(value, MAX_EVENT_DATA_TEXT)),
            );
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("wait for udevadm: {error}"))?;
    Err(format!("udevadm exited {status}"))
}

async fn flush_udev(center: &EventCenter, fields: &mut serde_json::Map<String, Value>) {
    if fields.is_empty() {
        return;
    }
    let action = fields
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("change")
        .to_string();
    let subsystem = fields
        .get("subsystem")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let source = if matches!(subsystem.as_str(), "block" | "scsi" | "nvme") {
        "storage"
    } else {
        "udev"
    };
    let data = Value::Object(std::mem::take(fields));
    center.emit(source, &format!("udev.{action}"), data).await;
}

async fn run_systemd_monitor(center: Arc<EventCenter>) {
    loop {
        center.set_status("systemd-dbus", "starting", None).await;
        let result = monitor_systemd(center.clone()).await;
        center
            .set_status("systemd-dbus", "error", result.err())
            .await;
        tokio::time::sleep(RESTART_DELAY).await;
    }
}

async fn monitor_systemd(center: Arc<EventCenter>) -> Result<(), String> {
    let busctl = tool_path(&["/usr/bin/busctl", "/bin/busctl"])
        .ok_or_else(|| "busctl is not installed".to_string())?;
    let mut child = monitor_command(busctl, &["monitor", "org.freedesktop.systemd1"])?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "busctl monitor stdout is unavailable".to_string())?;
    drain_stderr(child.stderr.take());
    center.set_status("systemd-dbus", "running", None).await;
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    let mut fields = serde_json::Map::new();
    let mut in_header = false;
    loop {
        let Some(truncated) = read_bounded_line(&mut reader, &mut line)
            .await
            .map_err(|error| format!("read busctl monitor: {error}"))?
        else {
            flush_systemd(&center, &mut fields).await;
            break;
        };
        if truncated {
            fields.clear();
            continue;
        }
        let text = String::from_utf8_lossy(&line);
        let text = text.trim();
        if text.is_empty() || text.starts_with('‣') {
            flush_systemd(&center, &mut fields).await;
            in_header = text.starts_with('‣');
        }
        if !in_header {
            continue;
        }
        for key in [
            "Type",
            "Path",
            "Interface",
            "Member",
            "Sender",
            "Destination",
        ] {
            let marker = format!("{key}=");
            if let Some(value) = text
                .split_whitespace()
                .find_map(|token| token.strip_prefix(&marker))
                .filter(|value| !value.is_empty())
            {
                fields.insert(
                    key.to_ascii_lowercase(),
                    Value::String(truncate_text(value, MAX_EVENT_DATA_TEXT)),
                );
            }
        }
        if fields.contains_key("member") {
            in_header = false;
        }
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("wait for busctl monitor: {error}"))?;
    Err(format!("busctl monitor exited {status}"))
}

async fn flush_systemd(center: &EventCenter, fields: &mut serde_json::Map<String, Value>) {
    if !fields
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|value| value.starts_with("signal"))
    {
        fields.clear();
        return;
    }
    let member = fields
        .get("member")
        .and_then(Value::as_str)
        .unwrap_or("signal")
        .to_string();
    let data = Value::Object(std::mem::take(fields));
    center
        .emit("systemd", &format!("systemd.{member}"), data)
        .await;
}

async fn run_journal(center: Arc<EventCenter>) {
    loop {
        center.set_status("journal", "starting", None).await;
        let result = monitor_journal(center.clone()).await;
        center.set_status("journal", "error", result.err()).await;
        tokio::time::sleep(RESTART_DELAY).await;
    }
}

async fn monitor_journal(center: Arc<EventCenter>) -> Result<(), String> {
    let journalctl = tool_path(&["/usr/bin/journalctl", "/bin/journalctl"])
        .ok_or_else(|| "journalctl is not installed".to_string())?;
    let mut child = monitor_command(
        journalctl,
        &[
            "--follow",
            "--no-pager",
            "--quiet",
            "--since=now",
            "--output=json",
            "--output-fields=__REALTIME_TIMESTAMP,_BOOT_ID,PRIORITY,SYSLOG_IDENTIFIER,_COMM,_EXE,_PID,_UID,_SYSTEMD_UNIT,MESSAGE",
        ],
    )?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "journalctl stdout is unavailable".to_string())?;
    drain_stderr(child.stderr.take());
    center.set_status("journal", "running", None).await;
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        let Some(truncated) = read_bounded_line(&mut reader, &mut line)
            .await
            .map_err(|error| format!("read journal follow: {error}"))?
        else {
            break;
        };
        if truncated {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let Some((source, kind, data)) = classify_journal(value) else {
            continue;
        };
        center.emit(source, kind, data).await;
    }
    let status = child
        .wait()
        .await
        .map_err(|error| format!("wait for journalctl: {error}"))?;
    Err(format!("journalctl exited {status}"))
}

fn classify_journal(value: Value) -> Option<(&'static str, &'static str, Value)> {
    let message = value["MESSAGE"].as_str()?;
    let lower = message.to_ascii_lowercase();
    let (source, kind) = if lower.contains("apparmor=\"denied\"")
        || lower.contains("avc:  denied")
        || lower.contains("authentication failure")
        || lower.contains("failed password")
        || lower.contains("invalid user")
        || lower.contains("module verification failed")
        || lower.contains("lockdown:")
    {
        ("security", "security.event")
    } else if [
        "i/o error",
        "buffer i/o",
        "blk_update_request",
        "critical medium error",
        "nvme timeout",
        "ext4-fs error",
        "xfs error",
        "btrfs error",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        ("storage", "storage.error")
    } else if value["PRIORITY"]
        .as_str()
        .and_then(|priority| priority.parse::<u8>().ok())
        .is_some_and(|priority| priority <= 3)
    {
        ("journal", "journal.error")
    } else {
        return None;
    };
    Some((
        source,
        kind,
        json!({
            "timestamp_us": value["__REALTIME_TIMESTAMP"],
            "boot_id": value["_BOOT_ID"],
            "priority": value["PRIORITY"],
            "identifier": value["SYSLOG_IDENTIFIER"].as_str().or_else(|| value["_COMM"].as_str()),
            "exe": value["_EXE"],
            "pid": value["_PID"],
            "uid": value["_UID"],
            "unit": value["_SYSTEMD_UNIT"],
            "message": truncate_text(message, MAX_EVENT_DATA_TEXT),
        }),
    ))
}

fn monitor_command(program: &'static str, args: &[&str]) -> Result<tokio::process::Child, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LC_ALL", "C.UTF-8")
        .env("SYSTEMD_PAGER", "cat")
        .env("PAGER", "cat")
        .kill_on_drop(true)
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .spawn()
        .map_err(|error| format!("failed to launch {program}: {error}"))
}

fn drain_stderr(stderr: Option<tokio::process::ChildStderr>) {
    if let Some(stderr) = stderr {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = Vec::new();
            loop {
                match read_bounded_line(&mut reader, &mut line).await {
                    Ok(None) | Err(_) => break,
                    Ok(Some(_)) => {}
                }
            }
        });
    }
}

async fn read_bounded_line<R>(reader: &mut R, output: &mut Vec<u8>) -> std::io::Result<Option<bool>>
where
    R: AsyncBufRead + Unpin,
{
    output.clear();
    let mut truncated = false;
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Ok(Some(truncated))
            };
        }
        let newline = buffer.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(buffer.len(), |index| index + 1);
        let payload_len = newline.unwrap_or(buffer.len());
        let remaining = MAX_EVENT_LINE.saturating_sub(output.len());
        let keep = remaining.min(payload_len);
        output.extend_from_slice(&buffer[..keep]);
        truncated |= keep < payload_len;
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(truncated));
        }
    }
}

fn pidfd_open(pid: u32) -> Result<RawFd, String> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        return Err(format!(
            "pidfd_open({pid}) failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(fd as RawFd)
}

fn process_snapshot(pid: u32, start_time: u64) -> Value {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let fields = status
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| matches!(*key, "Name" | "Uid" | "Gid" | "State"))
        .map(|(key, value)| {
            (
                key.to_ascii_lowercase(),
                Value::String(value.trim().to_string()),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let cmdline = fs::read(format!("/proc/{pid}/cmdline")).ok().map(|value| {
        truncate_text(
            &String::from_utf8_lossy(&value).replace('\0', " "),
            MAX_EVENT_DATA_TEXT,
        )
    });
    json!({
        "pid": pid,
        "start_time_ticks": start_time,
        "status": fields,
        "cmdline": cmdline,
    })
}

async fn persist_event(record: Value) -> Result<(), String> {
    tokio::task::spawn_blocking(move || persist_event_sync(record))
        .await
        .map_err(|error| format!("Event Center persistence worker failed: {error}"))?
}

fn persist_event_sync(record: Value) -> Result<(), String> {
    let _guard = EVENT_FILE_LOCK
        .get_or_init(|| StdMutex::new(()))
        .lock()
        .map_err(|_| "Event Center file lock is poisoned".to_string())?;
    let path = event_log_path();
    if fs::metadata(&path)
        .ok()
        .is_some_and(|metadata| metadata.len() > MAX_EVENT_LOG_BYTES)
    {
        let rotated = path.with_extension("jsonl.1");
        if rotated.exists() {
            fs::remove_file(&rotated)
                .map_err(|error| format!("remove rotated Event Center log: {error}"))?;
        }
        fs::rename(&path, &rotated).map_err(|error| format!("rotate Event Center log: {error}"))?;
    }
    let line = serde_json::to_string(&record)
        .map_err(|error| format!("serialize Event Center record: {error}"))?;
    crate::filelock::append_locked(&path, &line)
        .map_err(|error| format!("append Event Center log {}: {error}", path.display()))
}

fn recent_events(source: Option<&str>, limit: usize) -> Result<Value, String> {
    let path = event_log_path();
    let data = match fs::read_to_string(&path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("read Event Center log {}: {error}", path.display())),
    };
    let mut events = Vec::new();
    for line in data.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if source.is_some_and(|source| value["source"].as_str() != Some(source)) {
            continue;
        }
        events.push(value);
        if events.len() == limit {
            break;
        }
    }
    let count = events.len();
    Ok(json!({
        "source": source,
        "limit": limit,
        "events": events,
        "count": count,
        "path": path,
    }))
}

fn event_log_path() -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("event-center.jsonl")
}

fn validate_action(
    action: &str,
    source: Option<&str>,
    limit: u64,
    pid: Option<u64>,
) -> Result<(), String> {
    if !(1..=1000).contains(&limit) {
        return Err("event limit must be 1..1000".to_string());
    }
    match action {
        "status" if source.is_none() && pid.is_none() && limit == 100 => Ok(()),
        "recent"
            if pid.is_none()
                && source
                    .map(|source| {
                        matches!(
                            source,
                            "udev" | "systemd" | "journal" | "storage" | "security" | "process"
                        )
                    })
                    .unwrap_or(true) =>
        {
            Ok(())
        }
        "watch-pid"
            if source.is_none()
                && pid.is_some_and(|pid| pid > 0 && pid <= u32::MAX as u64)
                && limit == 100 =>
        {
            Ok(())
        }
        "status" => Err("status does not accept source or pid".to_string()),
        "recent" => Err("recent accepts an optional known source and limit".to_string()),
        "watch-pid" => Err("watch-pid requires one positive pid".to_string()),
        _ => Err(format!("unknown Event Center action: {action}")),
    }
}

fn optional_u64(params: &Value, key: &str) -> Result<Option<u64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("parameter `{key}` must be a non-negative integer")),
        Some(Value::String(value)) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("parameter `{key}` must be an integer")),
        Some(_) => Err(format!("parameter `{key}` must be an integer or null")),
    }
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(format!("parameter `{key}` must be a string or null")),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key)?.ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn tool_path(candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
}

fn truncate_text(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.to_string();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_classifier_finds_storage_errors() {
        let value = json!({
            "MESSAGE": "nvme timeout on nvme0",
            "PRIORITY": "3",
        });
        let (source, kind, _) = classify_journal(value).unwrap();
        assert_eq!(source, "storage");
        assert_eq!(kind, "storage.error");
    }

    #[test]
    fn event_sources_are_bounded() {
        validate_action("recent", Some("security"), 10, None).unwrap();
        assert!(validate_action("recent", Some("*"), 10, None).is_err());
    }
}
