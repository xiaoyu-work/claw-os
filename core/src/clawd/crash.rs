use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const CRASH_SCOPE: &str = "system";
const COREDUMP_MESSAGE_ID: &str = "fc2e22bc6ee647b6b90729ab34a250b1";
const COREDUMP_FIELDS: &str = "__REALTIME_TIMESTAMP,_BOOT_ID,COREDUMP_TIMESTAMP,COREDUMP_PID,COREDUMP_UID,COREDUMP_GID,COREDUMP_SIGNAL,COREDUMP_SIGNAL_NAME,COREDUMP_COMM,COREDUMP_EXE,COREDUMP_CMDLINE,COREDUMP_UNIT,COREDUMP_USER_UNIT,COREDUMP_FILENAME,COREDUMP_TRUNCATED,COREDUMP_SIZE,COREDUMP_PACKAGE_NAME,COREDUMP_PACKAGE_VERSION";
const EVENT_FIELDS: &str = "__REALTIME_TIMESTAMP,_BOOT_ID,_PID,_UID,_SYSTEMD_UNIT,_SYSTEMD_USER_UNIT,PRIORITY,SYSLOG_IDENTIFIER,_COMM,_EXE,MESSAGE,MESSAGE_ID,COREDUMP_PID,COREDUMP_UNIT,COREDUMP_USER_UNIT";
const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const DEBUGGER_TIMEOUT: Duration = Duration::from_secs(90);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
const MAX_CORE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_SINCE_MINUTES: u64 = 7 * 24 * 60;
const MAX_LIMIT: u64 = 100;
static CRASH_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn inspect(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Crash Doctor requires Linux systemd".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Crash Doctor requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;

        crate::paths::with_user_override(uid, home, async {
            authorize_session(&session_id, peer_pid)
        })
        .await?;

        let _guard = CRASH_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        match action.as_str() {
            "recent" => {
                let (since_minutes, limit) = query_bounds(&params)?;
                let coredumps = recent_coredumps(since_minutes, limit).await?;
                let count = coredumps.len();
                Ok(json!({
                    "window_minutes": since_minutes,
                    "coredumps": coredumps,
                    "count": count,
                }))
            }
            "diagnose" => {
                let (since_minutes, limit) = query_bounds(&params)?;
                diagnose(since_minutes, limit).await
            }
            "backtrace" => {
                reject_query_bounds(&params)?;
                let id = required_string(&params, "id")?;
                backtrace(&id).await
            }
            other => Err(format!(
                "unknown Crash Doctor action `{other}`; expected recent, diagnose, or backtrace"
            )),
        }
    }
}

fn authorize_session(session_id: &str, peer_pid: u32) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("crash-doctor session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("crash-doctor") {
        return Err("crash inspection is restricted to the crash-doctor App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("crash-doctor session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "crash-doctor session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("crash-doctor session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("crash request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    let requested = Cap::new(Verb::SYS_CRASH, Scope::name(CRASH_SCOPE));
    if !caps.covers(&requested) {
        return Err(format!(
            "crash-doctor session lacks {}:{}",
            Verb::SYS_CRASH.as_str(),
            CRASH_SCOPE
        ));
    }
    Ok(())
}

fn query_bounds(params: &Value) -> Result<(u64, u64), String> {
    let since_minutes = optional_u64(params, "since_minutes")?.unwrap_or(60);
    let limit = optional_u64(params, "limit")?.unwrap_or(20);
    if !(1..=MAX_SINCE_MINUTES).contains(&since_minutes) {
        return Err(format!(
            "since_minutes must be between 1 and {MAX_SINCE_MINUTES}"
        ));
    }
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(format!("limit must be between 1 and {MAX_LIMIT}"));
    }
    if params.get("id").is_some_and(|value| !value.is_null()) {
        return Err("recent and diagnose do not accept id".to_string());
    }
    Ok((since_minutes, limit))
}

fn reject_query_bounds(params: &Value) -> Result<(), String> {
    if ["since_minutes", "limit"]
        .iter()
        .any(|key| params.get(*key).is_some_and(|value| !value.is_null()))
    {
        return Err("backtrace does not accept since_minutes or limit".to_string());
    }
    Ok(())
}

async fn diagnose(since_minutes: u64, limit: u64) -> Result<Value, String> {
    let coredumps = recent_coredumps(since_minutes, limit).await?;
    let events = recent_crash_events(since_minutes, limit).await?;
    let correlations = correlate(&coredumps, &events);
    let findings = findings(&coredumps, &events);
    let status = if findings
        .iter()
        .any(|finding| finding["severity"].as_str() == Some("critical"))
    {
        "critical"
    } else if findings.is_empty() {
        "ok"
    } else {
        "warning"
    };
    let recommendations = findings
        .iter()
        .filter_map(|finding| finding["recommendation"].as_str())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    Ok(json!({
        "schema": 1,
        "status": status,
        "window_minutes": since_minutes,
        "summary": format!(
            "{} coredump(s), {} crash-related journal event(s), {} finding(s)",
            coredumps.len(),
            events.len(),
            findings.len(),
        ),
        "coredumps": coredumps,
        "events": events,
        "correlations": correlations,
        "findings": findings,
        "recommendations": recommendations,
    }))
}

async fn recent_coredumps(since_minutes: u64, limit: u64) -> Result<Vec<Value>, String> {
    let since = format!("-{since_minutes}min");
    let limit = limit.to_string();
    let args = vec![
        "--no-pager".to_string(),
        "--quiet".to_string(),
        "--output=json".to_string(),
        format!("--output-fields={COREDUMP_FIELDS}"),
        "--reverse".to_string(),
        "--since".to_string(),
        since,
        "-n".to_string(),
        limit,
        format!("MESSAGE_ID={COREDUMP_MESSAGE_ID}"),
    ];
    let output = run_checked(
        journalctl_path(),
        args,
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    Ok(parse_json_records(&output.stdout)
        .into_iter()
        .filter_map(|record| normalize_coredump(&record))
        .collect())
}

async fn coredump_by_id(boot_id: &str, pid: u32, crash_timestamp_us: u64) -> Result<Value, String> {
    let args = vec![
        "--no-pager".to_string(),
        "--quiet".to_string(),
        "--output=json".to_string(),
        format!("--output-fields={COREDUMP_FIELDS}"),
        "--reverse".to_string(),
        "-n".to_string(),
        "2".to_string(),
        format!("MESSAGE_ID={COREDUMP_MESSAGE_ID}"),
        format!("_BOOT_ID={boot_id}"),
        format!("COREDUMP_PID={pid}"),
        format!("COREDUMP_TIMESTAMP={crash_timestamp_us}"),
    ];
    let output = run_checked(
        journalctl_path(),
        args,
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    let matches = parse_json_records(&output.stdout)
        .into_iter()
        .filter_map(|record| normalize_coredump(&record))
        .filter(|record| {
            record["boot_id"].as_str() == Some(boot_id)
                && record["pid"].as_u64() == Some(pid as u64)
                && record["crash_timestamp_us"].as_u64() == Some(crash_timestamp_us)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [record] => Ok(record.clone()),
        [] => Err(format!(
            "coredump not found: {boot_id}:{pid}:{crash_timestamp_us}"
        )),
        _ => Err(format!(
            "coredump selector is ambiguous: {boot_id}:{pid}:{crash_timestamp_us}"
        )),
    }
}

fn normalize_coredump(record: &Value) -> Option<Value> {
    let boot_id = field_string(record, &["_BOOT_ID", "boot_id"])?;
    let pid = field_u64(record, &["COREDUMP_PID", "pid"])?;
    let crash_timestamp_us = field_u64(record, &["COREDUMP_TIMESTAMP", "crash_timestamp_us"])?;
    if boot_id.len() != 32
        || !boot_id.bytes().all(|byte| byte.is_ascii_hexdigit())
        || pid == 0
        || pid > u32::MAX as u64
    {
        return None;
    }
    Some(json!({
        "id": format!(
            "{}:{}:{}",
            boot_id.to_ascii_lowercase(),
            pid,
            crash_timestamp_us,
        ),
        "timestamp_us": crash_timestamp_us,
        "journal_timestamp_us": field_u64(record, &["__REALTIME_TIMESTAMP", "journal_timestamp_us"]),
        "crash_timestamp_us": crash_timestamp_us,
        "boot_id": boot_id.to_ascii_lowercase(),
        "pid": pid,
        "uid": field_u64(record, &["COREDUMP_UID", "uid"]),
        "gid": field_u64(record, &["COREDUMP_GID", "gid"]),
        "signal": field_string(record, &["COREDUMP_SIGNAL_NAME", "COREDUMP_SIGNAL", "signal"]),
        "comm": field_string(record, &["COREDUMP_COMM", "comm"]),
        "exe": field_string(record, &["COREDUMP_EXE", "exe"]),
        "command_line": field_string(record, &["COREDUMP_CMDLINE", "cmdline"]),
        "unit": field_string(record, &["COREDUMP_UNIT", "unit"]),
        "user_unit": field_string(record, &["COREDUMP_USER_UNIT", "user_unit"]),
        "core_file": field_string(record, &["COREDUMP_FILENAME", "core_file"]),
        "truncated": field_bool(record, &["COREDUMP_TRUNCATED", "truncated"]),
        "core_size": field_u64(record, &["COREDUMP_SIZE", "core_size"]),
        "package": field_string(record, &["COREDUMP_PACKAGE_NAME", "package"]),
        "package_version": field_string(record, &["COREDUMP_PACKAGE_VERSION", "package_version"]),
    }))
}

async fn recent_crash_events(since_minutes: u64, limit: u64) -> Result<Vec<Value>, String> {
    let journal_limit = (limit.saturating_mul(20)).clamp(100, 1_000);
    let kernel = journal_events(since_minutes, journal_limit, true);
    let errors = journal_events(since_minutes, journal_limit, false);
    let (kernel, errors) = tokio::join!(kernel, errors);
    let mut records = kernel?;
    records.extend(errors?);

    let mut seen = BTreeSet::new();
    let mut events = Vec::new();
    for record in records {
        let Some(event) = normalize_event(&record) else {
            continue;
        };
        let key = format!(
            "{}:{}:{}:{}",
            event["boot_id"].as_str().unwrap_or_default(),
            event["timestamp_us"].as_u64().unwrap_or_default(),
            event["pid"].as_str().unwrap_or_default(),
            event["message"].as_str().unwrap_or_default(),
        );
        if seen.insert(key) {
            events.push(event);
        }
    }
    events.sort_by_key(|event| std::cmp::Reverse(event["timestamp_us"].as_u64()));
    events.truncate(journal_limit as usize);
    Ok(events)
}

async fn journal_events(
    since_minutes: u64,
    limit: u64,
    kernel: bool,
) -> Result<Vec<Value>, String> {
    let mut args = vec![
        "--no-pager".to_string(),
        "--quiet".to_string(),
        "--output=json".to_string(),
        format!("--output-fields={EVENT_FIELDS}"),
        "--reverse".to_string(),
        "--since".to_string(),
        format!("-{since_minutes}min"),
        "-n".to_string(),
        limit.to_string(),
    ];
    if kernel {
        args.push("--dmesg".to_string());
    } else {
        args.push("--priority=0..4".to_string());
    }
    let output = run_checked(
        journalctl_path(),
        args,
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;
    Ok(parse_json_records(&output.stdout))
}

fn normalize_event(record: &Value) -> Option<Value> {
    let message = field_string(record, &["MESSAGE", "message"])?;
    let kind = classify_message(&message)?;
    let timestamp_us = field_u64(record, &["__REALTIME_TIMESTAMP", "timestamp_us"])?;
    let boot_id = field_string(record, &["_BOOT_ID", "boot_id"])
        .filter(|value| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(|value| value.to_ascii_lowercase());
    Some(json!({
        "id": format!(
            "journal:{}:{}",
            boot_id.as_deref().unwrap_or("unknown"),
            timestamp_us,
        ),
        "kind": kind,
        "timestamp_us": timestamp_us,
        "boot_id": boot_id,
        "pid": field_string(record, &["_PID", "COREDUMP_PID", "pid"]),
        "uid": field_string(record, &["_UID", "uid"]),
        "unit": field_string(record, &["_SYSTEMD_UNIT", "COREDUMP_UNIT", "unit"]),
        "user_unit": field_string(record, &["_SYSTEMD_USER_UNIT", "COREDUMP_USER_UNIT", "user_unit"]),
        "priority": field_string(record, &["PRIORITY", "priority"]),
        "identifier": field_string(record, &["SYSLOG_IDENTIFIER", "identifier"]),
        "comm": field_string(record, &["_COMM", "comm"]),
        "exe": field_string(record, &["_EXE", "exe"]),
        "message": message,
    }))
}

fn classify_message(message: &str) -> Option<&'static str> {
    let lower = message.to_ascii_lowercase();
    if [
        "out of memory",
        "oom-kill",
        "oom killer",
        "oom_reaper",
        "killed process",
        "memory cgroup out of memory",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Some("oom");
    }
    if [
        "segfault at",
        "segmentation fault",
        "general protection fault",
        "trap invalid opcode",
        "dumped core",
        "core dumped",
        "result 'core-dump'",
        "result \"core-dump\"",
        "code=dumped",
        "status=11/segv",
        "status=6/abrt",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Some("crash");
    }
    None
}

fn correlate(coredumps: &[Value], events: &[Value]) -> Vec<Value> {
    let mut correlations = Vec::new();
    for coredump in coredumps {
        let Some(coredump_time) = coredump["timestamp_us"].as_u64() else {
            continue;
        };
        let dump_boot = coredump["boot_id"].as_str();
        let dump_pid = coredump["pid"].as_u64().map(|value| value.to_string());
        let dump_unit = coredump["unit"]
            .as_str()
            .or_else(|| coredump["user_unit"].as_str());
        let dump_name = coredump["comm"].as_str().or_else(|| {
            coredump["exe"]
                .as_str()
                .and_then(|value| Path::new(value).file_name())
                .and_then(|value| value.to_str())
        });
        let mut matches = Vec::new();

        for event in events {
            let Some(event_time) = event["timestamp_us"].as_u64() else {
                continue;
            };
            if coredump_time.abs_diff(event_time) > 180 * 1_000_000 {
                continue;
            }
            if let (Some(dump_boot), Some(event_boot)) = (dump_boot, event["boot_id"].as_str()) {
                if dump_boot != event_boot {
                    continue;
                }
            }
            let mut reasons = Vec::new();
            if dump_pid.as_deref() == event["pid"].as_str() {
                reasons.push("same-pid");
            }
            if dump_unit.is_some()
                && dump_unit
                    == event["unit"]
                        .as_str()
                        .or_else(|| event["user_unit"].as_str())
            {
                reasons.push("same-unit");
            }
            if let Some(name) = dump_name {
                if event["message"].as_str().is_some_and(|message| {
                    message
                        .to_ascii_lowercase()
                        .contains(&name.to_ascii_lowercase())
                }) {
                    reasons.push("process-name-in-message");
                }
            }
            if reasons.is_empty() && event["kind"] == "crash" {
                reasons.push("nearby-crash-event");
            }
            if !reasons.is_empty() {
                matches.push(json!({
                    "event_id": event["id"],
                    "delta_ms": coredump_time.abs_diff(event_time) / 1_000,
                    "reasons": reasons,
                }));
            }
        }
        if !matches.is_empty() {
            correlations.push(json!({
                "coredump_id": coredump["id"],
                "matches": matches,
            }));
        }
    }
    correlations
}

fn findings(coredumps: &[Value], events: &[Value]) -> Vec<Value> {
    let mut findings = Vec::new();
    if !coredumps.is_empty() {
        findings.push(json!({
            "code": "recent-coredumps",
            "severity": "warning",
            "title": "Recent process crashes were recorded",
            "detail": format!("{} coredump record(s) were found.", coredumps.len()),
            "evidence": coredumps.iter().filter_map(|item| item["id"].as_str()).collect::<Vec<_>>(),
            "recommendation": "Inspect the newest matching coredump with `cos app crash-doctor backtrace <id>` before restarting repeatedly.",
        }));
    }

    let oom_events = events
        .iter()
        .filter(|event| event["kind"] == "oom")
        .collect::<Vec<_>>();
    if !oom_events.is_empty() {
        findings.push(json!({
            "code": "oom-kill",
            "severity": "critical",
            "title": "The kernel reported out-of-memory termination",
            "detail": format!("{} OOM-related journal event(s) were found.", oom_events.len()),
            "evidence": oom_events.iter().map(|event| event["id"].clone()).collect::<Vec<_>>(),
            "recommendation": "Identify the killed process and cgroup, then inspect memory growth and limits before restarting it.",
        }));
    }

    let crash_events = events
        .iter()
        .filter(|event| event["kind"] == "crash")
        .collect::<Vec<_>>();
    if !crash_events.is_empty() {
        findings.push(json!({
            "code": "segfault-or-core-dump",
            "severity": "critical",
            "title": "The journal reported a segmentation fault or core dump",
            "detail": format!("{} crash-related journal event(s) were found.", crash_events.len()),
            "evidence": crash_events.iter().map(|event| event["id"].clone()).collect::<Vec<_>>(),
            "recommendation": "Correlate PID, executable, unit, and timestamp with a coredump backtrace.",
        }));
    }

    let mut groups = BTreeMap::<String, Vec<&str>>::new();
    for coredump in coredumps {
        let key = coredump["unit"]
            .as_str()
            .or_else(|| coredump["user_unit"].as_str())
            .or_else(|| coredump["exe"].as_str())
            .or_else(|| coredump["comm"].as_str());
        if let (Some(key), Some(id)) = (key, coredump["id"].as_str()) {
            groups.entry(key.to_string()).or_default().push(id);
        }
    }
    for (target, ids) in groups {
        if ids.len() < 2 {
            continue;
        }
        findings.push(json!({
            "code": "repeated-crash",
            "severity": "critical",
            "title": "A process or unit crashed repeatedly",
            "detail": format!("{target} produced {} coredumps in the selected window.", ids.len()),
            "evidence": ids,
            "recommendation": "Preserve a backtrace and inspect recent package, library, configuration, and resource-limit changes before another restart.",
        }));
    }
    findings
}

async fn backtrace(id: &str) -> Result<Value, String> {
    let (boot_id, pid, crash_timestamp_us) = parse_coredump_id(id)?;
    let coredump = coredump_by_id(&boot_id, pid, crash_timestamp_us).await?;
    let matches = vec![
        format!("_BOOT_ID={boot_id}"),
        format!("COREDUMP_PID={pid}"),
        format!("COREDUMP_TIMESTAMP={crash_timestamp_us}"),
    ];

    let mut info_args = vec!["--no-pager".to_string(), "info".to_string()];
    info_args.extend(matches.iter().cloned());
    let info = run_checked(
        coredumpctl_path(),
        info_args,
        TOOL_TIMEOUT,
        ChildPolicy::default(),
    )
    .await?;

    let debugger = live_backtrace(&coredump, &matches).await;
    Ok(json!({
        "id": id,
        "coredump": coredump,
        "recorded_info": {
            "text": info.stdout,
            "truncated": info.stdout_truncated,
        },
        "debugger": match debugger {
            Ok(value) => value,
            Err(error) => json!({
                "available": false,
                "error": error,
            }),
        },
    }))
}

async fn live_backtrace(coredump: &Value, matches: &[String]) -> Result<Value, String> {
    if !Path::new(gdb_path()).is_file() {
        return Err(
            "gdb is not installed; recorded coredump information is still available".to_string(),
        );
    }
    let uid = coredump["uid"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "coredump metadata has no valid UID".to_string())?;
    let gid = coredump["gid"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| "coredump metadata has no valid GID".to_string())?;
    if uid == 0 || gid == 0 {
        return Err(
            "live GDB analysis is disabled for root-owned coredumps; use the recorded stack"
                .to_string(),
        );
    }
    if coredump["core_size"]
        .as_u64()
        .is_some_and(|size| size > MAX_CORE_BYTES)
    {
        return Err(format!(
            "coredump exceeds the {} MiB live-debug limit",
            MAX_CORE_BYTES / 1024 / 1024
        ));
    }

    let temp = crash_tempdir()?;
    let core_path = temp.path().join("core");
    let mut dump_args = vec![
        "--no-pager".to_string(),
        "--quiet".to_string(),
        format!("--output={}", core_path.display()),
        "dump".to_string(),
    ];
    dump_args.extend(matches.iter().cloned());
    run_checked(
        coredumpctl_path(),
        dump_args,
        DEBUGGER_TIMEOUT,
        ChildPolicy {
            file_size_limit: Some(MAX_CORE_BYTES),
            ..ChildPolicy::default()
        },
    )
    .await?;

    let metadata =
        fs::metadata(&core_path).map_err(|error| format!("inspect extracted coredump: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err("coredump extraction produced no regular core file".to_string());
    }
    if metadata.len() > MAX_CORE_BYTES {
        return Err(format!(
            "extracted coredump exceeds the {} MiB live-debug limit",
            MAX_CORE_BYTES / 1024 / 1024
        ));
    }
    prepare_debug_path(temp.path(), &core_path, uid, gid)?;

    let mut args = vec![
        "--batch".to_string(),
        "--quiet".to_string(),
        "--nx".to_string(),
        "--nh".to_string(),
        "--init-eval-command=set auto-load off".to_string(),
        "--init-eval-command=set debuginfod enabled off".to_string(),
        "--init-eval-command=set startup-with-shell off".to_string(),
        "--eval-command=set pagination off".to_string(),
        "--eval-command=set print thread-events off".to_string(),
        "--eval-command=set print frame-arguments none".to_string(),
        "--eval-command=thread apply all bt 64".to_string(),
    ];
    if let Some(executable) = coredump["exe"]
        .as_str()
        .filter(|path| valid_executable_path(path))
    {
        args.push(format!("--se={executable}"));
    }
    args.push(format!("--core={}", core_path.display()));
    let output = run_command(
        gdb_path(),
        args,
        DEBUGGER_TIMEOUT,
        ChildPolicy {
            identity: Some((uid, gid)),
            file_size_limit: Some(16 * 1024 * 1024),
            address_space_limit: Some(4 * 1024 * 1024 * 1024),
            cpu_seconds: Some(DEBUGGER_TIMEOUT.as_secs()),
        },
    )
    .await?;
    let available = output.status.success() || !output.stdout.trim().is_empty();
    Ok(json!({
        "available": available,
        "exit_code": output.status.code(),
        "stdout": output.stdout,
        "stderr": output.stderr,
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
        "ran_as": {"uid": uid, "gid": gid},
        "core_size": metadata.len(),
    }))
}

fn crash_tempdir() -> Result<tempfile::TempDir, String> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("claw-crash-");
    if Path::new("/var/tmp").is_dir() {
        builder
            .tempdir_in("/var/tmp")
            .map_err(|error| format!("create crash analysis directory: {error}"))
    } else {
        builder
            .tempdir()
            .map_err(|error| format!("create crash analysis directory: {error}"))
    }
}

fn prepare_debug_path(directory: &Path, core: &Path, uid: u32, gid: u32) -> Result<(), String> {
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("secure crash analysis directory: {error}"))?;
    fs::set_permissions(core, fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("secure extracted coredump: {error}"))?;
    chown_path(directory, uid, gid)?;
    chown_path(core, uid, gid)
}

fn chown_path(path: &Path, uid: u32, gid: u32) -> Result<(), String> {
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("path contains NUL: {}", path.display()))?;
    if unsafe { libc::chown(path_c.as_ptr(), uid, gid) } != 0 {
        return Err(format!(
            "change ownership of {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn valid_executable_path(path: &str) -> bool {
    path.len() <= 4096
        && path.starts_with('/')
        && !path.chars().any(|character| character.is_control())
}

fn parse_coredump_id(id: &str) -> Result<(String, u32, u64), String> {
    let mut parts = id.split(':');
    let boot_id = parts.next().unwrap_or_default();
    let pid = parts.next().unwrap_or_default();
    let timestamp = parts.next().unwrap_or_default();
    if parts.next().is_some() || timestamp.is_empty() {
        return Err("coredump id must use <boot-id>:<pid>:<timestamp-us> form".to_string());
    }
    if boot_id.len() != 32 || !boot_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("coredump boot id must contain exactly 32 hexadecimal characters".to_string());
    }
    let pid = pid
        .parse::<u32>()
        .map_err(|_| "coredump pid must be a positive 32-bit integer".to_string())?;
    if pid == 0 {
        return Err("coredump pid must be positive".to_string());
    }
    let timestamp = timestamp
        .parse::<u64>()
        .map_err(|_| "coredump timestamp must be a positive integer".to_string())?;
    if timestamp == 0 {
        return Err("coredump timestamp must be positive".to_string());
    }
    Ok((boot_id.to_ascii_lowercase(), pid, timestamp))
}

fn parse_json_records(output: &str) -> Vec<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(output.trim()) {
        return match value {
            Value::Array(values) => values,
            Value::Object(_) => vec![value],
            _ => Vec::new(),
        };
    }
    output
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .collect()
}

fn field_string(record: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match record.get(*key) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    })
}

fn field_u64(record: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| match record.get(*key) {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(value)) => value.parse().ok(),
        _ => None,
    })
}

fn field_bool(record: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| match record.get(*key) {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::String(value)) => match value.as_str() {
            "1" | "true" | "yes" => Some(true),
            "0" | "false" | "no" => Some(false),
            _ => None,
        },
        Some(Value::Number(value)) => value.as_u64().map(|value| value != 0),
        _ => None,
    })
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

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    match params.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(Value::String(_)) | None | Some(Value::Null) => {
            Err(format!("missing required string parameter: {key}"))
        }
        Some(_) => Err(format!("parameter `{key}` must be a string")),
    }
}

#[derive(Clone, Copy, Default)]
struct ChildPolicy {
    identity: Option<(u32, u32)>,
    file_size_limit: Option<u64>,
    address_space_limit: Option<u64>,
    cpu_seconds: Option<u64>,
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

async fn run_checked(
    program: &'static str,
    args: Vec<String>,
    timeout: Duration,
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    let output = run_command(program, args, timeout, policy).await?;
    if !output.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            program,
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    Ok(output)
}

async fn run_command(
    program: &'static str,
    args: Vec<String>,
    timeout: Duration,
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_command_sync(program, args, timeout, policy))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
    timeout: Duration,
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(&args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/nonexistent")
        .env("LC_ALL", "C.UTF-8")
        .env("SYSTEMD_PAGER", "cat")
        .env("PAGER", "cat")
        .env("DEBUGINFOD_URLS", "")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || apply_child_policy(policy));
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {program}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program} stderr is unavailable"))?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("wait for timed-out {program}: {error}"))?;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for {program}: {error}"));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| format!("{program} stdout reader panicked"))??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| format!("{program} stderr reader panicked"))??;
    if timed_out {
        return Err(format!("{program} timed out after {}s", timeout.as_secs()));
    }
    Ok(CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    })
}

fn apply_child_policy(policy: ChildPolicy) -> std::io::Result<()> {
    set_limit(libc::RLIMIT_CORE, 0)?;
    if let Some(limit) = policy.file_size_limit {
        set_limit(libc::RLIMIT_FSIZE, limit)?;
    }
    if let Some(limit) = policy.address_space_limit {
        set_limit(libc::RLIMIT_AS, limit)?;
    }
    if let Some(limit) = policy.cpu_seconds {
        set_limit(libc::RLIMIT_CPU, limit)?;
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if let Some((uid, gid)) = policy.identity {
        if unsafe { libc::setgroups(0, std::ptr::null()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::setgid(gid) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if unsafe { libc::setuid(uid) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(target_env = "gnu")]
type RlimitResource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
type RlimitResource = libc::c_int;

fn set_limit(resource: RlimitResource, value: u64) -> std::io::Result<()> {
    let limit = libc::rlimit {
        rlim_cur: value as libc::rlim_t,
        rlim_max: value as libc::rlim_t,
    };
    if unsafe { libc::setrlimit(resource as _, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read child output: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = STREAM_CAP_BYTES.saturating_sub(kept.len());
        let keep = remaining.min(read);
        kept.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((kept, truncated))
}

fn tail(value: &str) -> String {
    const MAX: usize = 8 * 1024;
    if value.len() <= MAX {
        return value.trim().to_string();
    }
    let mut start = value.len() - MAX;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].trim().to_string()
}

fn journalctl_path() -> &'static str {
    "/usr/bin/journalctl"
}

fn coredumpctl_path() -> &'static str {
    "/usr/bin/coredumpctl"
}

fn gdb_path() -> &'static str {
    "/usr/bin/gdb"
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/crash.rs"
    ));
}
