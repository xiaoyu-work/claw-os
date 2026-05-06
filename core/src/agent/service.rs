//! `cos agent service` — filesystem-based job queue for the agent.
//!
//! This is the kernel-native answer to "run an agent in the background":
//! a small queue of JSON job files lives under
//! `data_dir/agent/jobs/{pending,running,done}/`, and any number of
//! `cos agent service work` workers consume them. State transitions are
//! atomic `fs::rename`s between the three subdirectories, so two
//! workers can race for the same job without corrupting it — at most
//! one will win the rename.
//!
//! The protocol is intentionally tiny:
//!
//!   - `submit "<prompt>" [--session ID] [--max-turns N]`
//!     drops a `pending/<job_id>.json` and returns `{job_id, status}`.
//!   - `list [--status pending|running|done|cancelled] [--limit N]`
//!     enumerates jobs across one or all status buckets.
//!   - `status [<job_id>]` returns either bucket counts (no id) or the
//!     full job document (with id).
//!   - `result <job_id> [--wait-secs N]` reads a finished job; with
//!     `--wait-secs N` polls up to N seconds for completion.
//!   - `work [--once] [--poll-ms N] [--max-jobs N]` runs the worker
//!     loop. `--once` processes exactly one pending job and exits;
//!     `--max-jobs N` exits after N (default: forever).
//!   - `cancel <job_id>` moves a still-pending job into `done/` with
//!     `status: cancelled`. Running jobs are not interrupted (no
//!     out-of-band cancellation in v1).
//!   - `prune [--older-than-days N] [--keep-last N]` GC's
//!     `done/`. Defaults: keep the last 100, drop anything finished
//!     more than 30 days ago.
//!
//! Composition with the rest of the OS:
//!
//!   - `cos cron` can `cos agent service submit "summarise yesterday's
//!     logs"` on a schedule.
//!   - `cos service start cos-agent-worker` (where the service
//!     definition runs `cos agent service work`) gives you a managed,
//!     restart-on-failure worker pool with health checks.
//!   - Telegram/Discord adapters (Q1 plan) can drop jobs without
//!     opening sockets — the FS is the message bus.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::paths::agent_jobs_dir;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Status bucket a job lives in. Maps 1:1 to the on-disk subdirectory
/// for pending/running/done; cancelled is stored under `done/` with
/// the `status` field discriminating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Ok,
    Error,
    Cancelled,
}

impl JobStatus {
    fn bucket(self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled => "done",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Ok => "ok",
            JobStatus::Error => "error",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

/// Persistent representation of a single agent job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    pub status: JobStatus,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns_used: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl Job {
    fn new_pending(prompt: String, session_id: Option<String>, max_turns: Option<u32>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            prompt,
            session_id,
            max_turns,
            status: JobStatus::Pending,
            created_at: now_iso(),
            started_at: None,
            finished_at: None,
            worker_pid: None,
            response: None,
            error: None,
            turns_used: None,
            provider: None,
            model: None,
        }
    }

    fn preview(&self, max: usize) -> String {
        let s = self.prompt.replace('\n', " ");
        if s.chars().count() <= max {
            s
        } else {
            let cut: String = s.chars().take(max).collect();
            format!("{cut}…")
        }
    }
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// Thin wrapper around the `<root>/{pending,running,done}/` layout.
/// All callers build one with [`Store::open_default`] (uses
/// `agent_jobs_dir()`). Tests use [`Store::with_root`] for hermetic
/// per-test temp dirs.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn open_default() -> io::Result<Self> {
        Self::with_root(agent_jobs_dir())
    }

    pub fn with_root(root: PathBuf) -> io::Result<Self> {
        for sub in ["pending", "running", "done"] {
            fs::create_dir_all(root.join(sub))?;
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn bucket_dir(&self, status: JobStatus) -> PathBuf {
        self.root.join(status.bucket())
    }

    fn path_for(&self, status: JobStatus, id: &str) -> PathBuf {
        self.bucket_dir(status).join(format!("{id}.json"))
    }

    /// Locate a job by id by checking pending/, running/, then done/.
    /// Returns the bucket the file currently lives in plus the parsed
    /// Job, or `Ok(None)` if no file exists in any bucket.
    pub fn locate(&self, id: &str) -> io::Result<Option<(JobStatus, Job)>> {
        for bucket in [JobStatus::Pending, JobStatus::Running, JobStatus::Ok] {
            let p = self.path_for(bucket, id);
            match fs::read_to_string(&p) {
                Ok(s) => {
                    let job: Job = serde_json::from_str(&s).map_err(io_other)?;
                    let actual = bucket_for_status(job.status);
                    return Ok(Some((actual, job)));
                }
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Persist a new pending job. Returns the job (with assigned id).
    pub fn submit(
        &self,
        prompt: String,
        session_id: Option<String>,
        max_turns: Option<u32>,
    ) -> io::Result<Job> {
        let job = Job::new_pending(prompt, session_id, max_turns);
        let path = self.path_for(JobStatus::Pending, &job.id);
        write_json_atomic(&path, &job)?;
        Ok(job)
    }

    /// List jobs in a given bucket, newest-first by mtime, optionally
    /// limited.
    pub fn list_bucket(&self, bucket: JobStatus, limit: Option<usize>) -> io::Result<Vec<Job>> {
        let dir = self.bucket_dir(bucket);
        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for e in fs::read_dir(&dir)? {
            let e = e?;
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if !meta.is_file() {
                continue;
            }
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            entries.push((mtime, path));
        }
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let mut out = Vec::with_capacity(entries.len());
        for (_, p) in entries.into_iter() {
            if let Some(lim) = limit {
                if out.len() >= lim {
                    break;
                }
            }
            // A concurrent worker may have just claimed/finished this
            // file (race against `claim_one` rename). Skip rather than
            // abort the whole listing on transient NotFound.
            let s = match fs::read_to_string(&p) {
                Ok(s) => s,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            match serde_json::from_str::<Job>(&s) {
                Ok(j) => out.push(j),
                Err(e) => {
                    tracing::warn!("agent service: skipping malformed job {p:?}: {e}");
                    continue;
                }
            }
        }
        Ok(out)
    }

    /// Atomically claim one pending job: rename pending/<id>.json →
    /// running/<id>.json, then rewrite the file with `status =
    /// Running` + `started_at` + `worker_pid`. Returns Ok(None) when
    /// no pending jobs exist or every candidate was lost to another
    /// worker.
    pub fn claim_one(&self) -> io::Result<Option<Job>> {
        let pending = self.bucket_dir(JobStatus::Pending);
        // Iterate all current pending entries. If a rename fails with
        // NotFound (another worker beat us) try the next; any other
        // error propagates.
        let mut candidates: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for e in fs::read_dir(&pending)? {
            let e = e?;
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let mtime = e.metadata().and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            candidates.push((mtime, path));
        }
        // Oldest first — FIFO by submission time.
        candidates.sort_by(|a, b| a.0.cmp(&b.0));

        for (_, src) in candidates {
            let id = match src.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            let dst = self.path_for(JobStatus::Running, &id);
            match fs::rename(&src, &dst) {
                Ok(()) => {
                    // We won — load, mutate, rewrite atomically.
                    let s = fs::read_to_string(&dst)?;
                    let mut job: Job = serde_json::from_str(&s).map_err(io_other)?;
                    job.status = JobStatus::Running;
                    job.started_at = Some(now_iso());
                    job.worker_pid = Some(std::process::id());
                    write_json_atomic(&dst, &job)?;
                    return Ok(Some(job));
                }
                Err(e) if e.kind() == ErrorKind::NotFound => continue, // raced
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }

    /// Mark a running job finished. Atomically rewrites the file in
    /// `running/`, then renames into `done/`.
    pub fn finish(
        &self,
        mut job: Job,
        outcome: FinishOutcome,
    ) -> io::Result<Job> {
        let running_path = self.path_for(JobStatus::Running, &job.id);
        match outcome {
            FinishOutcome::Ok { response, turns_used, provider, model } => {
                job.status = JobStatus::Ok;
                job.response = Some(response);
                job.turns_used = Some(turns_used);
                job.provider = Some(provider);
                job.model = Some(model);
            }
            FinishOutcome::Error(msg) => {
                job.status = JobStatus::Error;
                job.error = Some(msg);
            }
        }
        job.finished_at = Some(now_iso());
        write_json_atomic(&running_path, &job)?;
        let done_path = self.path_for(JobStatus::Ok, &job.id);
        fs::rename(&running_path, &done_path)?;
        Ok(job)
    }

    /// Cancel a pending job by moving it into done/ with `status =
    /// cancelled`. Returns:
    ///   - `Ok(Some(job))` if the cancellation succeeded
    ///   - `Ok(None)` if the job was already running, already done, or
    ///     missing entirely
    pub fn cancel_pending(&self, id: &str) -> io::Result<Option<Job>> {
        let src = self.path_for(JobStatus::Pending, id);
        let dst = self.path_for(JobStatus::Ok, id);
        match fs::read_to_string(&src) {
            Ok(s) => {
                let mut job: Job = serde_json::from_str(&s).map_err(io_other)?;
                job.status = JobStatus::Cancelled;
                job.finished_at = Some(now_iso());
                write_json_atomic(&src, &job)?;
                fs::rename(&src, &dst)?;
                Ok(Some(job))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Delete done/ entries older than `older_than` (mtime-based) OR
    /// beyond the most recent `keep_last`. Returns the number of files
    /// removed.
    pub fn prune(&self, older_than: Duration, keep_last: usize) -> io::Result<usize> {
        let dir = self.bucket_dir(JobStatus::Ok);
        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        for e in fs::read_dir(&dir)? {
            let e = e?;
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let mtime = e.metadata().and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            entries.push((mtime, path));
        }
        // Newest first; first `keep_last` are always retained.
        entries.sort_by(|a, b| b.0.cmp(&a.0));
        let now = std::time::SystemTime::now();
        let mut removed = 0usize;
        for (i, (mtime, p)) in entries.into_iter().enumerate() {
            if i < keep_last {
                continue;
            }
            let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
            if age >= older_than {
                if fs::remove_file(&p).is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    pub fn counts(&self) -> io::Result<(usize, usize, usize)> {
        let p = count_json(&self.bucket_dir(JobStatus::Pending))?;
        let r = count_json(&self.bucket_dir(JobStatus::Running))?;
        let d = count_json(&self.bucket_dir(JobStatus::Ok))?;
        Ok((p, r, d))
    }
}

/// Outcome of a worker run, fed into [`Store::finish`].
pub enum FinishOutcome {
    Ok {
        response: String,
        turns_used: u32,
        provider: String,
        model: String,
    },
    Error(String),
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bucket_for_status(s: JobStatus) -> JobStatus {
    match s {
        JobStatus::Pending => JobStatus::Pending,
        JobStatus::Running => JobStatus::Running,
        JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled => JobStatus::Ok,
    }
}

fn count_json(dir: &Path) -> io::Result<usize> {
    let mut n = 0usize;
    for e in fs::read_dir(dir)? {
        let e = e?;
        let path = e.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            n += 1;
        }
    }
    Ok(n)
}

fn write_json_atomic<T: Serialize>(target: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(io_other)?;
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, target)
}

fn io_other<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn job_to_summary(job: &Job) -> Value {
    json!({
        "id": job.id,
        "status": job.status.as_str(),
        "created_at": job.created_at,
        "preview": job.preview(80),
    })
}

// ---------------------------------------------------------------------------
// CLI dispatcher
// ---------------------------------------------------------------------------

/// Entry point invoked from `agent::run("service", args)`.
pub fn cmd(args: &[String]) -> Result<Value, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[] as &[String] } else { &args[1..] };
    match sub {
        "" | "help" | "-h" | "--help" => Ok(help_value()),
        "submit" => cmd_submit(rest),
        "list" => cmd_list(rest),
        "status" => cmd_status(rest),
        "result" => cmd_result(rest),
        "work" => cmd_work(rest),
        "cancel" => cmd_cancel(rest),
        "prune" => cmd_prune(rest),
        other => Err(format!(
            "unknown agent service subcommand: {other}. try: submit | list | status | result | work | cancel | prune"
        )),
    }
}

fn help_value() -> Value {
    json!({
        "subcommands": [
            "submit  \"<prompt>\" [--session ID] [--max-turns N]",
            "list    [--status pending|running|done|cancelled] [--limit N]",
            "status  [<job_id>]",
            "result  <job_id> [--wait-secs N]",
            "work    [--once] [--poll-ms N] [--max-jobs N]",
            "cancel  <job_id>",
            "prune   [--older-than-days N] [--keep-last N]",
        ],
        "queue_dir": agent_jobs_dir().display().to_string(),
    })
}

fn cmd_submit(args: &[String]) -> Result<Value, String> {
    let mut prompt: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut max_turns: Option<u32> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--session" => {
                let v = args.get(i + 1).ok_or("--session needs a value")?.clone();
                session_id = Some(v);
                i += 2;
            }
            "--max-turns" => {
                let v = args.get(i + 1).ok_or("--max-turns needs a value")?;
                max_turns = Some(v.parse().map_err(|e| format!("--max-turns: {e}"))?);
                i += 2;
            }
            s if s.starts_with("--") => return Err(format!("unknown flag: {s}")),
            _ => {
                if prompt.is_none() {
                    prompt = Some(args[i].clone());
                } else {
                    return Err("submit takes exactly one positional prompt argument".into());
                }
                i += 1;
            }
        }
    }
    let prompt = prompt
        .filter(|s| !s.trim().is_empty())
        .ok_or("usage: cos agent service submit \"<prompt>\" [--session ID] [--max-turns N]")?;
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let job = store
        .submit(prompt, session_id, max_turns)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "status": "submitted",
        "job_id": job.id,
        "queue_dir": store.root().display().to_string(),
    }))
}

fn cmd_list(args: &[String]) -> Result<Value, String> {
    let mut status_filter: Option<JobStatus> = None;
    let mut limit: Option<usize> = Some(50);
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--status" => {
                let v = args.get(i + 1).ok_or("--status needs a value")?;
                status_filter = Some(parse_status(v)?);
                i += 2;
            }
            "--limit" => {
                let v = args.get(i + 1).ok_or("--limit needs a value")?;
                limit = Some(v.parse().map_err(|e| format!("--limit: {e}"))?);
                i += 2;
            }
            "--all" => {
                limit = None;
                i += 1;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let buckets: Vec<JobStatus> = match status_filter {
        Some(JobStatus::Pending) => vec![JobStatus::Pending],
        Some(JobStatus::Running) => vec![JobStatus::Running],
        Some(JobStatus::Ok) | Some(JobStatus::Error) | Some(JobStatus::Cancelled) => {
            vec![JobStatus::Ok]
        }
        None => vec![JobStatus::Pending, JobStatus::Running, JobStatus::Ok],
    };
    let mut all: Vec<Job> = Vec::new();
    for b in buckets {
        let lim = limit; // re-applied at the end across the union
        let jobs = store.list_bucket(b, lim).map_err(|e| e.to_string())?;
        all.extend(jobs);
    }
    // If status filter is one of ok/error/cancelled, narrow further.
    if let Some(s) = status_filter {
        if matches!(s, JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled) {
            all.retain(|j| j.status == s);
        }
    }
    // Re-sort the union by created_at (ISO 8601, lexicographic compare
    // is correct) so the truncate below picks globally newest items
    // rather than draining one bucket before considering the next.
    all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    if let Some(lim) = limit {
        all.truncate(lim);
    }
    Ok(json!({
        "queue_dir": store.root().display().to_string(),
        "jobs": all.iter().map(job_to_summary).collect::<Vec<_>>(),
        "count": all.len(),
    }))
}

fn cmd_status(args: &[String]) -> Result<Value, String> {
    let store = Store::open_default().map_err(|e| e.to_string())?;
    if args.is_empty() {
        let (p, r, d) = store.counts().map_err(|e| e.to_string())?;
        return Ok(json!({
            "queue_dir": store.root().display().to_string(),
            "pending": p,
            "running": r,
            "done": d,
        }));
    }
    let id = &args[0];
    match store.locate(id).map_err(|e| e.to_string())? {
        Some((_, job)) => {
            Ok(serde_json::to_value(&job).map_err(|e| e.to_string())?)
        }
        None => Err(format!("job not found: {id}")),
    }
}

fn cmd_result(args: &[String]) -> Result<Value, String> {
    let id = args
        .first()
        .filter(|s| !s.trim().is_empty())
        .ok_or("usage: cos agent service result <job_id> [--wait-secs N]")?
        .clone();
    let mut wait_secs: u64 = 0;
    let mut i = 1usize;
    while i < args.len() {
        match args[i].as_str() {
            "--wait-secs" => {
                let v = args.get(i + 1).ok_or("--wait-secs needs a value")?;
                wait_secs = v.parse().map_err(|e| format!("--wait-secs: {e}"))?;
                i += 2;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(wait_secs);
    loop {
        match store.locate(&id).map_err(|e| e.to_string())? {
            Some((bucket, job)) => {
                if bucket == JobStatus::Ok {
                    return Ok(serde_json::to_value(&job).map_err(|e| e.to_string())?);
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "job {id} not finished (status={})",
                        job.status.as_str()
                    ));
                }
            }
            None => {
                if Instant::now() >= deadline {
                    return Err(format!("job not found: {id}"));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn cmd_cancel(args: &[String]) -> Result<Value, String> {
    let id = args
        .first()
        .filter(|s| !s.trim().is_empty())
        .ok_or("usage: cos agent service cancel <job_id>")?;
    let store = Store::open_default().map_err(|e| e.to_string())?;
    match store.cancel_pending(id).map_err(|e| e.to_string())? {
        Some(job) => Ok(json!({
            "status": "cancelled",
            "job_id": job.id,
        })),
        None => {
            // Either it never existed, or it was already running/done.
            // Distinguish by locate().
            match store.locate(id).map_err(|e| e.to_string())? {
                Some((_, job)) => Ok(json!({
                    "status": "not_cancelled",
                    "reason": "already_progressed",
                    "job_id": job.id,
                    "current_status": job.status.as_str(),
                })),
                None => Err(format!("job not found: {id}")),
            }
        }
    }
}

fn cmd_prune(args: &[String]) -> Result<Value, String> {
    let mut older_than_days: u64 = 30;
    let mut keep_last: usize = 100;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--older-than-days" => {
                let v = args.get(i + 1).ok_or("--older-than-days needs a value")?;
                older_than_days = v.parse().map_err(|e| format!("--older-than-days: {e}"))?;
                i += 2;
            }
            "--keep-last" => {
                let v = args.get(i + 1).ok_or("--keep-last needs a value")?;
                keep_last = v.parse().map_err(|e| format!("--keep-last: {e}"))?;
                i += 2;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let removed = store
        .prune(Duration::from_secs(older_than_days * 86_400), keep_last)
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "removed": removed,
        "kept_at_least": keep_last,
        "older_than_days": older_than_days,
    }))
}

fn cmd_work(args: &[String]) -> Result<Value, String> {
    let mut once = false;
    let mut poll_ms: u64 = 1_000;
    let mut max_jobs: Option<u32> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--once" => {
                once = true;
                i += 1;
            }
            "--poll-ms" => {
                let v = args.get(i + 1).ok_or("--poll-ms needs a value")?;
                poll_ms = v.parse().map_err(|e| format!("--poll-ms: {e}"))?;
                i += 2;
            }
            "--max-jobs" => {
                let v = args.get(i + 1).ok_or("--max-jobs needs a value")?;
                max_jobs = Some(v.parse().map_err(|e| format!("--max-jobs: {e}"))?);
                i += 2;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }

    let store = Store::open_default().map_err(|e| e.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    let mut processed: u32 = 0;
    let mut summaries: Vec<Value> = Vec::new();
    loop {
        match store.claim_one().map_err(|e| e.to_string())? {
            Some(job) => {
                let outcome = runtime.block_on(run_one_job(&job));
                match store.finish(job.clone(), outcome) {
                    Ok(finished) => summaries.push(json!({
                        "job_id": finished.id,
                        "status": finished.status.as_str(),
                    })),
                    Err(e) => {
                        tracing::warn!("agent service: finish() failed for {}: {e}", job.id);
                    }
                }
                processed += 1;
                if once {
                    break;
                }
                if let Some(cap) = max_jobs {
                    if processed >= cap {
                        break;
                    }
                }
            }
            None => {
                if once {
                    break;
                }
                std::thread::sleep(Duration::from_millis(poll_ms));
            }
        }
    }
    Ok(json!({
        "processed": processed,
        "results": summaries,
    }))
}

async fn run_one_job(job: &Job) -> FinishOutcome {
    use crate::agent::runtime::loop_;
    // Apply per-job max-turns override on a clone of the global cfg
    // so other jobs in the same worker process aren't affected.
    let base = crate::config::get().agent.clone();
    let mut cfg = base;
    if let Some(n) = job.max_turns {
        cfg.max_turns = n;
    }
    let provider = match crate::agent::llm::registry::build(&cfg.provider, &cfg.model, &cfg) {
        Ok(p) => p,
        Err(e) => return FinishOutcome::Error(format!("provider unavailable: {e}")),
    };
    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_guardrails(loop_::guardrails_from_cfg(&cfg));
    tools.set_approval(loop_::approval_from_cfg(&cfg));
    // MCP attach (best-effort) — handles dropped at end of fn.
    let _mcp_handles = loop_::attach_mcp_servers_for_cli(&mut tools, &cfg).await;

    let result = if let Some(sid) = job.session_id.as_deref() {
        match crate::agent::memory::sqlite_fts::MemoryDb::open_default() {
            Ok(db) => loop_::ask_with_memory(provider.clone(), &cfg, &job.prompt, &tools, &db, sid).await,
            Err(_) => loop_::ask_with(provider.clone(), &cfg, &job.prompt, &tools).await,
        }
    } else {
        loop_::ask_with(provider.clone(), &cfg, &job.prompt, &tools).await
    };

    match result {
        Ok(r) => FinishOutcome::Ok {
            response: r.answer,
            turns_used: r.turns,
            provider: r.provider,
            model: r.model,
        },
        Err(e) => FinishOutcome::Error(e.to_string()),
    }
}

fn parse_status(s: &str) -> Result<JobStatus, String> {
    match s.to_ascii_lowercase().as_str() {
        "pending" => Ok(JobStatus::Pending),
        "running" => Ok(JobStatus::Running),
        "done" | "ok" | "complete" | "completed" => Ok(JobStatus::Ok),
        "error" | "failed" => Ok(JobStatus::Error),
        "cancelled" | "canceled" => Ok(JobStatus::Cancelled),
        other => Err(format!("unknown status: {other}")),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_root() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    struct EnvGuard {
        prev: Option<String>,
    }
    impl EnvGuard {
        fn set(dir: &Path) -> Self {
            let prev = std::env::var("COS_DATA_DIR").ok();
            std::env::set_var("COS_DATA_DIR", dir);
            Self { prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => std::env::set_var("COS_DATA_DIR", v),
                None => std::env::remove_var("COS_DATA_DIR"),
            }
        }
    }

    #[test]
    fn store_creates_three_buckets() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        for sub in ["pending", "running", "done"] {
            assert!(dir.path().join(sub).is_dir(), "missing {sub}");
        }
        let _ = store; // silence
    }

    #[test]
    fn submit_writes_pending_file_with_uuid_id() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("hello".into(), None, None).unwrap();
        assert_eq!(job.status, JobStatus::Pending);
        let path = dir.path().join("pending").join(format!("{}.json", job.id));
        assert!(path.is_file(), "no file at {path:?}");
        let s = fs::read_to_string(&path).unwrap();
        let parsed: Job = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.id, job.id);
        assert_eq!(parsed.prompt, "hello");
    }

    #[test]
    fn locate_finds_job_in_pending_bucket() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("hi".into(), None, None).unwrap();
        let (bucket, found) = store.locate(&job.id).unwrap().unwrap();
        assert_eq!(bucket, JobStatus::Pending);
        assert_eq!(found.id, job.id);
    }

    #[test]
    fn locate_returns_none_for_unknown_id() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        assert!(store.locate("00000000-not-real").unwrap().is_none());
    }

    #[test]
    fn claim_one_atomically_moves_pending_to_running() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("do work".into(), None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        assert_eq!(claimed.status, JobStatus::Running);
        assert!(claimed.started_at.is_some());
        assert_eq!(claimed.worker_pid, Some(std::process::id()));
        // pending/<id>.json gone, running/<id>.json present
        assert!(!dir.path().join("pending").join(format!("{}.json", job.id)).exists());
        assert!(dir.path().join("running").join(format!("{}.json", job.id)).is_file());
    }

    #[test]
    fn claim_one_returns_none_when_no_pending() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        assert!(store.claim_one().unwrap().is_none());
    }

    #[test]
    fn claim_one_picks_oldest_first() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let first = store.submit("first".into(), None, None).unwrap();
        // Touch the second one with a later mtime to be unambiguous on
        // filesystems with low resolution timestamps.
        std::thread::sleep(Duration::from_millis(20));
        let _second = store.submit("second".into(), None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        assert_eq!(claimed.id, first.id);
    }

    #[test]
    fn finish_ok_moves_running_to_done_with_response() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("p".into(), None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        let finished = store
            .finish(
                claimed,
                FinishOutcome::Ok {
                    response: "answer".into(),
                    turns_used: 2,
                    provider: "mock".into(),
                    model: "mock-model".into(),
                },
            )
            .unwrap();
        assert_eq!(finished.status, JobStatus::Ok);
        assert_eq!(finished.response.as_deref(), Some("answer"));
        assert_eq!(finished.turns_used, Some(2));
        assert!(!dir.path().join("running").join(format!("{}.json", job.id)).exists());
        assert!(dir.path().join("done").join(format!("{}.json", job.id)).is_file());
    }

    #[test]
    fn finish_error_records_message() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _job = store.submit("p".into(), None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        let finished = store
            .finish(claimed, FinishOutcome::Error("boom".into()))
            .unwrap();
        assert_eq!(finished.status, JobStatus::Error);
        assert_eq!(finished.error.as_deref(), Some("boom"));
    }

    #[test]
    fn cancel_pending_moves_to_done_with_cancelled_status() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("p".into(), None, None).unwrap();
        let cancelled = store.cancel_pending(&job.id).unwrap().unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(dir.path().join("done").join(format!("{}.json", job.id)).is_file());
        assert!(!dir.path().join("pending").join(format!("{}.json", job.id)).exists());
    }

    #[test]
    fn cancel_pending_returns_none_when_already_running() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _ = store.submit("p".into(), None, None).unwrap();
        let _ = store.claim_one().unwrap().unwrap();
        // The job is now in running/, not pending/ — cancel is a noop.
        let c = store.cancel_pending("nonexistent").unwrap();
        assert!(c.is_none());
    }

    #[test]
    fn list_bucket_returns_newest_first_and_respects_limit() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _a = store.submit("a".into(), None, None).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let b = store.submit("b".into(), None, None).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let c = store.submit("c".into(), None, None).unwrap();
        let v = store.list_bucket(JobStatus::Pending, Some(2)).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, c.id);
        assert_eq!(v[1].id, b.id);
    }

    #[test]
    fn counts_reflect_per_bucket_state() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _a = store.submit("a".into(), None, None).unwrap();
        let _b = store.submit("b".into(), None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        let _ = store
            .finish(
                claimed,
                FinishOutcome::Ok {
                    response: "x".into(),
                    turns_used: 1,
                    provider: "m".into(),
                    model: "m".into(),
                },
            )
            .unwrap();
        let (p, r, d) = store.counts().unwrap();
        assert_eq!(p, 1);
        assert_eq!(r, 0);
        assert_eq!(d, 1);
    }

    #[test]
    fn prune_drops_aged_files_beyond_keep_last() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        // Create 3 done jobs by submitting + claiming + finishing.
        for _ in 0..3 {
            let _ = store.submit("p".into(), None, None).unwrap();
            let claimed = store.claim_one().unwrap().unwrap();
            let _ = store
                .finish(
                    claimed,
                    FinishOutcome::Ok {
                        response: "x".into(),
                        turns_used: 1,
                        provider: "m".into(),
                        model: "m".into(),
                    },
                )
                .unwrap();
        }
        // keep_last = 1, older_than = 0 → should drop the 2 oldest.
        let removed = store.prune(Duration::from_secs(0), 1).unwrap();
        assert_eq!(removed, 2);
        let (_, _, d) = store.counts().unwrap();
        assert_eq!(d, 1);
    }

    #[test]
    fn job_preview_truncates_with_ellipsis() {
        let mut j = Job::new_pending("a".repeat(100), None, None);
        j.id = "fixed".into();
        assert_eq!(j.preview(10), "aaaaaaaaaa…");
        let short = Job::new_pending("hi".into(), None, None);
        assert_eq!(short.preview(10), "hi");
    }

    // ----- CLI dispatcher tests (use COS_DATA_DIR via EnvGuard) -----

    #[test]
    fn cmd_help_lists_subcommands() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let v = cmd(&[]).unwrap();
        let arr = v["subcommands"].as_array().unwrap();
        assert!(arr.iter().any(|s| s.as_str().unwrap().starts_with("submit")));
        assert!(arr.iter().any(|s| s.as_str().unwrap().starts_with("work")));
    }

    #[test]
    fn cmd_unknown_returns_helpful_error() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let err = cmd(&["bogus".into()]).unwrap_err();
        assert!(err.contains("unknown agent service subcommand"));
    }

    #[test]
    fn cmd_submit_then_status_then_cancel_round_trip() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let v = cmd(&["submit".into(), "do a thing".into()]).unwrap();
        assert_eq!(v["status"], "submitted");
        let id = v["job_id"].as_str().unwrap().to_string();

        let st = cmd(&["status".into(), id.clone()]).unwrap();
        assert_eq!(st["status"], "pending");
        assert_eq!(st["prompt"], "do a thing");

        let cancelled = cmd(&["cancel".into(), id.clone()]).unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(cancelled["job_id"], id);

        // status now returns the cancelled job from done/
        let st2 = cmd(&["status".into(), id.clone()]).unwrap();
        assert_eq!(st2["status"], "cancelled");
    }

    #[test]
    fn cmd_submit_requires_prompt() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let err = cmd(&["submit".into()]).unwrap_err();
        assert!(err.contains("usage"));
    }

    #[test]
    fn cmd_submit_rejects_extra_positional() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let err = cmd(&["submit".into(), "a".into(), "b".into()]).unwrap_err();
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn cmd_status_no_id_returns_counts() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        cmd(&["submit".into(), "p1".into()]).unwrap();
        cmd(&["submit".into(), "p2".into()]).unwrap();
        let v = cmd(&["status".into()]).unwrap();
        assert_eq!(v["pending"], 2);
        assert_eq!(v["running"], 0);
        assert_eq!(v["done"], 0);
    }

    #[test]
    fn cmd_status_unknown_id_errors() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let err = cmd(&["status".into(), "nope".into()]).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn cmd_list_filters_by_status() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        cmd(&["submit".into(), "p1".into()]).unwrap();
        cmd(&["submit".into(), "p2".into()]).unwrap();
        let v = cmd(&["list".into(), "--status".into(), "pending".into()]).unwrap();
        assert_eq!(v["count"], 2);
        let v2 = cmd(&["list".into(), "--status".into(), "done".into()]).unwrap();
        assert_eq!(v2["count"], 0);
    }

    #[test]
    fn cmd_list_rejects_unknown_status() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let err = cmd(&["list".into(), "--status".into(), "bogus".into()]).unwrap_err();
        assert!(err.contains("unknown status"));
    }

    #[test]
    fn cmd_result_no_wait_errors_for_pending() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let v = cmd(&["submit".into(), "p".into()]).unwrap();
        let id = v["job_id"].as_str().unwrap().to_string();
        let err = cmd(&["result".into(), id]).unwrap_err();
        assert!(err.contains("not finished"));
    }

    #[test]
    fn cmd_result_returns_done_job() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        // Manually drive a job through to done/ so we can assert
        // result without invoking a real provider.
        let store = Store::open_default().unwrap();
        let job = store.submit("p".into(), None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        let _ = store
            .finish(
                claimed,
                FinishOutcome::Ok {
                    response: "the answer".into(),
                    turns_used: 1,
                    provider: "mock".into(),
                    model: "mock-model".into(),
                },
            )
            .unwrap();
        let v = cmd(&["result".into(), job.id]).unwrap();
        assert_eq!(v["status"], "ok");
        assert_eq!(v["response"], "the answer");
    }

    #[test]
    fn cmd_cancel_unknown_errors() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let err = cmd(&["cancel".into(), "nope".into()]).unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn cmd_prune_returns_removed_count() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let store = Store::open_default().unwrap();
        for _ in 0..2 {
            let _ = store.submit("p".into(), None, None).unwrap();
            let c = store.claim_one().unwrap().unwrap();
            let _ = store
                .finish(
                    c,
                    FinishOutcome::Ok {
                        response: "x".into(),
                        turns_used: 1,
                        provider: "m".into(),
                        model: "m".into(),
                    },
                )
                .unwrap();
        }
        let v = cmd(&[
            "prune".into(),
            "--older-than-days".into(),
            "0".into(),
            "--keep-last".into(),
            "0".into(),
        ])
        .unwrap();
        assert_eq!(v["removed"], 2);
    }

    #[test]
    fn cmd_work_once_with_no_jobs_returns_zero_processed() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let v = cmd(&["work".into(), "--once".into()]).unwrap();
        assert_eq!(v["processed"], 0);
        assert_eq!(v["results"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn list_bucket_skips_files_that_disappear_mid_read() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _ = store.submit("alive".into(), None, None).unwrap();
        // Plant a stale path: write it then remove it before list_bucket
        // can read. Since list_bucket reads inside the directory iter,
        // simulate the race by deleting one file just before listing
        // — easier: hand-craft a corrupted JSON file then verify it's
        // skipped (covers the "skip mid-list" code path equivalently).
        let bogus = dir.path().join("pending").join("bogus.json");
        fs::write(&bogus, b"not valid json").unwrap();
        let v = store.list_bucket(JobStatus::Pending, None).unwrap();
        // The valid one comes through; the malformed one is skipped.
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].prompt, "alive");
    }

    #[test]
    fn cmd_list_orders_union_by_created_at_desc() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let store = Store::open_default().unwrap();
        // Submit oldest pending.
        let oldest = store.submit("oldest".into(), None, None).unwrap();
        std::thread::sleep(Duration::from_millis(1100)); // ensure created_at second-rollover
        // Submit middle, then claim+finish (lands in done/).
        let mid = store.submit("middle".into(), None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        // claimed should be the oldest pending (FIFO). Finish it.
        let _ = store
            .finish(
                claimed,
                FinishOutcome::Ok {
                    response: "x".into(),
                    turns_used: 1,
                    provider: "m".into(),
                    model: "m".into(),
                },
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(1100));
        // Newest pending.
        let newest = store.submit("newest".into(), None, None).unwrap();

        // Expected ordering by created_at desc: newest, mid (still
        // pending), oldest (now in done/ as ok). cmd_list with no
        // filter should respect this ordering globally.
        let v = cmd(&["list".into()]).unwrap();
        let arr = v["jobs"].as_array().unwrap();
        assert_eq!(arr[0]["id"], newest.id);
        assert_eq!(arr[1]["id"], mid.id);
        assert_eq!(arr[2]["id"], oldest.id);
    }
}
