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

use crate::caps::{Cap, CapSet, Role, require_or_json, Scope, Verb};
use chrono::Timelike;

const DEFAULT_TIMEOUT_SECS: u64 = 300;
const MAX_TIMEOUT_SECS: u64 = 86_400;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

fn default_true() -> bool {
    true
}

struct CronSessionGuard {
    id: String,
    uid: u32,
}

impl Drop for CronSessionGuard {
    fn drop(&mut self) {
        crate::proc::deregister_session_for_owner(&self.id, self.uid);
    }
}

fn failed_run(
    started_at: &str,
    start: &chrono::DateTime<chrono::Utc>,
    error: &str,
) -> CronRunResult {
    let end = chrono::Utc::now();
    CronRunResult {
        started_at: started_at.to_string(),
        finished_at: Some(format_time(&end)),
        exit_code: None,
        status: "failed".to_string(),
        stdout_tail: None,
        stderr_tail: Some(error.to_string()),
        duration_ms: Some(
            end.signed_duration_since(*start)
                .num_milliseconds()
                .max(0) as u64,
        ),
        run_id: None,
        pid: None,
        pid_start_time_ticks: None,
    }
}

#[cfg(unix)]
fn apply_cron_identity(
    command: &mut std::process::Command,
    uid: u32,
    home: &std::path::Path,
    credential_fds: &[std::os::unix::io::RawFd],
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::process::CommandExt;

    let metadata = fs::metadata(home)
        .map_err(|error| format!("inspect cron owner home: {error}"))?;
    if metadata.uid() != uid {
        return Err("cron owner home uid mismatch".to_string());
    }
    let gid = primary_gid(uid)?;
    let euid = unsafe { libc::geteuid() as u32 };
    if euid != 0 && euid != uid {
        return Err(format!("cannot run cron owner uid {uid} as uid {euid}"));
    }
    let parent = unsafe { libc::getpid() };
    let credential_fds = credential_fds.to_vec();
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for fd in &credential_fds {
                let flags = libc::fcntl(*fd, libc::F_GETFD);
                if flags < 0
                    || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if euid == 0
                && (libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0)
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "cron scheduler exited during spawn",
                ));
            }
            Ok(())
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn apply_cron_identity(
    _command: &mut std::process::Command,
    _uid: u32,
    _home: &std::path::Path,
    _credential_fds: &[i32],
) -> Result<(), String> {
    Err("cron owner isolation requires Unix".to_string())
}

#[cfg(unix)]
fn primary_gid(uid: u32) -> Result<u32, String> {
    const SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let code = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if code != 0 || result.is_null() {
        return Err(format!("passwd lookup failed for uid {uid}"));
    }
    Ok(passwd.pw_gid)
}

fn validated_owner_home(uid: u32, recorded: Option<&str>) -> Result<PathBuf, String> {
    let configured = crate::paths::verified_home_for_uid(uid)?;
    if let Some(recorded) = recorded {
        let recorded = PathBuf::from(recorded)
            .canonicalize()
            .map_err(|error| format!("canonicalize recorded cron home: {error}"))?;
        if recorded != configured {
            return Err("cron owner home no longer matches the account database".to_string());
        }
    }
    Ok(configured)
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
    #[serde(default)]
    owner_uid: Option<u32>,
    #[serde(default)]
    owner_home: Option<String>,
    #[serde(default)]
    caps: Option<CapSet>,
    #[serde(default)]
    role: Option<Role>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    /// PID of the spawned shell. Recorded so that, on cos restart,
    /// `is_running` can decide whether a stale `status: "running"`
    /// row belongs to a still-live process or a crashed run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pid_start_time_ticks: Option<u64>,
}

struct CronOwner {
    uid: u32,
    home: String,
    caps: CapSet,
    role: Option<Role>,
    tier: Option<u8>,
}

fn current_owner() -> Result<CronOwner, String> {
    let session = crate::proc::current_session_info_for_caps()
        .ok_or_else(|| "cron changes require a registered session".to_string())?;
    let caps = session
        .caps
        .clone()
        .ok_or_else(|| "cron owner session has no capabilities".to_string())?;
    let uid = crate::paths::current_owner_uid_override().unwrap_or_else(|| {
        #[cfg(unix)]
        unsafe {
            libc::geteuid() as u32
        }
        #[cfg(not(unix))]
        {
            0
        }
    });
    if uid == 0 {
        return Err("cron jobs require a non-root owner".to_string());
    }
    let home = validated_owner_home(uid, None)?;
    Ok(CronOwner {
        uid,
        home: home.to_string_lossy().into_owned(),
        caps,
        role: session.role.as_deref().and_then(Role::parse),
        tier: session.tier,
    })
}

fn require_job_owner(job: &CronJob, uid: u32) -> Result<(), String> {
    match job.owner_uid {
        Some(owner_uid) if owner_uid == uid => Ok(()),
        Some(_) => Err(format!("cron job `{}` belongs to another user", job.id)),
        None => Err(format!("cron job `{}` has no trusted owner", job.id)),
    }
}

// ---------------------------------------------------------------------------
// Storage paths
// ---------------------------------------------------------------------------

fn cron_dir() -> PathBuf {
    crate::paths::data_dir().join("cron")
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
///
/// Day-of-month (field 3) and day-of-week (field 5) follow Vixie /
/// POSIX semantics: when both are restricted (non-`*`), the job runs
/// if **either** field matches the current day. When exactly one is
/// restricted, only that one applies. When both are `*`, every day
/// matches. This matches `cron(1)` on every mainstream Linux/BSD; the
/// previous AND-based logic silently desynchronised users who
/// imported a crontab written for vixie / systemd-cron.
fn cron_matches(schedule: &str, time: &chrono::DateTime<chrono::Utc>) -> bool {
    let fields: Vec<&str> = schedule.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }

    use chrono::{Datelike, Timelike};

    if !field_matches(fields[0], time.minute(), 0, 59) {
        return false;
    }
    if !field_matches(fields[1], time.hour(), 0, 23) {
        return false;
    }
    if !field_matches(fields[3], time.month(), 1, 12) {
        return false;
    }

    // Day-of-month (field 2) and day-of-week (field 4) get the
    // Vixie OR-semantics treatment. A `*` is treated as "unrestricted"
    // — when both are `*` the combined day predicate is trivially
    // true; when exactly one is restricted, only that one matters;
    // when both are restricted, EITHER match makes the rule fire.
    let dom_field = fields[2];
    let dow_field = fields[4];
    let dom_wild = dom_field == "*";
    let dow_wild = dow_field == "*";

    let dom_ok = field_matches(dom_field, time.day(), 1, 31);
    let dow_ok = field_matches(dow_field, time.weekday().num_days_from_sunday(), 0, 6);

    match (dom_wild, dow_wild) {
        (true, true) => true,
        (false, true) => dom_ok,
        (true, false) => dow_ok,
        (false, false) => dom_ok || dow_ok,
    }
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
        return (value - start).is_multiple_of(step);
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
    validate_id(id)?;
    let path = job_path(id);
    let data = crate::filelock::read_locked(&path)
        .map_err(|e| format!("failed to read job {id}: {e}"))?
        .ok_or_else(|| format!("cron job not found: {id}"))?;
    serde_json::from_str(&data).map_err(|e| format!("failed to parse job {id}: {e}"))
}

fn save_job(job: &CronJob) -> Result<(), String> {
    let data =
        serde_json::to_string_pretty(job).map_err(|e| format!("failed to serialize job: {e}"))?;
    crate::filelock::write_locked(&job_path(&job.id), &data)
        .map_err(|e| format!("failed to write job: {e}"))
}

fn create_job(job: &CronJob) -> Result<(), String> {
    let data =
        serde_json::to_string_pretty(job).map_err(|e| format!("failed to serialize job: {e}"))?;
    crate::filelock::update_locked::<_, String>(&job_path(&job.id), |existing| {
        if existing.is_some() {
            return Err(format!("cron job already exists: {}", job.id));
        }
        Ok(data)
    })
    .map_err(|error| error.to_string())
}

/// Atomic read-modify-write on a single cron job file. Eliminates
/// the lost-update race between concurrent `cos cron run X` and
/// `cos cron tick` invocations (the prior load_job + mutate +
/// save_job pattern would clobber whichever path finished writing
/// second).
fn update_job<F>(id: &str, transform: F) -> Result<CronJob, String>
where
    F: FnOnce(CronJob) -> Result<CronJob, String>,
{
    validate_id(id)?;
    let path = job_path(id);
    let captured: std::cell::RefCell<Option<CronJob>> = std::cell::RefCell::new(None);
    crate::filelock::update_locked::<_, String>(&path, |existing| {
        let raw = existing.ok_or_else(|| format!("cron job not found: {id}"))?;
        let job: CronJob = serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse job {id}: {e}"))?;
        let next = transform(job)?;
        let data = serde_json::to_string_pretty(&next)
            .map_err(|e| format!("failed to serialize job: {e}"))?;
        *captured.borrow_mut() = Some(next);
        Ok(data)
    })
    .map_err(|e| e.to_string())?;
    captured
        .into_inner()
        .ok_or_else(|| "internal: update_job lost captured job".to_string())
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
            let data = crate::filelock::read_locked(&path)
                .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
            if let Some(data) = data {
                if let Ok(job) = serde_json::from_str::<CronJob>(&data) {
                    jobs.push(job);
                }
            }
        }
    }
    jobs.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(jobs)
}

fn save_run_log(job_id: &str, result: &CronRunResult) -> Result<(), String> {
    let mut filename = result.started_at.replace(':', "-");
    if let Some(run_id) = result.run_id.as_deref() {
        filename.push('-');
        filename.push_str(run_id);
    }
    let path = job_logs_dir(job_id).join(format!("{filename}.json"));
    let data = serde_json::to_string_pretty(result)
        .map_err(|e| format!("failed to serialize run result: {e}"))?;
    crate::filelock::write_locked(&path, &data).map_err(|e| format!("failed to write run log: {e}"))
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
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
    entries.truncate(limit);

    let mut results = Vec::new();
    for entry in entries {
        let data = crate::filelock::read_locked(&entry.path())
            .map_err(|e| format!("failed to read log entry: {e}"))?;
        if let Some(data) = data {
            if let Ok(r) = serde_json::from_str::<CronRunResult>(&data) {
                results.push(r);
            }
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

fn execute_job(job: &CronJob, run_id: &str) -> CronRunResult {
    let start = chrono::Utc::now();
    let started_at = format_time(&start);

    // Build the subprocess command
    #[cfg(unix)]
    let (shell, shell_flag) = ("sh", "-c");
    #[cfg(not(unix))]
    let (shell, shell_flag) = ("cmd", "/c");

    let owner_uid = match job.owner_uid {
        Some(uid) if uid != 0 => uid,
        _ => return failed_run(&started_at, &start, "cron job has no non-root owner"),
    };
    let owner_home = match validated_owner_home(owner_uid, job.owner_home.as_deref()) {
        Ok(home) => home,
        Err(error) => return failed_run(&started_at, &start, &error),
    };
    let caps = match job.caps.clone() {
        Some(caps) => {
            let safe_caps = Role::AgentHost.caps_with_scopes(
                Some(Scope::path(format!("{}/**", owner_home.display()))),
                Some(Scope::Wild),
                Some(Scope::Wild),
            );
            caps.intersect(&safe_caps)
        }
        None => return failed_run(&started_at, &start, "cron job has no capability snapshot"),
    };
    if !caps.covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild)) {
        return failed_run(
            &started_at,
            &start,
            "cron job capability snapshot does not permit process execution",
        );
    }
    let tier = job
        .tier
        .unwrap_or(Role::AgentHost.credential_tier())
        .max(Role::AgentHost.credential_tier());
    let session_id = format!("cron-{}", uuid::Uuid::new_v4().simple());
    let session = crate::proc::SessionInfo {
        session_id: session_id.clone(),
        pid: 0,
        command: vec![job.command.clone()],
        started_at: started_at.clone(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("cron".to_string()),
        parent: None,
        workdir: Some(owner_home.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: Some(tier),
        scope: job.scope.clone(),
        priority: None,
        caps: Some(caps.clone()),
        transient_caps: None,
        role: Some(Role::AgentHost.name().to_string()),
        app_id: None,
        pending_bind: true,
        start_time_ticks: None,
    };
    if let Err(error) = crate::proc::register_session_for_owner(session, owner_uid) {
        return failed_run(&started_at, &start, &format!("register cron session: {error}"));
    }
    let _session_guard = CronSessionGuard {
        id: session_id.clone(),
        uid: owner_uid,
    };

    let mut cmd = std::process::Command::new(crate::bridge::app_runner_path());
    cmd.arg("--").arg(shell).arg(shell_flag).arg(&job.command);
    // Default stdin to /dev/null so a job that mistakenly tries to
    // read input doesn't block forever inheriting the cos parent
    // tty / pipe.
    cmd.stdin(std::process::Stdio::null());

    // Inject cron context env vars
    cmd.env("COS_CRON_JOB", &job.id);
    cmd.env("COS_SESSION", &session_id);
    cmd.env(
        "COS_PROC_DATA_DIR",
        PathBuf::from("/run/cos/caps").join(owner_uid.to_string()),
    );
    cmd.env("HOME", &owner_home).env("COS_HOME", &owner_home);

    // Inject tier/scope if specified
    cmd.env("COS_TIER", tier.to_string());
    if let Some(ref scope) = job.scope {
        cmd.env("COS_SCOPE", scope);
    }

    // Keep plaintext credentials in sealed anonymous memfds. Injection
    // is enabled only when Yama blocks same-UID ptrace/proc-fd access;
    // the child is also marked non-dumpable before exec as defense in depth.
    let mut credential_files = Vec::new();
    if !job.credentials.is_empty() {
        if let Err(error) = require_proc_credential_isolation() {
            return failed_run(&started_at, &start, &error);
        }
    }
    for cred_name in &job.credentials {
        let requested = Cap::new(
            Verb::SECRET_READ,
            Scope::name(format!("default/{cred_name}")),
        );
        if !caps.covers(&requested) {
            return failed_run(
                &started_at,
                &start,
                &format!("cron job lacks secret.read for `{cred_name}`"),
            );
        }
        match crate::credential::load_for_scheduler(
            cred_name,
            "default",
            &owner_home,
            owner_uid,
            tier,
        ) {
            Ok(val) => {
                let safe_name = cred_name.to_uppercase().replace('-', "_");
                let file = match prepare_credential_memfd(cred_name, &val) {
                    Ok(file) => file,
                    Err(error) => {
                        return failed_run(
                            &started_at,
                            &start,
                            &format!("prepare credential `{cred_name}`: {error}"),
                        );
                    }
                };
                #[cfg(unix)]
                let path = {
                    use std::os::unix::io::AsRawFd;
                    format!("/proc/self/fd/{}", file.as_raw_fd())
                };
                #[cfg(not(unix))]
                let path = String::new();
                let env_key = format!("COS_CRED_{}_FILE", safe_name);
                cmd.env(env_key, path);
                credential_files.push(file);
            }
            Err(e) => {
                return failed_run(
                    &started_at,
                    &start,
                    &format!("load credential `{cred_name}`: {e}"),
                );
            }
        }
    }

    // Capture output
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    let credential_fds: Vec<_> = {
        use std::os::unix::io::AsRawFd;
        credential_files.iter().map(|file| file.as_raw_fd()).collect()
    };
    #[cfg(not(unix))]
    let credential_fds = Vec::new();
    if let Err(error) =
        apply_cron_identity(&mut cmd, owner_uid, &owner_home, &credential_fds)
    {
        return failed_run(&started_at, &start, &error);
    }

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
                run_id: None,
                pid: None,
                pid_start_time_ticks: None,
            };
        }
    };
    drop(credential_files);
    let child_pid = child.id();
    if let Err(error) =
        crate::proc::bind_session_process_for_owner(&session_id, child_pid, owner_uid)
    {
        let mut child = child;
        let _ = terminate_previous_run(
            child_pid,
            crate::proc::read_start_time_ticks_pub(child_pid),
            Some(owner_uid),
        );
        let _ = child.wait();
        return failed_run(&started_at, &start, &error);
    }
    let child_start_time = crate::proc::read_start_time_ticks_pub(child_pid);
    if let Err(error) = update_job(&job.id, |mut current| {
        let Some(marker) = current.last_run.as_mut() else {
            return Err("cron run marker disappeared before process bind".to_string());
        };
        if marker.status != "running" || marker.run_id.as_deref() != Some(run_id) {
            return Err("cron run lease changed before process bind".to_string());
        }
        marker.pid = Some(child_pid);
        marker.pid_start_time_ticks = child_start_time;
        Ok(current)
    }) {
        let mut child = child;
        let _ = terminate_previous_run(child_pid, child_start_time, Some(owner_uid));
        let _ = child.wait();
        return failed_run(
            &started_at,
            &start,
            &format!("persist cron process identity: {error}"),
        );
    }

    // Always go through the bounded drainer. The previous
    // wait_with_output path read ALL output into memory, so a
    // multi-GB log line would OOM the cron driver and take down
    // tick processing for every other job.
    let effective_timeout = job
        .timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);
    let mut result = wait_with_timeout_for_owner(
        child,
        &started_at,
        &start,
        effective_timeout,
        owner_uid,
        child_start_time,
    );
    result.run_id = Some(run_id.to_string());
    result.pid = Some(child_pid);
    result.pid_start_time_ticks = child_start_time;
    result
}

#[cfg(target_os = "linux")]
fn prepare_credential_memfd(name: &str, value: &str) -> std::io::Result<std::fs::File> {
    use std::io::Write;
    use std::os::unix::io::FromRawFd;

    let label = std::ffi::CString::new(format!("cos-cron-{name}"))
        .map_err(|_| std::io::Error::other("credential name contains NUL"))?;
    let fd = unsafe {
        libc::memfd_create(
            label.as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(value.as_bytes())?;
    file.flush()?;
    if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let seals = libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    if unsafe { libc::fcntl(fd, libc::F_ADD_SEALS, seals) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn require_proc_credential_isolation() -> Result<(), String> {
    let value = fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope")
        .map_err(|error| format!("read kernel.yama.ptrace_scope: {error}"))?;
    let level = value
        .trim()
        .parse::<u32>()
        .map_err(|error| format!("parse kernel.yama.ptrace_scope: {error}"))?;
    if level < 2 {
        return Err(
            "cron credentials require kernel.yama.ptrace_scope=2 or stronger".to_string(),
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn require_proc_credential_isolation() -> Result<(), String> {
    Err("cron credential isolation requires Linux".to_string())
}

#[cfg(not(target_os = "linux"))]
fn prepare_credential_memfd(_name: &str, _value: &str) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::other(
        "cron credential injection requires Linux memfd",
    ))
}

fn reject_symlink(path: &std::path::Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(std::io::Error::other(
            format!("refusing symlink credential path {}", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn cleanup_runtime_credentials() -> Result<(), String> {
    let root = PathBuf::from("/run/cos/cron");
    reject_symlink(&root).map_err(|error| format!("inspect cron runtime: {error}"))?;
    match fs::remove_dir_all(&root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove stale cron credentials: {error}")),
    }
}

fn wait_with_timeout(
    child: std::process::Child,
    started_at: &str,
    start: &chrono::DateTime<chrono::Utc>,
    timeout_secs: u64,
) -> CronRunResult {
    wait_with_timeout_inner(child, started_at, start, timeout_secs, None)
}

fn wait_with_timeout_for_owner(
    child: std::process::Child,
    started_at: &str,
    start: &chrono::DateTime<chrono::Utc>,
    timeout_secs: u64,
    owner_uid: u32,
    child_start_time: Option<u64>,
) -> CronRunResult {
    wait_with_timeout_inner(
        child,
        started_at,
        start,
        timeout_secs,
        Some((owner_uid, child_start_time)),
    )
}

fn wait_with_timeout_inner(
    mut child: std::process::Child,
    started_at: &str,
    start: &chrono::DateTime<chrono::Utc>,
    timeout_secs: u64,
    run_identity: Option<(u32, Option<u64>)>,
) -> CronRunResult {
    use std::sync::{Arc, Mutex};
    use std::thread;

    /// Per-stream output cap. A misbehaving job can otherwise pin
    /// gigabytes in the cron driver's RSS and OOM-kill the
    /// scheduler. We cap at 1 MiB which is far larger than the
    /// 2 KiB tail we eventually keep — plenty of headroom for the
    /// `tail_string` window — and continue draining the pipe past
    /// the cap so the writer doesn't wedge on a full pipe.
    const STREAM_CAP_BYTES: usize = 1024 * 1024;

    let deadline = if timeout_secs == u64::MAX {
        None
    } else {
        Some(std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs))
    };

    // Drain stdout / stderr on background threads while we poll
    // try_wait. Without this, a job that produces more than the
    // kernel pipe buffer (~64 KiB on Linux) blocks on write to a
    // full pipe and never exits, our try_wait perpetually returns
    // None, and we falsely report "timeout" even though the command
    // would have completed if anyone had been reading its output.
    //
    // Threads exit when read sees EOF, which happens when either
    // the child exits naturally OR we kill it on timeout. They keep
    // draining past STREAM_CAP_BYTES but discard those bytes so the
    // pipe never fills and the child never blocks.
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let stdout_truncated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stderr_truncated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let drain_stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_thread = stdout.map(|mut s| {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Err(error) = set_nonblocking_fd(s.as_raw_fd()) {
                tracing::warn!(error = %error, "failed to make cron stdout nonblocking");
            }
        }
        let buf = Arc::clone(&stdout_buf);
        let truncated = Arc::clone(&stdout_truncated);
        let stop = Arc::clone(&drain_stop);
        thread::spawn(move || drain_capped(&mut s, &buf, &truncated, &stop, STREAM_CAP_BYTES))
    });
    let stderr_thread = stderr.map(|mut s| {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            if let Err(error) = set_nonblocking_fd(s.as_raw_fd()) {
                tracing::warn!(error = %error, "failed to make cron stderr nonblocking");
            }
        }
        let buf = Arc::clone(&stderr_buf);
        let truncated = Arc::clone(&stderr_truncated);
        let stop = Arc::clone(&drain_stop);
        thread::spawn(move || drain_capped(&mut s, &buf, &truncated, &stop, STREAM_CAP_BYTES))
    });

    // Poll for natural exit; break on exit or on timeout-induced kill.
    enum Reap {
        Exited(std::process::ExitStatus, bool),
        Killed,
        Errored(String),
    }
    let reap = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let terminated_descendants = run_identity
                    .map(|(owner_uid, _)| {
                        process_group_alive(child.id())
                            && terminate_known_process_group(child.id(), owner_uid)
                    })
                    .unwrap_or(false);
                break Reap::Exited(status, terminated_descendants);
            }
            Ok(None) => {
                if let Some(dl) = deadline {
                    if std::time::Instant::now() >= dl {
                        match run_identity {
                            Some((owner_uid, child_start_time)) => {
                                let _ = terminate_previous_run(
                                    child.id(),
                                    child_start_time,
                                    Some(owner_uid),
                                );
                            }
                            None => {
                                let _ = child.kill();
                            }
                        }
                        let _ = child.wait(); // reap
                        break Reap::Killed;
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => break Reap::Errored(format!("wait error: {e}")),
        }
    };

    drain_stop.store(true, std::sync::atomic::Ordering::Relaxed);
    if let Some(t) = stdout_thread {
        let _ = t.join();
    }
    if let Some(t) = stderr_thread {
        let _ = t.join();
    }
    let stdout_string =
        String::from_utf8_lossy(&stdout_buf.lock().expect("stdout buf")).into_owned();
    let stderr_string =
        String::from_utf8_lossy(&stderr_buf.lock().expect("stderr buf")).into_owned();
    let stdout_tail = if stdout_string.is_empty() {
        None
    } else {
        Some(tail_string(&stdout_string))
    };
    let stderr_tail_base = if stderr_string.is_empty() {
        None
    } else {
        Some(tail_string(&stderr_string))
    };
    let stdout_tail = if stdout_truncated.load(std::sync::atomic::Ordering::Relaxed) {
        Some(format!(
            "{}\n[truncated: stdout exceeded {} bytes]",
            stdout_tail.as_deref().unwrap_or(""),
            STREAM_CAP_BYTES
        ))
    } else {
        stdout_tail
    };
    let stderr_tail_base = if stderr_truncated.load(std::sync::atomic::Ordering::Relaxed) {
        Some(format!(
            "{}\n[truncated: stderr exceeded {} bytes]",
            stderr_tail_base.as_deref().unwrap_or(""),
            STREAM_CAP_BYTES
        ))
    } else {
        stderr_tail_base
    };

    let end = chrono::Utc::now();
    let duration = (end - *start).num_milliseconds().max(0) as u64;

    match reap {
        Reap::Exited(status, terminated_descendants) => {
            let code = status.code();
            let s = if status.success() && !terminated_descendants {
                "success"
            } else {
                "failed"
            };
            let stderr_tail = if terminated_descendants {
                Some(
                    stderr_tail_base
                        .map(|value| {
                            format!("{value}\n[terminated: background descendants outlived shell]")
                        })
                        .unwrap_or_else(|| {
                            "[terminated: background descendants outlived shell]".to_string()
                        }),
                )
            } else {
                stderr_tail_base
            };
            CronRunResult {
                started_at: started_at.to_string(),
                finished_at: Some(format_time(&end)),
                exit_code: code,
                status: s.to_string(),
                stdout_tail,
                stderr_tail,
                duration_ms: Some(duration),
                run_id: None,
                pid: None,
                pid_start_time_ticks: None,
            }
        }
        Reap::Killed => CronRunResult {
            started_at: started_at.to_string(),
            finished_at: Some(format_time(&end)),
            exit_code: None,
            status: "timeout".to_string(),
            stdout_tail,
            stderr_tail: Some(
                stderr_tail_base
                    .map(|s| format!("{s}\n[killed: timeout after {timeout_secs}s]"))
                    .unwrap_or_else(|| format!("[killed: timeout after {timeout_secs}s]")),
            ),
            duration_ms: Some(duration),
            run_id: None,
            pid: None,
            pid_start_time_ticks: None,
        },
        Reap::Errored(msg) => CronRunResult {
            started_at: started_at.to_string(),
            finished_at: Some(format_time(&end)),
            exit_code: None,
            status: "failed".to_string(),
            stdout_tail,
            stderr_tail: Some(msg),
            duration_ms: Some(duration),
            run_id: None,
            pid: None,
            pid_start_time_ticks: None,
        },
    }
}

/// Read from `s` indefinitely until EOF, but only retain the first
/// `cap` bytes in `buf`. Bytes after `cap` are read and discarded so
/// the writer never wedges on a full pipe.
fn drain_capped<R: std::io::Read>(
    s: &mut R,
    buf: &std::sync::Mutex<Vec<u8>>,
    truncated: &std::sync::atomic::AtomicBool,
    stop: &std::sync::atomic::AtomicBool,
    cap: usize,
) {
    let mut chunk = [0u8; 8192];
    loop {
        match s.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                let mut held = buf.lock().expect("drain buf");
                if held.len() < cap {
                    let want = (cap - held.len()).min(n);
                    held.extend_from_slice(&chunk[..want]);
                    if n > want {
                        truncated.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                } else {
                    truncated.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
        }
    }
}

#[cfg(unix)]
fn set_nonblocking_fd(fd: std::os::unix::io::RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Overlap checking
// ---------------------------------------------------------------------------

/// Whether the job's last-recorded run is still active.
///
/// Returns true only when the recorded pid is still alive. Crashes
/// of the cos process while a run was marked `running` would
/// otherwise leave `last_run.status == "running"` on disk forever
/// — blocking every subsequent tick under the default Skip policy.
fn is_running(job: &CronJob) -> bool {
    let Some(r) = &job.last_run else {
        return false;
    };
    if r.status != "running" {
        return false;
    }
    match r.pid {
        Some(pid) => {
            crate::proc::is_alive_with_start_time(pid, r.pid_start_time_ticks)
                || job.owner_uid.is_some_and(|uid| {
                    process_group_alive(pid) && process_group_owned_by(pid, uid)
                })
        }
        None => chrono::DateTime::parse_from_rfc3339(&r.started_at)
            .ok()
            .map(|started| chrono::Utc::now().signed_duration_since(started))
            .is_some_and(|age| {
                age >= chrono::Duration::zero() && age <= chrono::Duration::seconds(30)
            }),
    }
}

fn pid_alive(pid: u32) -> bool {
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
        let err = std::io::Error::last_os_error();
        err.raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn terminate_previous_run(
    pid: u32,
    start_time_ticks: Option<u64>,
    owner_uid: Option<u32>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let expected_start =
            start_time_ticks.ok_or_else(|| "previous run has no process start time".to_string())?;
        let owner_uid =
            owner_uid.ok_or_else(|| "previous run has no trusted owner".to_string())?;
        let leader_alive = crate::proc::is_alive_with_start_time(pid, Some(expected_start));
        if !leader_alive && !process_group_alive(pid) {
            return Ok(());
        }
        if leader_alive && process_uid(pid) != Some(owner_uid) {
            return Err(format!("previous run pid {pid} is not owned by uid {owner_uid}"));
        }
        if !process_group_owned_by(pid, owner_uid) {
            return Err(format!(
                "previous run process group {pid} contains foreign processes"
            ));
        }
        if unsafe { libc::kill(-(pid as i32), libc::SIGTERM) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(format!("signal previous run {pid}: {error}"));
            }
            return Ok(());
        }
        for _ in 0..20 {
            if !process_group_alive(pid) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if !process_group_alive(pid) {
            return Ok(());
        }
        if unsafe { libc::kill(-(pid as i32), libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(format!("kill previous run {pid}: {error}"));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, start_time_ticks, owner_uid);
        Err("overlap_policy=Kill requires Unix".to_string())
    }
}

#[cfg(unix)]
fn process_group_alive(group_leader: u32) -> bool {
    let code = unsafe { libc::kill(-(group_leader as i32), 0) };
    if code == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
fn terminate_known_process_group(group_leader: u32, owner_uid: u32) -> bool {
    if !process_group_alive(group_leader) {
        return false;
    }
    if !process_group_owned_by(group_leader, owner_uid) {
        tracing::error!(
            group_leader,
            owner_uid,
            "refusing to terminate cron process group with foreign members"
        );
        return false;
    }
    let _ = unsafe { libc::kill(-(group_leader as i32), libc::SIGTERM) };
    for _ in 0..20 {
        if !process_group_alive(group_leader) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    if process_group_alive(group_leader) {
        let _ = unsafe { libc::kill(-(group_leader as i32), libc::SIGKILL) };
    }
    true
}

#[cfg(not(unix))]
fn terminate_known_process_group(_group_leader: u32, _owner_uid: u32) -> bool {
    false
}

#[cfg(not(unix))]
fn process_group_alive(_group_leader: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn process_uid(pid: u32) -> Option<u32> {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("Uid:")
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|value| value.parse::<u32>().ok())
            })
        })
}

#[cfg(target_os = "linux")]
fn process_group_owned_by(group_leader: u32, owner_uid: u32) -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    let mut found = false;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let Some(pgrp) = stat[close + 1..]
            .split_whitespace()
            .nth(2)
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        if pgrp != group_leader {
            continue;
        }
        found = true;
        if process_uid(pid) != Some(owner_uid) {
            return false;
        }
    }
    found
}

#[cfg(not(target_os = "linux"))]
fn process_group_owned_by(_group_leader: u32, _owner_uid: u32) -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn process_uid(_pid: u32) -> Option<u32> {
    None
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
    require_or_json(Verb::TIME_CRON, Scope::wild()).map_err(|v| v.to_string())?;

    let id = args.first().ok_or(
        "usage: cos cron add <id> --schedule \"EXPR\" --command \"CMD\" [--description TEXT] \
         [--tier N] [--scope PATH] [--credentials k1,k2] [--overlap skip|queue|kill|allow] \
         [--timeout SECS]",
    )?;
    validate_id(id)?;

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
                let timeout = args[i + 1]
                    .parse::<u64>()
                    .map_err(|_| "timeout must be a positive integer (seconds)".to_string())?;
                if timeout == 0 || timeout > MAX_TIMEOUT_SECS {
                    return Err(format!(
                        "timeout must be between 1 and {MAX_TIMEOUT_SECS} seconds"
                    ));
                }
                timeout_secs = Some(timeout);
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
    let owner = current_owner()?;
    if !owner
        .caps
        .covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild))
    {
        return Err("cron owner lacks proc.spawn:* for shell execution".to_string());
    }
    let tier = match (tier, owner.tier) {
        (Some(requested), Some(parent)) if requested < parent => {
            return Err(format!(
                "cron tier {requested} would exceed owner tier {parent}"
            ));
        }
        (Some(requested), Some(_)) => Some(requested),
        (Some(_), None) => {
            return Err("cron owner session has no credential tier".to_string());
        }
        (None, parent) => parent,
    };
    if !credentials.is_empty() && tier.is_none() {
        return Err("cron credentials require a trusted owner tier".to_string());
    }

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
        owner_uid: Some(owner.uid),
        owner_home: Some(owner.home),
        caps: Some(owner.caps),
        role: owner.role,
    };

    create_job(&job)?;

    Ok(json!({
        "added": job.id,
        "schedule": job.schedule,
        "next_run": job.next_run,
    }))
}

/// Remove a cron job by ID.
fn cmd_remove(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::TIME_CRON, Scope::wild()).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron remove <id>")?;
    let owner_uid = current_owner()?.uid;
    let job = load_job(id)?;
    require_job_owner(&job, owner_uid)?;
    let path = job_path(&job.id);

    fs::remove_file(&path).map_err(|e| format!("failed to remove job: {e}"))?;

    // Optionally clean up logs
    let logs = job_logs_dir(&job.id);
    if logs.is_dir() {
        let _ = fs::remove_dir_all(&logs);
    }

    Ok(json!({ "removed": id }))
}

/// List all cron jobs with summary status.
fn cmd_list(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::TIME_CRON, Scope::wild()).map_err(|v| v.to_string())?;

    let owner_uid = current_owner()?.uid;
    let all_jobs = list_all_jobs()?;
    let legacy_unowned = all_jobs
        .iter()
        .filter(|job| job.owner_uid.is_none())
        .count();
    let jobs: Vec<_> = all_jobs
        .into_iter()
        .filter(|job| job.owner_uid == Some(owner_uid))
        .collect();
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
        "legacy_unowned": legacy_unowned,
        "migration": (legacy_unowned > 0).then_some(
            "legacy ownerless jobs are quarantined; recreate them to bind a trusted owner"
        ),
    }))
}

/// Detailed status of a specific job.
fn cmd_status(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::TIME_CRON, Scope::wild()).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron status <id>")?;
    let job = load_job(id)?;
    require_job_owner(&job, current_owner()?.uid)?;

    serde_json::to_value(&job).map_err(|e| format!("failed to serialize job: {e}"))
}

/// Enable a disabled job.
fn cmd_enable(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::TIME_CRON, Scope::wild()).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron enable <id>")?;
    let owner = current_owner()?;
    if !owner
        .caps
        .covers(&Cap::new(Verb::PROC_SPAWN, Scope::Wild))
    {
        return Err("cron owner lacks proc.spawn:* for shell execution".to_string());
    }
    let now = chrono::Utc::now();
    let job = update_job(id, |mut job| {
        require_job_owner(&job, owner.uid)?;
        match (job.tier, owner.tier) {
            (Some(job_tier), Some(owner_tier)) if job_tier < owner_tier => {
                return Err(format!(
                    "cron tier {job_tier} would exceed owner tier {owner_tier}"
                ));
            }
            (Some(_), None) => {
                return Err("cron owner session has no credential tier".to_string());
            }
            (None, Some(owner_tier)) => job.tier = Some(owner_tier),
            _ => {}
        }
        if !job.credentials.is_empty() && job.tier.is_none() {
            return Err("cron credentials require a trusted owner tier".to_string());
        }
        job.owner_home = Some(owner.home);
        job.caps = Some(owner.caps);
        job.role = owner.role;
        job.enabled = true;
        job.next_run = next_run_time(&job.schedule, &now).map(|t| format_time(&t));
        Ok(job)
    })?;

    Ok(json!({
        "id": job.id,
        "enabled": true,
    }))
}

/// Disable a job without removing it.
fn cmd_disable(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::TIME_CRON, Scope::wild()).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron disable <id>")?;
    let owner_uid = current_owner()?.uid;
    let job = update_job(id, |mut job| {
        require_job_owner(&job, owner_uid)?;
        job.enabled = false;
        job.next_run = None;
        Ok(job)
    })?;

    Ok(json!({
        "id": job.id,
        "enabled": false,
    }))
}

/// View execution history for a job.
///
/// Usage: cos cron logs <id> [--limit N]
fn cmd_logs(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::DATA_LOG_READ, Scope::wild()).map_err(|v| v.to_string())?;

    let id = args
        .first()
        .ok_or("usage: cos cron logs <id> [--limit N]")?;

    let job = load_job(id)?;
    require_job_owner(&job, current_owner()?.uid)?;

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

    let entries = load_run_logs(&job.id, limit)?;

    Ok(json!({
        "job_id": job.id,
        "entries": entries.iter().map(|r| serde_json::to_value(r).unwrap_or(json!(null))).collect::<Vec<_>>(),
        "count": entries.len(),
        "limit": limit,
    }))
}

/// Manually trigger a job immediately.
fn cmd_run(args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::PROC_SPAWN, Scope::wild()).map_err(|v| v.to_string())?;

    let id = args.first().ok_or("usage: cos cron run <id>")?;
    let owner_uid = current_owner()?.uid;

    // Phase 1: claim the run slot atomically. update_job rejects
    // any concurrent claimant who saw is_running=true under Skip.
    let previous_to_kill = std::cell::RefCell::new(None);
    let job = update_job(id, |mut job| {
        require_job_owner(&job, owner_uid)?;
        if is_running(&job) {
            match job.overlap_policy {
                OverlapPolicy::Skip => return Err("__SKIPPED__".to_string()),
                OverlapPolicy::Queue => return Err("__QUEUED__".to_string()),
                OverlapPolicy::Kill => {
                    let Some(run) = job.last_run.as_ref() else {
                        return Err("__SKIPPED__".to_string());
                    };
                    let Some(pid) = run.pid else {
                        return Err("__PENDING__".to_string());
                    };
                    *previous_to_kill.borrow_mut() = Some((pid, run.pid_start_time_ticks));
                }
                OverlapPolicy::Allow => {}
            }
        }
        let running_marker = CronRunResult {
            started_at: format_time(&chrono::Utc::now()),
            finished_at: None,
            exit_code: None,
            status: "running".to_string(),
            stdout_tail: None,
            stderr_tail: None,
            duration_ms: None,
            run_id: Some(uuid::Uuid::new_v4().simple().to_string()),
            pid: None,
            pid_start_time_ticks: None,
        };
        job.last_run = Some(running_marker);
        Ok(job)
    });
    let job = match job {
        Ok(j) => j,
        Err(e) if e == "__SKIPPED__" => {
            return Ok(json!({
                "job_id": id,
                "status": "skipped",
                "reason": "previous run is still running (overlap_policy: Skip)",
            }));
        }
        Err(e) if e == "__QUEUED__" => {
            return Ok(json!({
                "job_id": id,
                "status": "skipped",
                "reason": "previous run is still running (overlap_policy: Queue; retry later)",
            }));
        }
        Err(e) if e == "__PENDING__" => {
            return Ok(json!({
                "job_id": id,
                "status": "skipped",
                "reason": "previous run has not finished spawning",
            }));
        }
        Err(e) => return Err(e),
    };
    let run_id = job
        .last_run
        .as_ref()
        .and_then(|run| run.run_id.clone())
        .ok_or_else(|| "cron run claim has no run id".to_string())?;
    if let Some((pid, start_time)) = previous_to_kill.into_inner() {
        if let Err(error) = terminate_previous_run(pid, start_time, job.owner_uid) {
            let failure_start = chrono::Utc::now();
            let mut result = failed_run(
                &format_time(&failure_start),
                &failure_start,
                &format!("terminate previous run: {error}"),
            );
            result.run_id = Some(run_id.clone());
            let result_for_close = result.clone();
            update_job(id, |mut current| {
                if current
                    .last_run
                    .as_ref()
                    .and_then(|run| run.run_id.as_deref())
                    == Some(run_id.as_str())
                {
                    current.last_run = Some(result_for_close);
                }
                Ok(current)
            })?;
            return serde_json::to_value(&result)
                .map_err(|e| format!("failed to serialize result: {e}"));
        }
    }

    // Phase 2: execute (no lock held — the run row is the lease).
    let mut result = execute_job(&job, &run_id);
    result.run_id = Some(run_id.clone());

    // Phase 3: persist result + next_run atomically.
    let log_error = save_run_log(&job.id, &result).err();
    let result_for_close = result.clone();
    update_job(id, |mut job| {
        if job
            .last_run
            .as_ref()
            .and_then(|run| run.run_id.as_deref())
            != Some(run_id.as_str())
        {
            return Ok(job);
        }
        job.last_run = Some(result_for_close);
        let now = chrono::Utc::now();
        job.next_run = if job.enabled {
            next_run_time(&job.schedule, &now).map(|t| format_time(&t))
        } else {
            None
        };
        Ok(job)
    })?;

    let mut value =
        serde_json::to_value(&result).map_err(|e| format!("failed to serialize result: {e}"))?;
    value["log_error"] = json!(log_error);
    Ok(value)
}

/// Process all due jobs. Called by an external scheduler (e.g., systemd timer)
/// every minute.
///
/// Usage: cos cron tick
fn cmd_tick(_args: &[String]) -> Result<Value, String> {
    require_or_json(Verb::SYS_KERNEL, Scope::wild()).map_err(|v| v.to_string())?;

    let now = chrono::Utc::now();
    let tick_time = now
        .with_nanosecond(0)
        .unwrap_or(now)
        .with_second(0)
        .unwrap_or(now);
    let jobs = list_all_jobs()?;

    let mut executed: Vec<Value> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();

    for job in jobs {
        if !job.enabled {
            continue;
        }
        if !cron_matches(&job.schedule, &tick_time) {
            continue;
        }
        if job.owner_uid.is_none()
            || job.owner_home.is_none()
            || job.caps.is_none()
        {
            skipped.push(json!({
                "id": job.id,
                "reason": "legacy ownerless job is quarantined",
            }));
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

        // Phase 1: atomically claim the run slot. If another tick
        // raced us and stamped a still-live "running" marker first,
        // update_job's closure sees that and re-skips.
        let previous_to_kill = std::cell::RefCell::new(None);
        let claimed = update_job(&job.id, |mut j| {
            if !j.enabled {
                return Err("__DISABLED__".to_string());
            }
            if !cron_matches(&j.schedule, &tick_time) {
                return Err("__NOT_DUE__".to_string());
            }
            if is_running(&j) {
                match j.overlap_policy {
                    OverlapPolicy::Skip => return Err("__SKIPPED__".to_string()),
                    OverlapPolicy::Queue => return Err("__QUEUED__".to_string()),
                    OverlapPolicy::Kill => {
                        let Some(run) = j.last_run.as_ref() else {
                            return Err("__SKIPPED__".to_string());
                        };
                        let Some(pid) = run.pid else {
                            return Err("__PENDING__".to_string());
                        };
                        *previous_to_kill.borrow_mut() =
                            Some((pid, run.pid_start_time_ticks));
                    }
                    OverlapPolicy::Allow => {}
                }
            }
            let running_marker = CronRunResult {
                started_at: format_time(&now),
                finished_at: None,
                exit_code: None,
                status: "running".to_string(),
                stdout_tail: None,
                stderr_tail: None,
                duration_ms: None,
                run_id: Some(uuid::Uuid::new_v4().simple().to_string()),
                pid: None,
                pid_start_time_ticks: None,
            };
            j.last_run = Some(running_marker);
            Ok(j)
        });
        let job = match claimed {
            Ok(j) => j,
            Err(e) if e == "__DISABLED__" || e == "__NOT_DUE__" => continue,
            Err(e) if e == "__SKIPPED__" => {
                skipped.push(json!({
                    "id": job.id,
                    "reason": "previous run still running (race with concurrent tick)",
                }));
                continue;
            }
            Err(e) if e == "__QUEUED__" => {
                skipped.push(json!({
                    "id": job.id,
                    "reason": "previous run still running (overlap_policy: Queue)",
                }));
                continue;
            }
            Err(e) if e == "__PENDING__" => {
                skipped.push(json!({
                    "id": job.id,
                    "reason": "previous run has not finished spawning",
                }));
                continue;
            }
            Err(e) => {
                skipped.push(json!({
                    "id": job.id,
                    "reason": format!("failed to claim run slot: {e}"),
                }));
                continue;
            }
        };
        let run_id = match job
            .last_run
            .as_ref()
            .and_then(|run| run.run_id.clone())
        {
            Some(run_id) => run_id,
            None => {
                skipped.push(json!({
                    "id": job.id,
                    "reason": "claimed run has no run id",
                }));
                continue;
            }
        };
        if let Some((pid, start_time)) = previous_to_kill.into_inner() {
            if let Err(error) = terminate_previous_run(pid, start_time, job.owner_uid) {
                let mut result = failed_run(
                    &format_time(&now),
                    &now,
                    &format!("terminate previous run: {error}"),
                );
                result.run_id = Some(run_id.clone());
                let result_for_close = result.clone();
                if let Err(close_error) = update_job(&job.id, |mut current| {
                    if current
                        .last_run
                        .as_ref()
                        .and_then(|run| run.run_id.as_deref())
                        == Some(run_id.as_str())
                    {
                        current.last_run = Some(result_for_close);
                    }
                    Ok(current)
                }) {
                    tracing::warn!(
                        job_id = %job.id,
                        error = %close_error,
                        "failed to close cron run after overlap termination error"
                    );
                }
                skipped.push(json!({
                    "id": job.id,
                    "reason": error,
                }));
                continue;
            }
        }

        // Execute
        let mut result = execute_job(&job, &run_id);
        result.run_id = Some(run_id.clone());

        // Save log entry
        let log_error = save_run_log(&job.id, &result).err();

        // Phase 2: persist result + next_run atomically.
        let exec_status = result.status.clone();
        let result_for_close = result.clone();
        if let Err(close_error) = update_job(&job.id, |mut j| {
            if j
                .last_run
                .as_ref()
                .and_then(|run| run.run_id.as_deref())
                != Some(run_id.as_str())
            {
                return Ok(j);
            }
            j.last_run = Some(result_for_close);
            j.next_run = next_run_time(&j.schedule, &now).map(|t| format_time(&t));
            Ok(j)
        }) {
            tracing::warn!(
                job_id = %job.id,
                error = %close_error,
                "failed to persist cron run result"
            );
        }
        if let Some(error) = log_error.as_deref() {
            tracing::warn!(job_id = %job.id, error = %error, "failed to persist cron run log");
        }

        executed.push(json!({
            "id": job.id,
            "status": exec_status,
            "log_error": log_error,
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/cron.rs"
    ));
}
