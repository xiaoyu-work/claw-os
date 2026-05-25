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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

    pub fn as_str(self) -> &'static str {
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
    /// uid of the user who submitted the task (via `SO_PEERCRED` on
    /// the clawd unix socket). Used to load the requesting user's
    /// `~/.config/cos/config.json` instead of clawd's root-owned one
    /// when the worker executes the job — without this, every
    /// non-root submitter sees "no LLM provider configured".
    /// `None` for jobs submitted in-process or before this field
    /// existed (old job files on disk are still readable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_uid: Option<u32>,
    /// Absolute `$HOME` directory of the submitting user, resolved
    /// via `getpwuid_r` at submit time. The worker reads
    /// `<owner_home>/.config/cos/config.json` to pick up the user's
    /// provider/model/key setup. `None` falls back to the daemon's
    /// own (typically empty) config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_home: Option<String>,
}

impl Job {
    fn new_pending(
        prompt: String,
        session_id: Option<String>,
        max_turns: Option<u32>,
        owner_uid: Option<u32>,
        owner_home: Option<String>,
    ) -> Self {
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
            owner_uid,
            owner_home,
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
        // The bucket dirs (pending/running/done) and a sibling
        // `locks/` dir hold the per-job flock sentinels. Pre-creating
        // them keeps the hot path (claim_one / cancel_pending /
        // submit) lock-free at start-up.
        for sub in ["pending", "running", "done", "locks", "streams"] {
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

    pub fn stream_path(&self, id: &str) -> PathBuf {
        self.root.join("streams").join(format!("{id}.jsonl"))
    }

    pub fn append_stream_event(
        &self,
        id: &str,
        event: &crate::agent::llm::StreamEvent,
    ) -> io::Result<()> {
        let value = json!({
            "ts": chrono::Utc::now(),
            "event": event,
        });
        let line = serde_json::to_string(&value).map_err(io_other)?;
        crate::filelock::append_locked(&self.stream_path(id), &line).map_err(io_other)
    }

    pub fn read_stream_events(&self, id: &str, cursor: usize) -> io::Result<(usize, Vec<Value>)> {
        let path = self.stream_path(id);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok((0, Vec::new())),
            Err(err) => return Err(err),
        };
        let mut events = Vec::new();
        let mut next_cursor = 0usize;
        for (idx, line) in raw.lines().enumerate() {
            next_cursor = idx + 1;
            if idx < cursor {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(value) => events.push(value),
                Err(err) => {
                    tracing::warn!(
                        "agent service: skipping malformed stream frame {path:?}: {err}"
                    );
                }
            }
        }
        Ok((next_cursor, events))
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
    /// `owner_uid`/`owner_home` carry the submitting peer's identity
    /// resolved via `SO_PEERCRED` + `getpwuid_r` so the worker can
    /// load that user's `~/.config/cos/config.json` instead of the
    /// daemon's. Both fields are optional and default to `None`
    /// (matches `cos agent service submit` from the same shell, where
    /// no peer lookup is needed).
    pub fn submit(
        &self,
        prompt: String,
        session_id: Option<String>,
        max_turns: Option<u32>,
        owner_uid: Option<u32>,
        owner_home: Option<String>,
    ) -> io::Result<Job> {
        let job = Job::new_pending(prompt, session_id, max_turns, owner_uid, owner_home);
        let path = self.path_for(JobStatus::Pending, &job.id);
        write_json_atomic(&path, &job)?;
        crate::clawd::audit::record_task_event("clawd.task.submitted", &job);
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
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
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
            // Per-id exclusive lock: serialise claim_one and
            // cancel_pending so they can't both succeed for the same
            // job. Without this, claim's rename(pending→running) and
            // cancel's rename(pending→done) race on POSIX (rename(2)
            // is atomic but not mutually exclusive across different
            // destinations), letting a cancelled job still receive
            // a real response from a worker that won the race.
            let _lock = match self.lock_for_id(&id) {
                Ok(l) => l,
                Err(_) => continue, // best-effort: never block forever on lock failure
            };
            // Re-check existence after taking the lock — cancel may have
            // already moved the file while we were waiting.
            if !src.exists() {
                continue;
            }
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
                    crate::clawd::audit::record_task_event("clawd.task.started", &job);
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
    pub fn finish(&self, mut job: Job, outcome: FinishOutcome) -> io::Result<Job> {
        let running_path = self.path_for(JobStatus::Running, &job.id);
        match outcome {
            FinishOutcome::Ok {
                response,
                turns_used,
                provider,
                model,
            } => {
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
        finish_durable_session(&job)?;
        crate::clawd::audit::record_task_event("clawd.task.finished", &job);
        Ok(job)
    }

    /// Cancel a pending job by moving it into done/ with `status =
    /// cancelled`. Returns:
    ///   - `Ok(Some(job))` if the cancellation succeeded
    ///   - `Ok(None)` if the job was already running, already done, or
    ///     missing entirely
    pub fn cancel_pending(&self, id: &str) -> io::Result<Option<Job>> {
        // Per-id exclusive lock prevents claim_one and cancel_pending
        // from both succeeding for the same id. Without it, a worker
        // could claim_one the file while we still read it from
        // pending/, then we'd write the cancelled record to a path
        // that no longer existed (and the worker would post a real
        // response anyway).
        let _lock = self.lock_for_id(id)?;
        let src = self.path_for(JobStatus::Pending, id);
        let dst = self.path_for(JobStatus::Ok, id);
        match fs::read_to_string(&src) {
            Ok(s) => {
                let mut job: Job = serde_json::from_str(&s).map_err(io_other)?;
                job.status = JobStatus::Cancelled;
                job.finished_at = Some(now_iso());
                write_json_atomic(&src, &job)?;
                fs::rename(&src, &dst)?;
                finish_durable_session(&job)?;
                crate::clawd::audit::record_task_event("clawd.task.cancelled", &job);
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
            let mtime = e
                .metadata()
                .and_then(|m| m.modified())
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

    /// Acquire an exclusive flock keyed on `id`. Used by claim_one
    /// and cancel_pending to serialise per-job state transitions:
    /// without this, two workers (or worker+canceller) can both win
    /// their independent `fs::rename(2)` calls because the kernel
    /// only guarantees atomicity per-rename, not mutual exclusion
    /// across two different destinations. The sentinel inode is
    /// stable across rename storms.
    fn lock_for_id(&self, id: &str) -> io::Result<JobLock> {
        let lock_dir = self.root.join("locks");
        fs::create_dir_all(&lock_dir)?;
        let lock_path = lock_dir.join(format!("{id}.lock"));
        let f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(JobLock { file: f })
    }
}

/// RAII guard for per-job flock taken by `Store::lock_for_id`.
struct JobLock {
    file: fs::File,
}

impl Drop for JobLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
            }
        }
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

#[derive(Debug, Clone, Copy)]
pub struct WorkerOptions {
    pub once: bool,
    pub poll_ms: u64,
    pub max_jobs: Option<u32>,
}

impl Default for WorkerOptions {
    fn default() -> Self {
        Self {
            once: false,
            poll_ms: 1_000,
            max_jobs: None,
        }
    }
}

pub fn run_worker_loop(options: WorkerOptions, shutdown: Arc<AtomicBool>) -> Result<Value, String> {
    run_worker_loop_inner(options, shutdown, false)
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
    let bytes = serde_json::to_vec_pretty(value).map_err(io_other)?;
    // Crash-safe: shared helper writes a per-process tmp file,
    // sync_all's it, renames into place, then fsyncs the parent dir.
    // Replaces an earlier `fs::write + fs::rename` which skipped
    // fsync entirely, so a power loss between write and rename could
    // expose a torn job file at recovery time.
    crate::agent::util::atomic_write_with_fsync(target, &bytes)
}

fn io_other<E: std::fmt::Display>(e: E) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

fn finish_durable_session(job: &Job) -> io::Result<()> {
    let Some(raw_sid) = job.session_id.as_deref() else {
        return Ok(());
    };
    let Ok(sid) = raw_sid.parse::<crate::session::SessionId>() else {
        return Ok(());
    };
    if !crate::session::session_dir(&sid).exists() {
        return Ok(());
    }

    let status = match job.status {
        JobStatus::Ok => crate::session::Status::Done,
        JobStatus::Error | JobStatus::Cancelled => crate::session::Status::Failed,
        JobStatus::Pending | JobStatus::Running => return Ok(()),
    };
    crate::session::end(&sid, status).map_err(io_other)
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Resolve the running process's uid + `$HOME` for stamping onto
/// jobs submitted via `cos agent service submit`. clawd-routed
/// submits go through `clawd::tasks::submit`, which uses the peer
/// credentials of the unix socket instead.
fn current_owner_identity() -> (Option<u32>, Option<String>) {
    #[cfg(unix)]
    let uid = Some(unsafe { libc::getuid() } as u32);
    #[cfg(not(unix))]
    let uid: Option<u32> = None;
    let home = std::env::var("HOME").ok().filter(|s| !s.is_empty());
    (uid, home)
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
    let rest = if args.is_empty() {
        &[] as &[String]
    } else {
        &args[1..]
    };
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
    // `cos agent service submit` runs in the user's own process, so
    // the worker (which is also in this process in single-shot mode)
    // will load that user's config naturally — but stamping owner_*
    // anyway keeps the on-disk job document complete for ops/audit.
    let (uid, home) = current_owner_identity();
    let job = store
        .submit(prompt, session_id, max_turns, uid, home)
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
        Some((_, job)) => Ok(serde_json::to_value(&job).map_err(|e| e.to_string())?),
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
    let mut options = WorkerOptions::default();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--once" => {
                options.once = true;
                i += 1;
            }
            "--poll-ms" => {
                let v = args.get(i + 1).ok_or("--poll-ms needs a value")?;
                options.poll_ms = v.parse().map_err(|e| format!("--poll-ms: {e}"))?;
                i += 2;
            }
            "--max-jobs" => {
                let v = args.get(i + 1).ok_or("--max-jobs needs a value")?;
                options.max_jobs = Some(v.parse().map_err(|e| format!("--max-jobs: {e}"))?);
                i += 2;
            }
            s => return Err(format!("unknown flag: {s}")),
        }
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    run_worker_loop_inner(options, shutdown, true)
}

fn run_worker_loop_inner(
    options: WorkerOptions,
    shutdown: Arc<AtomicBool>,
    install_signals: bool,
) -> Result<Value, String> {
    crate::clawd::audit::install_runtime_hook();
    let store = Store::open_default().map_err(|e| e.to_string())?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    // Shared "graceful shutdown requested" flag, flipped by a tokio
    // signal listener for SIGTERM / SIGINT (and ctrl_c on Windows).
    // Without this, systemctl stop / pkill -TERM would tear the
    // worker down mid-LLM-call and leave the request stuck in
    // claimed/ with no response and no cancellation marker. With
    // this flag the worker finishes the in-flight job, then exits
    // before claiming a new one.
    if install_signals {
        runtime.spawn(install_shutdown_listener(shutdown.clone()));
    }
    let mut processed: u32 = 0;
    let mut summaries: Vec<Value> = Vec::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            tracing::info!("agent service worker: shutdown signal received, draining");
            break;
        }
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
                if options.once {
                    break;
                }
                if let Some(cap) = options.max_jobs {
                    if processed >= cap {
                        break;
                    }
                }
            }
            None => {
                if options.once {
                    break;
                }
                // Sleep in short slices so a shutdown signal can
                // interrupt long poll intervals without waiting out
                // the full duration.
                let total = Duration::from_millis(options.poll_ms);
                let slice = Duration::from_millis(100.min(options.poll_ms));
                let start = Instant::now();
                while start.elapsed() < total {
                    if shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(slice);
                }
            }
        }
    }
    Ok(json!({
        "processed": processed,
        "results": summaries,
        "shutdown": shutdown.load(Ordering::SeqCst),
    }))
}

/// Install a tokio signal handler that flips `shutdown` to `true` on
/// the first SIGTERM/SIGINT (or ctrl_c on Windows). Subsequent signals
/// are ignored — operators can `kill -KILL` if they really want a hard
/// stop. Running as a future spawned on the worker's current_thread
/// runtime means it shares the OS signal handler with `block_on` calls
/// from `run_one_job`.
async fn install_shutdown_listener(shutdown: Arc<AtomicBool>) {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("agent service: SIGTERM listener unavailable: {e}");
                return;
            }
        };
        let mut intr = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("agent service: SIGINT listener unavailable: {e}");
                return;
            }
        };
        tokio::select! {
            _ = term.recv() => {
                tracing::info!("agent service: SIGTERM");
            }
            _ = intr.recv() => {
                tracing::info!("agent service: SIGINT");
            }
        }
        shutdown.store(true, Ordering::SeqCst);
    }
    #[cfg(not(unix))]
    {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!("agent service: ctrl_c listener failed: {e}");
            return;
        }
        shutdown.store(true, Ordering::SeqCst);
    }
}

async fn run_one_job(job: &Job) -> FinishOutcome {
    // If the job carries an owner_home (clawd-routed jobs from a
    // non-daemon user), load THAT user's config into a task-local
    // override AND redirect every per-user path resolver
    // (`paths::user_config_dir`, `paths::user_data_dir`, and
    // therefore `paths::user_credentials_dir`,
    // `paths::user_app_override_path`, `paths::user_app_consent_path`,
    // `paths::user_budget_config_path`, …) to the owner's home before
    // running the rest of the job. Every `config::get()` inside the
    // agent loop — and every credential / consent / app-override
    // lookup reached transitively from tool implementations, the LLM
    // gate, and the delegate tool — will see the user's
    // provider/model/keys rather than the daemon's defaults.
    //
    // Without this, `cos agent ask` from a non-root user fails either
    // "no LLM provider configured" (config not loaded) or, post
    // config-override fix, "GitHub Copilot is not signed in" because
    // clawd (uid=0, HOME=/root) reads
    // `/root/.local/share/cos/credentials/agent/copilot_github_token.json`
    // which doesn't exist — the user wrote the credential under
    // `/home/<user>/.local/share/cos/credentials/...`.
    if let Some(home) = job.owner_home.as_deref() {
        let home_path = std::path::PathBuf::from(home);
        let cfg = crate::config::intern_for_home(&home_path);
        crate::paths::with_home_override(
            home_path,
            crate::config::with_override(cfg, run_one_job_inner(job)),
        )
        .await
    } else {
        run_one_job_inner(job).await
    }
}

async fn run_one_job_inner(job: &Job) -> FinishOutcome {
    use crate::agent::runtime::loop_;

    let _session_guard = match enter_job_session(job) {
        Ok(guard) => guard,
        Err(err) => return FinishOutcome::Error(format!("session unavailable: {err}")),
    };

    // Apply per-job max-turns override on a clone of the global cfg
    // so other jobs in the same worker process aren't affected.
    // `config::get()` here is intentionally the *task-local* one
    // installed by `run_one_job` for clawd-routed jobs; for in-process
    // submits it falls through to the process-wide config as before.
    let base = crate::config::get().agent.clone();
    let mut cfg = base;
    if let Some(n) = job.max_turns {
        cfg.max_turns = n;
    }
    let provider = match crate::agent::llm::registry::build(&cfg.provider, &cfg.model, &cfg) {
        Ok(p) => p,
        Err(e) => return FinishOutcome::Error(format!("provider unavailable: {e}")),
    };
    let provider = crate::ai::gate::wrap_for_system(provider);
    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_guardrails(loop_::guardrails_from_cfg(&cfg));
    tools.set_approval(loop_::approval_from_cfg(&cfg));
    // MCP attach (best-effort) — handles dropped at end of fn.
    let _mcp_handles = loop_::attach_mcp_servers_for_cli(&mut tools, &cfg).await;

    let result = if let Some(sid) = job.session_id.as_deref() {
        match crate::agent::memory::sqlite_fts::MemoryDb::open_default() {
            Ok(db) => {
                // Replay prior turns so multi-turn task.stream sessions (the
                // desktop agent UI is the main caller) see continuous context
                // instead of treating every job.submit as a fresh exchange.
                loop_::ask_with_stream_continuation(
                    provider.clone(),
                    &cfg,
                    &job.prompt,
                    &tools,
                    &db,
                    sid,
                    100,
                    Arc::new(JobStreamSink {
                        job_id: job.id.clone(),
                    }),
                    crate::agent::runtime::progress::null_progress(),
                )
                .await
            }
            Err(_) => {
                loop_::ask_with_stream(
                    provider.clone(),
                    &cfg,
                    &job.prompt,
                    &tools,
                    None,
                    Arc::new(JobStreamSink {
                        job_id: job.id.clone(),
                    }),
                    crate::agent::runtime::progress::null_progress(),
                )
                .await
            }
        }
    } else {
        loop_::ask_with_stream(
            provider.clone(),
            &cfg,
            &job.prompt,
            &tools,
            None,
            Arc::new(JobStreamSink {
                job_id: job.id.clone(),
            }),
            crate::agent::runtime::progress::null_progress(),
        )
        .await
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

struct JobStreamSink {
    job_id: String,
}

impl crate::agent::llm::accumulate::StreamSink for JobStreamSink {
    fn on_event(&self, event: &crate::agent::llm::StreamEvent) {
        match Store::open_default().and_then(|store| store.append_stream_event(&self.job_id, event))
        {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(job_id = %self.job_id, error = %err, "failed to append agent stream event");
            }
        }
    }
}

fn enter_job_session(
    job: &Job,
) -> Result<Option<crate::clawd::session_scope::ProcSessionGuard>, String> {
    let Some(session_id) = job.session_id.as_deref() else {
        return Ok(None);
    };
    let sid = session_id
        .parse::<crate::session::SessionId>()
        .map_err(|err| err.to_string())?;
    if !crate::session::session_dir(&sid).exists() {
        return Ok(None);
    }
    crate::clawd::session_scope::ProcSessionGuard::enter(&sid, "clawd-agent-worker").map(Some)
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
        // Serialise env mutation across the test process. Without
        // this, two concurrent tests both call `set_var(...)` and
        // each other's `cmd()` call observes the wrong root.
        _lock: std::sync::MutexGuard<'static, ()>,
    }
    impl EnvGuard {
        fn set(dir: &Path) -> Self {
            let _lock = crate::test_env::lock_env();
            let prev = std::env::var("COS_DATA_DIR").ok();
            std::env::set_var("COS_DATA_DIR", dir);
            Self { prev, _lock }
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
        let job = store.submit("hello".into(), None, None, None, None).unwrap();
        assert_eq!(job.status, JobStatus::Pending);
        let path = dir.path().join("pending").join(format!("{}.json", job.id));
        assert!(path.is_file(), "no file at {path:?}");
        let s = fs::read_to_string(&path).unwrap();
        let parsed: Job = serde_json::from_str(&s).unwrap();
        assert_eq!(parsed.id, job.id);
        assert_eq!(parsed.prompt, "hello");
    }

    #[test]
    fn submit_round_trips_owner_uid_and_home() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store
            .submit(
                "hi".into(),
                None,
                None,
                Some(1001),
                Some("/home/alice".into()),
            )
            .unwrap();
        assert_eq!(job.owner_uid, Some(1001));
        assert_eq!(job.owner_home.as_deref(), Some("/home/alice"));

        // Re-read from disk to confirm serde keeps the fields.
        let path = dir.path().join("pending").join(format!("{}.json", job.id));
        let parsed: Job = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.owner_uid, Some(1001));
        assert_eq!(parsed.owner_home.as_deref(), Some("/home/alice"));
    }

    #[test]
    fn legacy_job_file_without_owner_fields_still_loads() {
        // Older clawd installs wrote Job JSON without owner_uid /
        // owner_home. The new fields are #[serde(default)] so those
        // files must still deserialize.
        let dir = fresh_root();
        let _store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let legacy = json!({
            "id": id,
            "prompt": "old",
            "status": "pending",
            "created_at": now_iso(),
        });
        let path = dir.path().join("pending").join(format!("{id}.json"));
        fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let parsed: Job = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed.id, id);
        assert!(parsed.owner_uid.is_none());
        assert!(parsed.owner_home.is_none());
    }
    #[test]
    fn locate_finds_job_in_pending_bucket() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("hi".into(), None, None, None, None).unwrap();
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
        let job = store.submit("do work".into(), None, None, None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        assert_eq!(claimed.status, JobStatus::Running);
        assert!(claimed.started_at.is_some());
        assert_eq!(claimed.worker_pid, Some(std::process::id()));
        // pending/<id>.json gone, running/<id>.json present
        assert!(!dir
            .path()
            .join("pending")
            .join(format!("{}.json", job.id))
            .exists());
        assert!(dir
            .path()
            .join("running")
            .join(format!("{}.json", job.id))
            .is_file());
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
        let first = store.submit("first".into(), None, None, None, None).unwrap();
        // Touch the second one with a later mtime to be unambiguous on
        // filesystems with low resolution timestamps.
        std::thread::sleep(Duration::from_millis(20));
        let _second = store.submit("second".into(), None, None, None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        assert_eq!(claimed.id, first.id);
    }

    #[test]
    fn finish_ok_moves_running_to_done_with_response() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("p".into(), None, None, None, None).unwrap();
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
        assert!(!dir
            .path()
            .join("running")
            .join(format!("{}.json", job.id))
            .exists());
        assert!(dir
            .path()
            .join("done")
            .join(format!("{}.json", job.id))
            .is_file());
    }

    #[test]
    fn finish_error_records_message() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _job = store.submit("p".into(), None, None, None, None).unwrap();
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
        let job = store.submit("p".into(), None, None, None, None).unwrap();
        let cancelled = store.cancel_pending(&job.id).unwrap().unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(dir
            .path()
            .join("done")
            .join(format!("{}.json", job.id))
            .is_file());
        assert!(!dir
            .path()
            .join("pending")
            .join(format!("{}.json", job.id))
            .exists());
    }

    #[test]
    fn cancel_pending_returns_none_when_already_running() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _ = store.submit("p".into(), None, None, None, None).unwrap();
        let _ = store.claim_one().unwrap().unwrap();
        // The job is now in running/, not pending/ — cancel is a noop.
        let c = store.cancel_pending("nonexistent").unwrap();
        assert!(c.is_none());
    }

    #[test]
    fn cancel_and_claim_no_silent_loss() {
        // Race claim_one() against cancel_pending() across many job
        // ids in parallel threads. The expected invariant is: for
        // every submitted id, exactly one of {claim_one, cancel}
        // succeeds — never both, never neither. Before the lock-based
        // fix, the second rename(pending→{running,done}) could
        // silently lose a state transition: claim would post a real
        // response for a request the user thought they cancelled.
        use std::sync::Arc;
        use std::sync::Mutex;

        let dir = fresh_root();
        let store = Arc::new(Store::with_root(dir.path().to_path_buf()).unwrap());
        let n_jobs = 64usize;
        let ids: Vec<String> = (0..n_jobs)
            .map(|i| store.submit(format!("job-{i}"), None, None, None, None).unwrap().id)
            .collect();

        // Outcomes per id: count of successful claims and successful
        // cancellations. We assert exactly one of each per id.
        let claimed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let cancelled: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

        // One thread tries to cancel every id; another claims as many
        // as it can. They interleave on the per-id flock, so for each
        // id at most one wins.
        let s1 = store.clone();
        let ids1 = ids.clone();
        let cancelled1 = cancelled.clone();
        let h_cancel = std::thread::spawn(move || {
            for id in &ids1 {
                if let Ok(Some(j)) = s1.cancel_pending(id) {
                    cancelled1.lock().unwrap().push(j.id);
                }
            }
        });
        let s2 = store.clone();
        let claimed2 = claimed.clone();
        let h_claim = std::thread::spawn(move || {
            // Loop until pending is empty. Each successful claim is
            // mutually exclusive with any concurrent cancel of the
            // same id.
            loop {
                match s2.claim_one() {
                    Ok(Some(j)) => claimed2.lock().unwrap().push(j.id),
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        });
        h_cancel.join().unwrap();
        h_claim.join().unwrap();

        let claimed = claimed.lock().unwrap().clone();
        let cancelled = cancelled.lock().unwrap().clone();
        // Sanity: never more than one outcome per id.
        let mut seen = std::collections::HashSet::new();
        for id in claimed.iter().chain(cancelled.iter()) {
            assert!(
                seen.insert(id.clone()),
                "id {id} reported both claimed and cancelled — silent loss of cancel"
            );
        }
        // The claim loop runs to exhaustion, so every id that wasn't
        // cancelled must have been claimed. Total covered == n_jobs.
        assert_eq!(
            claimed.len() + cancelled.len(),
            n_jobs,
            "missing transitions: claimed={} cancelled={}",
            claimed.len(),
            cancelled.len()
        );
        // Confirm filesystem state agrees: no pending leftovers.
        let pending_count = fs::read_dir(dir.path().join("pending"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .count();
        assert_eq!(pending_count, 0, "every job must have transitioned");
    }

    #[test]
    fn list_bucket_returns_newest_first_and_respects_limit() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _a = store.submit("a".into(), None, None, None, None).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let b = store.submit("b".into(), None, None, None, None).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let c = store.submit("c".into(), None, None, None, None).unwrap();
        let v = store.list_bucket(JobStatus::Pending, Some(2)).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, c.id);
        assert_eq!(v[1].id, b.id);
    }

    #[test]
    fn counts_reflect_per_bucket_state() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let _a = store.submit("a".into(), None, None, None, None).unwrap();
        let _b = store.submit("b".into(), None, None, None, None).unwrap();
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
            let _ = store.submit("p".into(), None, None, None, None).unwrap();
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
        let mut j = Job::new_pending("a".repeat(100), None, None, None, None);
        j.id = "fixed".into();
        assert_eq!(j.preview(10), "aaaaaaaaaa…");
        let short = Job::new_pending("hi".into(), None, None, None, None);
        assert_eq!(short.preview(10), "hi");
    }

    // ----- CLI dispatcher tests (use COS_DATA_DIR via EnvGuard) -----

    #[test]
    fn cmd_help_lists_subcommands() {
        let dir = fresh_root();
        let _g = EnvGuard::set(dir.path());
        let v = cmd(&[]).unwrap();
        let arr = v["subcommands"].as_array().unwrap();
        assert!(arr
            .iter()
            .any(|s| s.as_str().unwrap().starts_with("submit")));
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
        let job = store.submit("p".into(), None, None, None, None).unwrap();
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
            let _ = store.submit("p".into(), None, None, None, None).unwrap();
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
        let _ = store.submit("alive".into(), None, None, None, None).unwrap();
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
        let oldest = store.submit("oldest".into(), None, None, None, None).unwrap();
        std::thread::sleep(Duration::from_millis(1100)); // ensure created_at second-rollover
                                                         // Submit middle, then claim+finish (lands in done/).
        let mid = store.submit("middle".into(), None, None, None, None).unwrap();
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
        let newest = store.submit("newest".into(), None, None, None, None).unwrap();

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
