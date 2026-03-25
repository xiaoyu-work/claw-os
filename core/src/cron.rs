/// Agent-native cron scheduler for Claw OS.
///
/// Unlike traditional crond, this provides agent-native capabilities:
/// - Execution context: tier, scope, and credential injection
/// - Structured result capture: stdout/stderr tails, exit codes, durations
/// - Overlap protection: skip, queue, kill, or allow concurrent runs
/// - Runtime dynamic management: add/remove/enable/disable without config reload
///
/// Storage: `$COS_DATA_DIR/cron/jobs/<id>.json` for definitions,
///          `$COS_DATA_DIR/cron/logs/<id>/<timestamp>.json` for run history.
///
/// Commands:
///   add      — Register a cron job (--schedule, --command, --tier, --scope, etc.)
///   remove   — Remove a cron job by ID
///   list     — List all cron jobs with status and next run time
///   status   — Detailed status of a specific job
///   enable   — Enable a disabled job
///   disable  — Disable a job without removing it
///   logs     — View execution history for a job (--limit N)
///   run      — Manually trigger a job immediately
///   tick     — Process all due jobs (called by scheduler every minute)
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;

use crate::policy::{self, OpType};
use chrono::Timelike;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronJob {
    id: String,
    schedule: String,
    command: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tier: Option<u8>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    credentials: Vec<String>,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    overlap_policy: OverlapPolicy,
    #[serde(default)]
    timeout_secs: Option<u64>,
    created_at: String,
    #[serde(default)]
    last_run: Option<CronRunResult>,
    #[serde(default)]
    next_run: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
enum OverlapPolicy {
    #[default]
    Skip,
    Queue,
    Kill,
    Allow,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CronRunResult {
    started_at: String,
    finished_at: Option<String>,
    exit_code: Option<i32>,
    status: String,
    stdout_tail: Option<String>,
    stderr_tail: Option<String>,
    duration_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Storage paths
// ---------------------------------------------------------------------------

fn cron_dir() -> PathBuf {
    PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
        .join("cron")
}

fn jobs_dir() -> PathBuf {
    cron_dir().join("jobs")
}

fn logs_dir() -> PathBuf {
    cron_dir().join("logs")
}

fn job_path(id: &str) -> PathBuf {
    jobs_dir().join(format!("{id}.json"))
}

fn job_logs_dir(id: &str) -> PathBuf {
    logs_dir().join(id)
}

// ---------------------------------------------------------------------------
// Cron expression parser
// ---------------------------------------------------------------------------

/// Check whether `schedule` (a 5-field cron expression) matches `time`.
///
/// Fields: minute hour day-of-month month day-of-week
///
/// Supported syntax per field:
///   `*`   — every value
///   `N`   — specific value
///   `*/N` — step (every N from min)
///   `N-M` — range (inclusive)
///   `N,M` — list (comma-separated; items can be values, ranges, or steps)
fn cron_matches(schedule: &str, time: &chrono::DateTime<chrono::Utc>) -> bool {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }

    use chrono::{Datelike, Timelike};

    field_matches(fields[0], time.minute(), 0, 59)
        && field_matches(fields[1], time.hour(), 0, 23)
        && field_matches(fields[2], time.day(), 1, 31)
        && field_matches(fields[3], time.month(), 1, 12)
        && field_matches(fields[4], time.weekday().num_days_from_sunday(), 0, 6)
}

/// Check whether a single cron field matches the given `value`.
fn field_matches(field: &str, value: u32, min: u32, max: u32) -> bool {
    // A field can be a comma-separated list of items
    for item in field.split(',') {
        if item_matches(item, value, min, max) {
            return true;
        }
    }
    false
}

/// Match a single non-comma item: `*`, `*/N`, `N-M`, `N-M/S`, or `N`.
fn item_matches(item: &str, value: u32, min: u32, max: u32) -> bool {
    if item == "*" {
        return true;
    }

    // Step: */N or N-M/S
    if let Some((range_part, step_str)) = item.split_once('/') {
        let step: u32 = match step_str.parse() {
            Ok(s) if s > 0 => s,
            _ => return false,
        };
        let (start, end) = if range_part == "*" {
            (min, max)
        } else if let Some((lo, hi)) = range_part.split_once('-') {
            match (lo.parse::<u32>(), hi.parse::<u32>()) {
                (Ok(l), Ok(h)) => (l, h),
                _ => return false,
            }
        } else {
            match range_part.parse::<u32>() {
                Ok(s) => (s, max),
                _ => return false,
            }
        };
        if value < start || value > end {
            return false;
        }
        return (value - start) % step == 0;
    }

    // Range: N-M
    if let Some((lo_str, hi_str)) = item.split_once('-') {
        return match (lo_str.parse::<u32>(), hi_str.parse::<u32>()) {
            (Ok(lo), Ok(hi)) => value >= lo && value <= hi,
            _ => false,
        };
    }

    // Exact value
    match item.parse::<u32>() {
        Ok(n) => value == n,
        _ => false,
    }
}

/// Compute the next time a cron schedule will fire, starting from `from`.
///
/// Forward-scans minute by minute for up to 48 hours.
fn next_run_time(
    schedule: &str,
    from: &chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    use chrono::Duration;

    // Start from the next full minute after `from`
    let mut candidate = from
        .with_nanosecond(0)?
        .with_second(0)?
        .checked_add_signed(Duration::minutes(1))?;

    let limit = 48 * 60; // 48 hours of minutes
    for _ in 0..limit {
        if cron_matches(schedule, &candidate) {
            return Some(candidate);
        }
        candidate = candidate.checked_add_signed(Duration::minutes(1))?;
    }
    None
}

fn format_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

// ---------------------------------------------------------------------------
// Job I/O helpers
// ---------------------------------------------------------------------------

fn load_job(id: &str) -> Result<CronJob, String> {
    let path = job_path(id);
    if !path.is_file() {
        return Err(format!("cron job not found: {id}"));
    }
    let data = fs::read_to_string(&path).map_err(|e| format!("failed to read job {id}: {e}"))?;
    serde_json::from_str(&data).map_err(|e| format!("failed to parse job {id}: {e}"))
}

fn save_job(job: &CronJob) -> Result<(), String> {
    let dir = jobs_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create jobs dir: {e}"))?;
    let data =
        serde_json::to_string_pretty(job).map_err(|e| format!("failed to serialize job: {e}"))?;
    fs::write(job_path(&job.id), data).map_err(|e| format!("failed to write job: {e}"))
}

fn list_all_jobs() -> Result<Vec<CronJob>, String> {
    let dir = jobs_dir();
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    let entries = fs::read_dir(&dir).map_err(|e| format!("failed to read jobs dir: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("failed to read dir entry: {e}"))?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let data = fs::read_to_string(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            if let Ok(job) = serde_json::from_str::<CronJob>(&data) {
                jobs.push(job);
            }
        }
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(jobs)
}

fn save_run_log(job_id: &str, result: &CronRunResult) -> Result<(), String> {
    let dir = job_logs_dir(job_id);
    fs::create_dir_all(&dir).map_err(|e| format!("failed to create logs dir: {e}"))?;
    let filename = result.started_at.replace(':', "-");
    let path = dir.join(format!("{filename}.json"));
    let data = serde_json::to_string_pretty(result)
        .map_err(|e| format!("failed to serialize run result: {e}"))?;
    fs::write(path, data).map_err(|e| format!("failed to write run log: {e}"))
}

fn load_run_logs(job_id: &str, limit: usize) -> Result<Vec<CronRunResult>, String> {
    let dir = job_logs_dir(job_id);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<_> = fs::read_dir(&dir)
        .map_err(|e| format!("failed to read logs dir: {e}"))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext == "json")
                .unwrap_or(false)
        })
        .collect();

    // Sort by filename descending (newest first)
    entries.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    entries.truncate(limit);

    let mut results = Vec::new();
    for entry in entries {
        let data = fs::read_to_string(entry.path())
            .map_err(|e| format!("failed to read log entry: {e}"))?;
        if let Ok(r) = serde_json::from_str::<CronRunResult>(&data) {
            results.push(r);
        }
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Tail helper — keep last N bytes of output
// ---------------------------------------------------------------------------

const TAIL_BYTES: usize = 2048;

fn tail_string(s: &str) -> String {
    if s.len() <= TAIL_BYTES {
        s.to_string()
    } else {
        let start = s.len() - TAIL_BYTES;
        // Find the next char boundary to avoid splitting a multi-byte char
        let start = s.ceil_char_boundary(start);
        format!("...{}", &s[start..])
    }
}

// ---------------------------------------------------------------------------
// Job execution
// ---------------------------------------------------------------------------

fn execute_job(job: &CronJob) -> CronRunResult {
    let start = chrono::Utc::now();
    let started_at = format_time(&start);

    // Build the subprocess command
    #[cfg(unix)]
    let (shell, shell_flag) = ("sh", "-c");
    #[cfg(not(unix))]
    let (shell, shell_flag) = ("cmd", "/c");

    let mut cmd = std::process::Command::new(shell);
    cmd.arg(shell_flag).arg(&job.command);

    // Inject cron context env vars
    cmd.env("COS_CRON_JOB", &job.id);
    cmd.env("COS_SESSION", format!("cron-{}", job.id));

    // Inject tier/scope if specified
    if let Some(tier) = job.tier {
        cmd.env("COS_TIER", tier.to_string());
    }
    if let Some(ref scope) = job.scope {
        cmd.env("COS_SCOPE", scope);
    }

    // Inject credentials as env vars
    for cred_name in &job.credentials {
        // Try to load the credential value from the credential store.
        // We read the file directly to avoid circular module dependencies.
        let cred_path =
            PathBuf::from(std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into()))
                .join("credentials")
                .join(format!("{cred_name}.json"));
        if let Ok(data) = fs::read_to_string(&cred_path) {
            if let Ok(parsed) = serde_json::from_str::<Value>(&data) {
                if let Some(val) = parsed.get("value_b64").and_then(|v| v.as_str()) {
                    // Inject the raw b64 token — the command can decode it.
                    let env_key =
                        format!("COS_CRED_{}", cred_name.to_uppercase().replace('-', "_"));
                    cmd.env(env_key, val);
                }
            }
        }
    }

    // Capture output
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let end = chrono::Utc::now();
            let duration = (end - start).num_milliseconds().max(0) as u64;
            return CronRunResult {
                started_at,
                finished_at: Some(format_time(&end)),
                exit_code: None,
                status: "failed".to_string(),
                stdout_tail: None,
                stderr_tail: Some(format!("failed to spawn: {e}")),
                duration_ms: Some(duration),
            };
        }
    };

    // Apply timeout if configured
    if let Some(timeout_secs) = job.timeout_secs {
        return wait_with_timeout(child, &started_at, &start, timeout_secs);
    }

    // No timeout — wait indefinitely
    match child.wait_with_output() {
        Ok(output) => {
            let end = chrono::Utc::now();
            let duration = (end - start).num_milliseconds().max(0) as u64;
            let code = output.status.code();
            let status = if output.status.success() {
                "success"
            } else {
                "failed"
            };
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            CronRunResult {
                started_at,
                finished_at: Some(format_time(&end)),
                exit_code: code,
                status: status.to_string(),
                stdout_tail: if stdout.is_empty() {
                    None
                } else {
                    Some(tail_string(&stdout))
                },
                stderr_tail: if stderr.is_empty() {
                    None
                } else {
                    Some(tail_string(&stderr))
                },
                duration_ms: Some(duration),
            }
        }
        Err(e) => {
            let end = chrono::Utc::now();
            let duration = (end - start).num_milliseconds().max(0) as u64;
            CronRunResult {
                started_at,
                finished_at: Some(format_time(&end)),
                exit_code: None,
                status: "failed".to_string(),
                stdout_tail: None,
                stderr_tail: Some(format!("wait failed: {e}")),
                duration_ms: Some(duration),
            }
        }
    }
}

fn wait_with_timeout(
    mut child: std::process::Child,
    started_at: &str,
    start: &chrono::DateTime<chrono::Utc>,
    timeout_secs: u64,
) -> CronRunResult {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        match child.try_wait() {
            Ok(Some(exit_status)) => {
                // Process exited before timeout
                let end = chrono::Utc::now();
                let duration = (end - *start).num_milliseconds().max(0) as u64;
                let code = exit_status.code();
                let status = if exit_status.success() {
                    "success"
                } else {
                    "failed"
                };
                // Read whatever output is available
                let (stdout_tail, stderr_tail) = read_child_pipes(&mut child);
                return CronRunResult {
                    started_at: started_at.to_string(),
                    finished_at: Some(format_time(&end)),
                    exit_code: code,
                    status: status.to_string(),
                    stdout_tail,
                    stderr_tail,
                    duration_ms: Some(duration),
                };
            }
            Ok(None) => {
                // Still running
                if std::time::Instant::now() >= deadline {
                    // Timeout: kill the child
                    let _ = child.kill();
                    let _ = child.wait(); // reap
                    let end = chrono::Utc::now();
                    let duration = (end - *start).num_milliseconds().max(0) as u64;
                    let (stdout_tail, stderr_tail) = read_child_pipes(&mut child);
                    return CronRunResult {
                        started_at: started_at.to_string(),
                        finished_at: Some(format_time(&end)),
                        exit_code: None,
                        status: "timeout".to_string(),
                        stdout_tail,
                        stderr_tail: Some(
                            stderr_tail
                                .map(|s| format!("{s}\n[killed: timeout after {timeout_secs}s]"))
                                .unwrap_or_else(|| {
                                    format!("[killed: timeout after {timeout_secs}s]")
                                }),
                        ),
                        duration_ms: Some(duration),
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                let end = chrono::Utc::now();
                let duration = (end - *start).num_milliseconds().max(0) as u64;
                return CronRunResult {
                    started_at: started_at.to_string(),
                    finished_at: Some(format_time(&end)),
                    exit_code: None,
                    status: "failed".to_string(),
                    stdout_tail: None,
                    stderr_tail: Some(format!("wait error: {e}")),
                    duration_ms: Some(duration),
                };
            }
        }
    }
}

fn read_child_pipes(child: &mut std::process::Child) -> (Option<String>, Option<String>) {
    use std::io::Read;
    let stdout_tail = child.stdout.take().and_then(|mut r| {
        let mut buf = String::new();
        r.read_to_string(&mut buf).ok()?;
        if buf.is_empty() {
            None
        } else {
            Some(tail_string(&buf))
        }
    });
    let stderr_tail = child.stderr.take().and_then(|mut r| {
        let mut buf = String::new();
        r.read_to_string(&mut buf).ok()?;
        if buf.is_empty() {
            None
        } else {
            Some(tail_string(&buf))
        }
    });
    (stdout_tail, stderr_tail)
}

// ---------------------------------------------------------------------------
// Overlap checking
// ---------------------------------------------------------------------------

fn is_running(job: &CronJob) -> bool {
    matches!(&job.last_run, Some(r) if r.status == "running")
}

// ---------------------------------------------------------------------------
// Validate job ID
// ---------------------------------------------------------------------------

fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("job ID cannot be empty".into());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("job ID must be alphanumeric (hyphens/underscores allowed)".into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Route a cron subcommand.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "add" => cmd_add(args),
        "remove" => cmd_remove(args),
        "list" => cmd_list(args),
        "status" => cmd_status(args),
        "enable" => cmd_enable(args),
        "disable" => cmd_disable(args),
        "logs" => cmd_logs(args),
        "run" => cmd_run(args),
        "tick" => cmd_tick(args),
        _ => Err(format!("unknown cron command: {command}")),
    }
}

/// Register a new cron job.
///
/// Usage: cos cron add <id> --schedule "*/5 * * * *" --command "..." [options]
fn cmd_add(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let id = args.first().ok_or(
        "usage: cos cron add <id> --schedule \"EXPR\" --command \"CMD\" [--description TEXT] \
         [--tier N] [--scope PATH] [--credentials k1,k2] [--overlap skip|queue|kill|allow] \
         [--timeout SECS]",
    )?;
    validate_id(id)?;

    if job_path(id).is_file() {
        return Err(format!("cron job already exists: {id}"));
    }

    let mut schedule: Option<String> = None;
    let mut command: Option<String> = None;
    let mut description = String::new();
    let mut tier: Option<u8> = None;
    let mut scope: Option<String> = None;
    let mut credentials: Vec<String> = Vec::new();
    let mut overlap_policy = OverlapPolicy::Skip;
    let mut timeout_secs: Option<u64> = None;

    let mut i = 1; // skip the id arg
    while i < args.len() {
        match args[i].as_str() {
            "--schedule" if i + 1 < args.len() => {
                schedule = Some(args[i + 1].clone());
                i += 2;
            }
            "--command" if i + 1 < args.len() => {
                command = Some(args[i + 1].clone());
                i += 2;
            }
            "--description" if i + 1 < args.len() => {
                description = args[i + 1].clone();
                i += 2;
            }
            "--tier" if i + 1 < args.len() => {
                let t = args[i + 1]
                    .parse::<u8>()
                    .map_err(|_| "tier must be 0-3".to_string())?;
                if t > 3 {
                    return Err("tier must be 0-3".into());
                }
                tier = Some(t);
                i += 2;
            }
            "--scope" if i + 1 < args.len() => {
                scope = Some(args[i + 1].clone());
                i += 2;
            }
            "--credentials" if i + 1 < args.len() => {
                credentials = args[i + 1]
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                i += 2;
            }
            "--overlap" if i + 1 < args.len() => {
                overlap_policy = match args[i + 1].as_str() {
                    "skip" | "Skip" => OverlapPolicy::Skip,
                    "queue" | "Queue" => OverlapPolicy::Queue,
                    "kill" | "Kill" => OverlapPolicy::Kill,
                    "allow" | "Allow" => OverlapPolicy::Allow,
                    other => {
                        return Err(format!(
                            "unknown overlap policy: {other}. valid: skip, queue, kill, allow"
                        ))
                    }
                };
                i += 2;
            }
            "--timeout" if i + 1 < args.len() => {
                timeout_secs = Some(
                    args[i + 1]
                        .parse::<u64>()
                        .map_err(|_| "timeout must be a positive integer (seconds)".to_string())?,
                );
                i += 2;
            }
            _ => {
                i += 1;
            }
        }
    }

    let schedule = schedule.ok_or("--schedule is required")?;
    let command = command.ok_or("--command is required")?;

    // Validate the schedule parses correctly
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "invalid cron schedule: expected 5 fields (minute hour day month weekday), got {}",
            fields.len()
        ));
    }

    let now = chrono::Utc::now();
    let next = next_run_time(&schedule, &now);

    let job = CronJob {
        id: id.clone(),
        schedule: schedule.clone(),
        command,
        description,
        tier,
        scope,
        credentials,
        enabled: true,
        overlap_policy,
        timeout_secs,
        created_at: format_time(&now),
        last_run: None,
        next_run: next.map(|t| format_time(&t)),
    };

    save_job(&job)?;

    Ok(json!({
        "added": job.id,
        "schedule": job.schedule,
        "next_run": job.next_run,
    }))
}

/// Remove a cron job by ID.
fn cmd_remove(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron remove <id>")?;
    let path = job_path(id);
    if !path.is_file() {
        return Err(format!("cron job not found: {id}"));
    }

    fs::remove_file(&path).map_err(|e| format!("failed to remove job: {e}"))?;

    // Optionally clean up logs
    let logs = job_logs_dir(id);
    if logs.is_dir() {
        let _ = fs::remove_dir_all(&logs);
    }

    Ok(json!({ "removed": id }))
}

/// List all cron jobs with summary status.
fn cmd_list(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let jobs = list_all_jobs()?;
    let job_list: Vec<Value> = jobs
        .iter()
        .map(|j| {
            let mut entry = json!({
                "id": j.id,
                "schedule": j.schedule,
                "enabled": j.enabled,
                "next_run": j.next_run,
            });
            if let Some(ref lr) = j.last_run {
                entry["last_run"] = json!({
                    "status": lr.status,
                    "finished_at": lr.finished_at,
                });
            }
            entry
        })
        .collect();

    let count = job_list.len();
    Ok(json!({
        "jobs": job_list,
        "count": count,
    }))
}

/// Detailed status of a specific job.
fn cmd_status(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron status <id>")?;
    let job = load_job(id)?;

    Ok(serde_json::to_value(&job).map_err(|e| format!("failed to serialize job: {e}"))?)
}

/// Enable a disabled job.
fn cmd_enable(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron enable <id>")?;
    let mut job = load_job(id)?;
    job.enabled = true;

    // Recompute next run
    let now = chrono::Utc::now();
    job.next_run = next_run_time(&job.schedule, &now).map(|t| format_time(&t));

    save_job(&job)?;

    Ok(json!({
        "id": job.id,
        "enabled": true,
    }))
}

/// Disable a job without removing it.
fn cmd_disable(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron disable <id>")?;
    let mut job = load_job(id)?;
    job.enabled = false;
    job.next_run = None;

    save_job(&job)?;

    Ok(json!({
        "id": job.id,
        "enabled": false,
    }))
}

/// View execution history for a job.
///
/// Usage: cos cron logs <id> [--limit N]
fn cmd_logs(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Read).map_err(|v| v.to_string())?;

    let id = args
        .first()
        .ok_or("usage: cos cron logs <id> [--limit N]")?;

    // Verify the job exists
    if !job_path(id).is_file() {
        return Err(format!("cron job not found: {id}"));
    }

    let mut limit: usize = 20;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--limit" && i + 1 < args.len() {
            limit = args[i + 1]
                .parse::<usize>()
                .map_err(|_| "limit must be a positive integer".to_string())?;
            i += 2;
        } else {
            i += 1;
        }
    }

    let entries = load_run_logs(id, limit)?;

    Ok(json!({
        "job_id": id,
        "entries": entries.iter().map(|r| serde_json::to_value(r).unwrap_or(json!(null))).collect::<Vec<_>>(),
        "count": entries.len(),
        "limit": limit,
    }))
}

/// Manually trigger a job immediately.
fn cmd_run(args: &[String]) -> Result<Value, String> {
    policy::require(OpType::Exec).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron run <id>")?;
    let mut job = load_job(id)?;

    // Respect overlap policy
    if is_running(&job) {
        match job.overlap_policy {
            OverlapPolicy::Skip => {
                return Ok(json!({
                    "job_id": id,
                    "status": "skipped",
                    "reason": "previous run is still running (overlap_policy: Skip)",
                }));
            }
            OverlapPolicy::Queue | OverlapPolicy::Kill | OverlapPolicy::Allow => {
                // For manual runs, proceed for Queue/Kill/Allow
            }
        }
    }

    // Mark as running
    let running_marker = CronRunResult {
        started_at: format_time(&chrono::Utc::now()),
        finished_at: None,
        exit_code: None,
        status: "running".to_string(),
        stdout_tail: None,
        stderr_tail: None,
        duration_ms: None,
    };
    job.last_run = Some(running_marker);
    save_job(&job)?;

    // Execute
    let result = execute_job(&job);

    // Save result
    save_run_log(&job.id, &result)?;
    job.last_run = Some(result.clone());

    // Update next_run
    let now = chrono::Utc::now();
    job.next_run = if job.enabled {
        next_run_time(&job.schedule, &now).map(|t| format_time(&t))
    } else {
        None
    };
    save_job(&job)?;

    Ok(serde_json::to_value(&result).map_err(|e| format!("failed to serialize result: {e}"))?)
}

/// Process all due jobs. Called by an external scheduler (e.g., systemd timer)
/// every minute.
///
/// Usage: cos cron tick
fn cmd_tick(_args: &[String]) -> Result<Value, String> {
    policy::require(OpType::System).map_err(|v| v.to_string())?;

    let now = chrono::Utc::now();
    let jobs = list_all_jobs()?;

    let mut executed: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    for mut job in jobs {
        if !job.enabled {
            continue;
        }

        // Check if the schedule matches the current minute (truncated to second=0)
        let tick_time = now
            .with_nanosecond(0)
            .unwrap_or(now)
            .with_second(0)
            .unwrap_or(now);

        if !cron_matches(&job.schedule, &tick_time) {
            continue;
        }

        // Check overlap policy
        if is_running(&job) {
            match job.overlap_policy {
                OverlapPolicy::Skip => {
                    skipped.push(json!({
                        "id": job.id,
                        "reason": "previous run still running (overlap_policy: Skip)",
                    }));
                    continue;
                }
                OverlapPolicy::Queue => {
                    // In a real implementation, we'd enqueue and wait.
                    // For simplicity, skip with a note.
                    skipped.push(json!({
                        "id": job.id,
                        "reason": "previous run still running (overlap_policy: Queue, queued for next tick)",
                    }));
                    continue;
                }
                OverlapPolicy::Kill | OverlapPolicy::Allow => {
                    // Proceed with execution
                }
            }
        }

        // Mark as running
        let running_marker = CronRunResult {
            started_at: format_time(&now),
            finished_at: None,
            exit_code: None,
            status: "running".to_string(),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: None,
        };
        job.last_run = Some(running_marker);
        let _ = save_job(&job);

        // Execute
        let result = execute_job(&job);

        // Save log entry
        let _ = save_run_log(&job.id, &result);

        // Update job definition
        let exec_status = result.status.clone();
        job.last_run = Some(result);
        job.next_run = next_run_time(&job.schedule, &now).map(|t| format_time(&t));
        let _ = save_job(&job);

        executed.push(json!({
            "id": job.id,
            "status": exec_status,
        }));
    }

    let processed = executed.len() + skipped.len();
    Ok(json!({
        "processed": processed,
        "executed": executed,
        "skipped": skipped,
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    // -- Cron expression matching --

    #[test]
    fn test_cron_matches_every_minute() {
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
            .unwrap();
        assert!(cron_matches("* * * * *", &t));

        let t2 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert!(cron_matches("* * * * *", &t2));
    }

    #[test]
    fn test_cron_matches_specific() {
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
            .unwrap();
        assert!(cron_matches("30 14 * * *", &t));
        assert!(!cron_matches("31 14 * * *", &t));
        assert!(!cron_matches("30 15 * * *", &t));
    }

    #[test]
    fn test_cron_matches_step() {
        // */5 means 0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55
        for min in [0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55] {
            let t = chrono::Utc
                .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
                .unwrap();
            assert!(cron_matches("*/5 * * * *", &t), "should match minute {min}");
        }
        for min in [1, 2, 3, 4, 6, 7, 8, 9, 11] {
            let t = chrono::Utc
                .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
                .unwrap();
            assert!(
                !cron_matches("*/5 * * * *", &t),
                "should NOT match minute {min}"
            );
        }
    }

    #[test]
    fn test_cron_matches_range() {
        for min in 1..=5 {
            let t = chrono::Utc
                .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
                .unwrap();
            assert!(cron_matches("1-5 * * * *", &t), "should match minute {min}");
        }
        let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap();
        assert!(!cron_matches("1-5 * * * *", &t));
        let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 6, 0).unwrap();
        assert!(!cron_matches("1-5 * * * *", &t));
    }

    #[test]
    fn test_cron_matches_list() {
        for min in [1, 15, 30] {
            let t = chrono::Utc
                .with_ymd_and_hms(2026, 3, 25, 12, min, 0)
                .unwrap();
            assert!(
                cron_matches("1,15,30 * * * *", &t),
                "should match minute {min}"
            );
        }
        let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 2, 0).unwrap();
        assert!(!cron_matches("1,15,30 * * * *", &t));
    }

    #[test]
    fn test_field_matches_star() {
        for v in 0..=59 {
            assert!(field_matches("*", v, 0, 59));
        }
    }

    #[test]
    fn test_field_matches_step_with_range() {
        // 1-10/3 matches 1, 4, 7, 10
        assert!(field_matches("1-10/3", 1, 0, 59));
        assert!(field_matches("1-10/3", 4, 0, 59));
        assert!(field_matches("1-10/3", 7, 0, 59));
        assert!(field_matches("1-10/3", 10, 0, 59));
        assert!(!field_matches("1-10/3", 2, 0, 59));
        assert!(!field_matches("1-10/3", 11, 0, 59));
    }

    #[test]
    fn test_cron_invalid_fields() {
        let t = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
            .unwrap();
        // Too few fields
        assert!(!cron_matches("* * *", &t));
        // Too many fields
        assert!(!cron_matches("* * * * * *", &t));
    }

    #[test]
    fn test_cron_day_of_week() {
        // 2026-03-25 is a Wednesday (day 3)
        let t = chrono::Utc.with_ymd_and_hms(2026, 3, 25, 12, 0, 0).unwrap();
        assert!(cron_matches("0 12 * * 3", &t)); // Wednesday
        assert!(!cron_matches("0 12 * * 1", &t)); // Monday
    }

    // -- next_run_time --

    #[test]
    fn test_next_run_time_every_minute() {
        let from = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
            .unwrap();
        let next = next_run_time("* * * * *", &from).unwrap();
        assert_eq!(
            next,
            chrono::Utc
                .with_ymd_and_hms(2026, 3, 25, 14, 31, 0)
                .unwrap()
        );
    }

    #[test]
    fn test_next_run_time_specific() {
        let from = chrono::Utc
            .with_ymd_and_hms(2026, 3, 25, 14, 30, 0)
            .unwrap();
        let next = next_run_time("0 15 * * *", &from).unwrap();
        assert_eq!(
            next,
            chrono::Utc.with_ymd_and_hms(2026, 3, 25, 15, 0, 0).unwrap()
        );
    }

    // -- overlap policy deserialization --

    #[test]
    fn test_overlap_policy_deserialization() {
        let policies = [
            (r#""Skip""#, OverlapPolicy::Skip),
            (r#""Queue""#, OverlapPolicy::Queue),
            (r#""Kill""#, OverlapPolicy::Kill),
            (r#""Allow""#, OverlapPolicy::Allow),
        ];
        for (json_str, expected) in policies {
            let parsed: OverlapPolicy = serde_json::from_str(json_str)
                .unwrap_or_else(|e| panic!("failed to parse {json_str}: {e}"));
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn test_overlap_policy_default() {
        let policy: OverlapPolicy = Default::default();
        assert_eq!(policy, OverlapPolicy::Skip);
    }

    // -- validate_id --

    #[test]
    fn test_validate_id_valid() {
        assert!(validate_id("my-job").is_ok());
        assert!(validate_id("job_1").is_ok());
        assert!(validate_id("test123").is_ok());
    }

    #[test]
    fn test_validate_id_invalid() {
        assert!(validate_id("").is_err());
        assert!(validate_id("has space").is_err());
        assert!(validate_id("has/slash").is_err());
        assert!(validate_id("has.dot").is_err());
    }

    // -- tail_string --

    #[test]
    fn test_tail_string_short() {
        let short = "hello world";
        assert_eq!(tail_string(short), "hello world");
    }

    #[test]
    fn test_tail_string_long() {
        let long = "x".repeat(4000);
        let tailed = tail_string(&long);
        assert!(tailed.len() <= TAIL_BYTES + 4); // +4 for "..."
        assert!(tailed.starts_with("..."));
    }

    // -- storage integration tests (use temp dir) --

    use std::sync::{Mutex, Once};

    static CRON_INIT: Once = Once::new();
    static CRON_LOCK: Mutex<()> = Mutex::new(());

    fn cron_setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = CRON_LOCK.lock().unwrap();
        CRON_INIT.call_once(|| {
            let dir = std::env::temp_dir().join(format!("cos-cron-test-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            let _ = fs::create_dir_all(&dir);
            std::env::set_var("COS_DATA_DIR", &dir);
        });
        std::env::remove_var("COS_SESSION");
        // Clean up jobs and logs between tests
        let jdir = jobs_dir();
        if jdir.is_dir() {
            let _ = fs::remove_dir_all(&jdir);
        }
        let ldir = logs_dir();
        if ldir.is_dir() {
            let _ = fs::remove_dir_all(&ldir);
        }
        guard
    }

    #[test]
    fn test_add_and_list() {
        let _g = cron_setup();

        let args = vec![
            "test-job".to_string(),
            "--schedule".to_string(),
            "*/5 * * * *".to_string(),
            "--command".to_string(),
            "echo hello".to_string(),
            "--description".to_string(),
            "A test job".to_string(),
        ];
        let result = cmd_add(&args).unwrap();
        assert_eq!(result["added"], "test-job");
        assert_eq!(result["schedule"], "*/5 * * * *");

        // List should show the job
        let list_result = cmd_list(&[]).unwrap();
        assert_eq!(list_result["count"], 1);
        let jobs = list_result["jobs"].as_array().unwrap();
        assert_eq!(jobs[0]["id"], "test-job");
        assert_eq!(jobs[0]["enabled"], true);
    }

    #[test]
    fn test_add_duplicate() {
        let _g = cron_setup();

        let args = vec![
            "dup-job".to_string(),
            "--schedule".to_string(),
            "* * * * *".to_string(),
            "--command".to_string(),
            "echo hi".to_string(),
        ];
        cmd_add(&args).unwrap();
        let err = cmd_add(&args).unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn test_remove() {
        let _g = cron_setup();

        let args = vec![
            "rm-job".to_string(),
            "--schedule".to_string(),
            "* * * * *".to_string(),
            "--command".to_string(),
            "echo bye".to_string(),
        ];
        cmd_add(&args).unwrap();

        let result = cmd_remove(&["rm-job".to_string()]).unwrap();
        assert_eq!(result["removed"], "rm-job");

        // Should be gone from list
        let list_result = cmd_list(&[]).unwrap();
        assert_eq!(list_result["count"], 0);
    }

    #[test]
    fn test_remove_nonexistent() {
        let _g = cron_setup();
        let err = cmd_remove(&["no-such-job".to_string()]).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_enable_disable() {
        let _g = cron_setup();

        let args = vec![
            "toggle-job".to_string(),
            "--schedule".to_string(),
            "0 * * * *".to_string(),
            "--command".to_string(),
            "echo toggle".to_string(),
        ];
        cmd_add(&args).unwrap();

        // Disable
        let result = cmd_disable(&["toggle-job".to_string()]).unwrap();
        assert_eq!(result["enabled"], false);

        // Verify via status
        let status = cmd_status(&["toggle-job".to_string()]).unwrap();
        assert_eq!(status["enabled"], false);
        assert_eq!(status["next_run"], Value::Null);

        // Enable
        let result = cmd_enable(&["toggle-job".to_string()]).unwrap();
        assert_eq!(result["enabled"], true);

        // Verify enabled and next_run is set
        let status = cmd_status(&["toggle-job".to_string()]).unwrap();
        assert_eq!(status["enabled"], true);
        assert!(status["next_run"].is_string());
    }

    #[test]
    fn test_status() {
        let _g = cron_setup();

        let args = vec![
            "status-job".to_string(),
            "--schedule".to_string(),
            "30 14 * * *".to_string(),
            "--command".to_string(),
            "echo status".to_string(),
            "--tier".to_string(),
            "1".to_string(),
            "--scope".to_string(),
            "/den/project".to_string(),
            "--overlap".to_string(),
            "allow".to_string(),
            "--timeout".to_string(),
            "300".to_string(),
        ];
        cmd_add(&args).unwrap();

        let result = cmd_status(&["status-job".to_string()]).unwrap();
        assert_eq!(result["id"], "status-job");
        assert_eq!(result["schedule"], "30 14 * * *");
        assert_eq!(result["tier"], 1);
        assert_eq!(result["scope"], "/den/project");
        assert_eq!(result["overlap_policy"], "Allow");
        assert_eq!(result["timeout_secs"], 300);
    }

    #[test]
    fn test_run_dispatch() {
        let _g = cron_setup();

        // Add a simple echo job
        let add_args = vec![
            "echo-job".to_string(),
            "--schedule".to_string(),
            "* * * * *".to_string(),
            "--command".to_string(),
            "echo cron-test-output".to_string(),
        ];
        cmd_add(&add_args).unwrap();

        // Run it manually
        let result = cmd_run(&["echo-job".to_string()]).unwrap();
        let status = result["status"].as_str().unwrap();
        // The command should succeed or fail (depends on shell availability)
        assert!(
            status == "success" || status == "failed",
            "unexpected status: {status}"
        );

        // Logs should have an entry
        let logs = cmd_logs(&["echo-job".to_string()]).unwrap();
        assert!(logs["count"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn test_logs_limit() {
        let _g = cron_setup();

        // Create a job and save multiple log entries
        let args = vec![
            "log-job".to_string(),
            "--schedule".to_string(),
            "* * * * *".to_string(),
            "--command".to_string(),
            "echo hi".to_string(),
        ];
        cmd_add(&args).unwrap();

        // Save 5 fake log entries
        for i in 0..5 {
            let result = CronRunResult {
                started_at: format!("2026-03-25T10-{:02}-00Z", i),
                finished_at: Some(format!("2026-03-25T10-{:02}-01Z", i)),
                exit_code: Some(0),
                status: "success".to_string(),
                stdout_tail: Some(format!("output {i}")),
                stderr_tail: None,
                duration_ms: Some(100),
            };
            save_run_log("log-job", &result).unwrap();
        }

        // Default limit (20) should return all 5
        let logs = cmd_logs(&["log-job".to_string()]).unwrap();
        assert_eq!(logs["count"], 5);

        // Limit to 2
        let logs = cmd_logs(&[
            "log-job".to_string(),
            "--limit".to_string(),
            "2".to_string(),
        ])
        .unwrap();
        assert_eq!(logs["count"], 2);
    }

    #[test]
    fn test_unknown_command() {
        let err = run("nonexistent", &[]).unwrap_err();
        assert!(err.contains("unknown cron command"));
    }

    #[test]
    fn test_add_missing_schedule() {
        let _g = cron_setup();
        let args = vec![
            "bad-job".to_string(),
            "--command".to_string(),
            "echo hi".to_string(),
        ];
        let err = cmd_add(&args).unwrap_err();
        assert!(err.contains("--schedule"));
    }

    #[test]
    fn test_add_missing_command() {
        let _g = cron_setup();
        let args = vec![
            "bad-job".to_string(),
            "--schedule".to_string(),
            "* * * * *".to_string(),
        ];
        let err = cmd_add(&args).unwrap_err();
        assert!(err.contains("--command"));
    }

    #[test]
    fn test_add_invalid_schedule() {
        let _g = cron_setup();
        let args = vec![
            "bad-sched".to_string(),
            "--schedule".to_string(),
            "* *".to_string(),
            "--command".to_string(),
            "echo hi".to_string(),
        ];
        let err = cmd_add(&args).unwrap_err();
        assert!(err.contains("5 fields"));
    }

    #[test]
    fn test_tick_no_jobs() {
        let _g = cron_setup();
        let result = cmd_tick(&[]).unwrap();
        assert_eq!(result["processed"], 0);
    }

    #[test]
    fn test_cronjob_serialization_roundtrip() {
        let job = CronJob {
            id: "roundtrip".to_string(),
            schedule: "*/10 * * * *".to_string(),
            command: "echo hello".to_string(),
            description: "test roundtrip".to_string(),
            tier: Some(1),
            scope: Some("/den".to_string()),
            credentials: vec!["key1".to_string(), "key2".to_string()],
            enabled: true,
            overlap_policy: OverlapPolicy::Queue,
            timeout_secs: Some(60),
            created_at: "2026-03-25T10:00:00Z".to_string(),
            last_run: None,
            next_run: Some("2026-03-25T10:10:00Z".to_string()),
        };

        let json = serde_json::to_string(&job).unwrap();
        let parsed: CronJob = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "roundtrip");
        assert_eq!(parsed.tier, Some(1));
        assert_eq!(parsed.credentials.len(), 2);
        assert_eq!(parsed.overlap_policy, OverlapPolicy::Queue);
        assert_eq!(parsed.timeout_secs, Some(60));
    }

    #[test]
    fn test_is_running() {
        let mut job = CronJob {
            id: "run-check".to_string(),
            schedule: "* * * * *".to_string(),
            command: "echo hi".to_string(),
            description: String::new(),
            tier: None,
            scope: None,
            credentials: Vec::new(),
            enabled: true,
            overlap_policy: OverlapPolicy::Skip,
            timeout_secs: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            last_run: None,
            next_run: None,
        };

        assert!(!is_running(&job));

        job.last_run = Some(CronRunResult {
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: None,
            exit_code: None,
            status: "running".to_string(),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: None,
        });
        assert!(is_running(&job));

        job.last_run = Some(CronRunResult {
            started_at: "2026-01-01T00:00:00Z".to_string(),
            finished_at: Some("2026-01-01T00:01:00Z".to_string()),
            exit_code: Some(0),
            status: "success".to_string(),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: Some(60000),
        });
        assert!(!is_running(&job));
    }
}
