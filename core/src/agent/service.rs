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
//!   - `cancel <job_id>` moves a pending job directly into `done/`, or
//!     marks a running job for interruption and signals its live agent loop.
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

/// Maximum number of times a job may be recovered from `running/` after
/// its worker died before we give up and fail it (see
/// [`Store::recover_orphaned_jobs`]). Stops a job that crashes every
/// worker from looping forever and starving the queue.
const MAX_RECOVERIES: u32 = 3;

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
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_context: Option<String>,
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
    pub worker_start_time_ticks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_at: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<crate::agent::runtime::evidence::EvidenceReport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<crate::agent::llm::ProviderFallbackState>,
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
    /// How many times this job has been recovered from `running/` after
    /// the worker executing it died (see [`Store::recover_orphaned_jobs`]).
    /// Bounds crash-loop blast radius: a job that repeatedly kills its
    /// worker is failed instead of requeued once this exceeds
    /// [`MAX_RECOVERIES`]. Defaults to 0; absent in old on-disk files.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub recovery_count: u32,
}

/// serde skip predicate for the common `recovery_count == 0` case so
/// existing job files stay byte-compatible.
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

impl Job {
    fn new_pending(
        prompt: String,
        context: Option<String>,
        branch_context: Option<String>,
        session_id: Option<String>,
        max_turns: Option<u32>,
        owner_uid: Option<u32>,
        owner_home: Option<String>,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            prompt,
            context,
            branch_context,
            session_id,
            max_turns,
            status: JobStatus::Pending,
            created_at: now_iso(),
            started_at: None,
            finished_at: None,
            worker_pid: None,
            worker_start_time_ticks: None,
            cancel_requested_at: None,
            response: None,
            error: None,
            turns_used: None,
            provider: None,
            model: None,
            evidence: None,
            fallback: None,
            owner_uid,
            owner_home,
            recovery_count: 0,
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
            crate::storage::ensure_private_dir(&root.join(sub))?;
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

    fn stream_path(&self, id: &str) -> PathBuf {
        self.root.join("streams").join(format!("{id}.jsonl"))
    }

    pub fn append_stream_event(
        &self,
        id: &str,
        event: &crate::agent::llm::StreamEvent,
    ) -> io::Result<()> {
        self.append_stream_record(
            id,
            json!({
                "ts": chrono::Utc::now(),
                "event": event,
            }),
        )
    }

    pub fn append_stream_progress(&self, id: &str, progress: Value) -> io::Result<()> {
        self.append_stream_record(
            id,
            json!({
                "ts": chrono::Utc::now(),
                "progress": progress,
            }),
        )
    }

    fn append_stream_record(&self, id: &str, value: Value) -> io::Result<()> {
        validate_job_id(id)?;
        let line = serde_json::to_string(&value).map_err(io_other)?;
        crate::filelock::append_locked(&self.stream_path(id), &line).map_err(io_other)
    }

    pub fn read_stream_events(&self, id: &str, cursor: usize) -> io::Result<(usize, Vec<Value>)> {
        validate_job_id(id)?;
        let path = self.stream_path(id);
        let raw = match crate::filelock::read_locked(&path).map_err(io_other)? {
            Some(raw) => raw,
            None => return Ok((0, Vec::new())),
        };
        let mut events = Vec::new();
        let mut next_cursor = cursor;
        let records = raw.split_inclusive('\n').collect::<Vec<_>>();
        for (idx, record) in records.iter().enumerate() {
            if idx < cursor {
                continue;
            }
            let line = record.trim_end_matches(['\r', '\n']);
            match serde_json::from_str::<Value>(line) {
                Ok(value) => {
                    events.push(value);
                    next_cursor = idx + 1;
                }
                Err(err) => {
                    let incomplete_tail = idx + 1 == records.len() && !record.ends_with('\n');
                    if incomplete_tail {
                        break;
                    }
                    next_cursor = idx + 1;
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
        validate_job_id(id)?;
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

    /// Locate a job visible to `owner_uid`. `None` is the privileged
    /// all-owners view; `Some(uid)` excludes legacy ownerless jobs.
    pub fn locate_for_owner(
        &self,
        id: &str,
        owner_uid: Option<u32>,
    ) -> io::Result<Option<(JobStatus, Job)>> {
        Ok(self
            .locate(id)?
            .filter(|(_, job)| job_visible_to(job, owner_uid)))
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
        self.submit_with_context(
            prompt, None, None, session_id, max_turns, owner_uid, owner_home,
        )
    }

    pub fn submit_with_context(
        &self,
        prompt: String,
        context: Option<String>,
        branch_context: Option<String>,
        session_id: Option<String>,
        max_turns: Option<u32>,
        owner_uid: Option<u32>,
        owner_home: Option<String>,
    ) -> io::Result<Job> {
        let job = Job::new_pending(
            prompt,
            context,
            branch_context,
            session_id,
            max_turns,
            owner_uid,
            owner_home,
        );
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
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
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

    /// List jobs visible to `owner_uid`, applying the limit after
    /// ownership filtering so other users' newer jobs cannot hide results.
    pub fn list_bucket_for_owner(
        &self,
        bucket: JobStatus,
        limit: Option<usize>,
        owner_uid: Option<u32>,
    ) -> io::Result<Vec<Job>> {
        let mut jobs = self.list_bucket(bucket, None)?;
        jobs.retain(|job| job_visible_to(job, owner_uid));
        if let Some(limit) = limit {
            jobs.truncate(limit);
        }
        Ok(jobs)
    }

    /// Atomically claim one pending job: rename pending/<id>.json →
    /// running/<id>.json, then rewrite the file with `status =
    /// Running` + `started_at` + worker PID/start-time identity. Returns Ok(None) when
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
        candidates.sort_by_key(|a| a.0);

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
                    validate_job_id(&job.id)?;
                    if job.id != id {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "job id `{}` does not match queue filename `{id}`",
                                job.id
                            ),
                        ));
                    }
                    job.status = JobStatus::Running;
                    job.started_at = Some(now_iso());
                    let worker_pid = std::process::id();
                    job.worker_pid = Some(worker_pid);
                    job.worker_start_time_ticks =
                        crate::proc::read_start_time_ticks_pub(worker_pid);
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

    /// Recover jobs stranded in `running/` by a worker that died before
    /// finishing them (crash, `kill -9`, OOM, power loss, container
    /// restart). Without this, such a job sits in `running/` forever:
    /// `claim_one` only ever looks at `pending/`, so the work is never
    /// retried and `cos agent task <id>` blocks until its deadline.
    ///
    /// For each `running/` job we verify both `worker_pid` and the kernel
    /// process start-time captured at claim:
    ///   * **Exact match** — another worker still owns it; leave untouched.
    ///   * **PID alive but identity missing/unreadable** — fail closed rather
    ///     than requeue and risk executing the job twice.
    ///   * **Dead / no pid recorded** — the owning worker is gone. Move
    ///     the job back to `pending/` (status reset to `Pending`, the
    ///     stale `worker_pid` / `started_at` cleared) so a worker can
    ///     re-claim it. A `recovery_count` guards against a poison job
    ///     that crashes every worker: after [`MAX_RECOVERIES`] attempts
    ///     it is failed into `done/` with an explanatory error instead
    ///     of being requeued forever.
    ///
    /// Intended to run once at worker start-up, before the claim loop.
    /// Returns `(requeued, failed)` counts for logging. Best-effort: a
    /// single malformed job file is skipped, not fatal.
    pub fn recover_orphaned_jobs(&self) -> io::Result<(usize, usize)> {
        let running = self.bucket_dir(JobStatus::Running);
        let mut requeued = 0usize;
        let mut failed = 0usize;

        let entries: Vec<PathBuf> = match fs::read_dir(&running) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                .collect(),
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok((0, 0)),
            Err(e) => return Err(e),
        };

        for path in entries {
            let id = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            // Serialise against claim_one/cancel for this id so we can't
            // requeue a job another worker is mid-claim on.
            let _lock = match self.lock_for_id(&id) {
                Ok(l) => l,
                Err(_) => continue,
            };
            // Re-read under the lock; it may have moved since the listing.
            let raw = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let mut job: Job = match serde_json::from_str(&raw) {
                Ok(j) => j,
                Err(e) => {
                    tracing::warn!("agent recovery: skipping malformed running job {path:?}: {e}");
                    continue;
                }
            };

            let mut unverifiable_identity = false;
            // Owner still alive with the exact same process identity ⇒ not
            // an orphan; leave it be.
            if let Some(pid) = job.worker_pid {
                match job.worker_start_time_ticks {
                    Some(expected) => match crate::proc::read_start_time_ticks_pub(pid) {
                        Some(current) if current == expected => continue,
                        Some(_) => {}
                        None if crate::proc::is_pid_alive(pid) => {
                            unverifiable_identity = true;
                        }
                        None => {}
                    },
                    None if crate::proc::is_pid_alive(pid) => {
                        unverifiable_identity = true;
                    }
                    None => {}
                }
            }
            if unverifiable_identity {
                job.status = JobStatus::Error;
                job.error = Some(
                    "worker PID is alive but its start-time identity is unavailable; \
                     refusing to retry a potentially active job"
                        .to_string(),
                );
                job.finished_at = Some(now_iso());
                write_json_atomic(&path, &job)?;
                let done = self.path_for(JobStatus::Ok, &id);
                fs::rename(&path, &done)?;
                let _ = finish_durable_session(&job);
                crate::clawd::audit::record_task_event(
                    "clawd.task.worker_identity_unverifiable",
                    &job,
                );
                failed += 1;
                continue;
            }

            if job.cancel_requested_at.is_some() {
                job.status = JobStatus::Cancelled;
                job.finished_at = Some(now_iso());
                write_json_atomic(&path, &job)?;
                let done = self.path_for(JobStatus::Ok, &id);
                fs::rename(&path, &done)?;
                let _ = finish_durable_session(&job);
                crate::clawd::audit::record_task_event(
                    "clawd.task.cancelled",
                    &job,
                );
                continue;
            }

            job.recovery_count = job.recovery_count.saturating_add(1);

            if job.recovery_count > MAX_RECOVERIES {
                // Poison job: it has already taken down a worker
                // MAX_RECOVERIES times. Fail it rather than risk an
                // endless crash loop that starves every other job.
                job.status = JobStatus::Error;
                job.error = Some(format!(
                    "job abandoned after {} interrupted run(s) (worker died mid-execution each time)",
                    job.recovery_count - 1
                ));
                job.finished_at = Some(now_iso());
                write_json_atomic(&path, &job)?;
                let done = self.path_for(JobStatus::Ok, &id);
                fs::rename(&path, &done)?;
                let _ = finish_durable_session(&job);
                crate::clawd::audit::record_task_event("clawd.task.abandoned", &job);
                failed += 1;
                continue;
            }

            // Requeue: reset to pending and move back to pending/.
            let mut audit_job = job.clone();
            audit_job.status = JobStatus::Pending;
            job.status = JobStatus::Pending;
            job.worker_pid = None;
            job.worker_start_time_ticks = None;
            job.started_at = None;
            job.cancel_requested_at = None;
            write_json_atomic(&path, &job)?;
            let pending = self.path_for(JobStatus::Pending, &id);
            fs::rename(&path, &pending)?;
            crate::clawd::audit::record_task_event("clawd.task.recovered", &audit_job);
            requeued += 1;
        }

        Ok((requeued, failed))
    }
    pub fn finish(&self, job: Job, outcome: FinishOutcome) -> io::Result<Job> {
        validate_job_id(&job.id)?;
        let _lock = self.lock_for_id(&job.id)?;
        let running_path = self.path_for(JobStatus::Running, &job.id);
        let raw = fs::read_to_string(&running_path)?;
        let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
        validate_job_id(&job.id)?;
        let outcome = if job.cancel_requested_at.is_some() {
            FinishOutcome::Cancelled
        } else {
            outcome
        };
        match outcome {
            FinishOutcome::Ok {
                response,
                turns_used,
                provider,
                model,
                evidence,
                fallback,
            } => {
                job.status = JobStatus::Ok;
                job.response = Some(response);
                job.turns_used = Some(turns_used);
                job.provider = Some(provider);
                job.model = Some(model);
                job.evidence = *evidence;
                job.fallback = *fallback;
            }
            FinishOutcome::Error(msg) => {
                job.status = JobStatus::Error;
                job.error = Some(msg);
            }
            FinishOutcome::Cancelled => {
                job.status = JobStatus::Cancelled;
                job.error = Some("cancelled by user".to_string());
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
        self.cancel_pending_for_owner(id, None)
    }

    /// Cancel a pending job only when it is visible to `owner_uid`.
    /// The ownership check happens under the same per-id lock as the
    /// state transition, so an unauthorized caller cannot mutate the job.
    pub fn cancel_pending_for_owner(
        &self,
        id: &str,
        owner_uid: Option<u32>,
    ) -> io::Result<Option<Job>> {
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
                if !job_visible_to(&job, owner_uid) {
                    return Ok(None);
                }
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

    pub fn request_cancel_for_owner(
        &self,
        id: &str,
        owner_uid: Option<u32>,
    ) -> io::Result<Option<(Job, bool)>> {
        let _lock = self.lock_for_id(id)?;
        let pending = self.path_for(JobStatus::Pending, id);
        let done = self.path_for(JobStatus::Ok, id);
        match fs::read_to_string(&pending) {
            Ok(raw) => {
                let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
                if !job_visible_to(&job, owner_uid) {
                    return Ok(None);
                }
                job.status = JobStatus::Cancelled;
                job.cancel_requested_at = Some(now_iso());
                job.finished_at = Some(now_iso());
                write_json_atomic(&pending, &job)?;
                fs::rename(&pending, &done)?;
                finish_durable_session(&job)?;
                crate::clawd::audit::record_task_event("clawd.task.cancelled", &job);
                return Ok(Some((job, true)));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let running = self.path_for(JobStatus::Running, id);
        match fs::read_to_string(&running) {
            Ok(raw) => {
                let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
                if !job_visible_to(&job, owner_uid) {
                    return Ok(None);
                }
                if job.cancel_requested_at.is_none() {
                    job.cancel_requested_at = Some(now_iso());
                    write_json_atomic(&running, &job)?;
                    crate::clawd::audit::record_task_event(
                        "clawd.task.cancel-requested",
                        &job,
                    );
                }
                Ok(Some((job, false)))
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn cancellation_requested(&self, id: &str) -> io::Result<bool> {
        validate_job_id(id)?;
        let path = self.path_for(JobStatus::Running, id);
        match fs::read_to_string(path) {
            Ok(raw) => {
                let job: Job = serde_json::from_str(&raw).map_err(io_other)?;
                Ok(job.cancel_requested_at.is_some())
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
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
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        let now = std::time::SystemTime::now();
        let mut removed = 0usize;
        for (i, (mtime, p)) in entries.into_iter().enumerate() {
            if i < keep_last {
                continue;
            }
            let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
            if age >= older_than
                && fs::remove_file(&p).is_ok() {
                    removed += 1;
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
        validate_job_id(id)?;
        let lock_dir = self.root.join("locks");
        crate::storage::ensure_private_dir(&lock_dir)?;
        let lock_path = lock_dir.join(format!("{id}.lock"));
        let mut options = fs::OpenOptions::new();
        options.create(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let f = options.open(&lock_path)?;
        crate::storage::set_private_file(&lock_path)?;
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

fn job_visible_to(job: &Job, owner_uid: Option<u32>) -> bool {
    match owner_uid {
        None => true,
        Some(uid) => job.owner_uid == Some(uid),
    }
}

fn validate_job_id(id: &str) -> io::Result<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid job id: {id}"),
        ));
    }
    Ok(())
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
        evidence: Box<Option<crate::agent::runtime::evidence::EvidenceReport>>,
        fallback: Box<Option<crate::agent::llm::ProviderFallbackState>>,
    },
    Error(String),
    Cancelled,
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
        "cancel_requested": job.cancel_requested_at.is_some(),
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
                    return serde_json::to_value(&job).map_err(|e| e.to_string());
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
    match store.request_cancel_for_owner(id, None).map_err(|e| e.to_string())? {
        Some((job, immediate)) => {
            if !immediate {
                crate::agent::runtime::interrupt::signal(id);
            }
            Ok(json!({
                "status": if immediate { "cancelled" } else { "cancel_requested" },
                "job_id": job.id,
            }))
        }
        None => {
            // Either it never existed or it was already terminal.
            match store.locate(id).map_err(|e| e.to_string())? {
                Some((_, job)) => Ok(json!({
                    "status": "not_cancelled",
                    "reason": "already_terminal",
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
    // Reclaim jobs stranded in running/ by a previously-crashed worker
    // before we start claiming new ones. This is what makes a daemon
    // restart self-healing: interrupted jobs get retried (or failed if
    // they keep killing the worker) instead of hanging forever.
    match store.recover_orphaned_jobs() {
        Ok((requeued, failed)) if requeued > 0 || failed > 0 => {
            tracing::info!(
                requeued,
                failed,
                "agent service worker: recovered orphaned jobs from a prior crash"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("agent service worker: orphan recovery failed: {e}"),
    }
    // Routed tool calls use `block_in_place` so synchronous primitives keep
    // the current task-local user/config context without starving unrelated
    // runtime work. That API requires Tokio's multi-thread scheduler.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
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
                let mut outcome = runtime.block_on(run_one_job(&job));
                if store
                    .cancellation_requested(&job.id)
                    .unwrap_or(false)
                {
                    outcome = FinishOutcome::Cancelled;
                }
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
    let run = crate::paths::with_routed_job(run_one_routed_job(job));
    tokio::pin!(run);
    tokio::select! {
        outcome = &mut run => outcome,
        _ = wait_for_cancellation(&job.id) => {
            loop {
                crate::agent::runtime::interrupt::signal(&job.id);
                tokio::select! {
                    _ = &mut run => break,
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {}
                }
            }
            FinishOutcome::Cancelled
        }
    }
}

async fn wait_for_cancellation(job_id: &str) {
    loop {
        let cancelled = Store::open_default()
            .and_then(|store| store.cancellation_requested(job_id))
            .unwrap_or(false);
        if cancelled {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_one_routed_job(job: &Job) -> FinishOutcome {
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
        let run = crate::config::with_override(cfg, run_one_job_inner(job));
        match job.owner_uid {
            Some(0) => run.await,
            Some(uid) => crate::paths::with_user_override(uid, home_path, run).await,
            None => crate::paths::with_home_override(home_path, run).await,
        }
    } else {
        run_one_job_inner(job).await
    }
}

async fn run_one_job_inner(job: &Job) -> FinishOutcome {
    let session = match job_session_info(job) {
        Ok(session) => session,
        Err(err) => return FinishOutcome::Error(format!("session unavailable: {err}")),
    };
    match session {
        Some(session) => {
            crate::proc::with_trusted_session_override(session, run_one_job_scoped(job)).await
        }
        None => run_one_job_scoped(job).await,
    }
}

async fn run_one_job_scoped(job: &Job) -> FinishOutcome {
    use crate::agent::runtime::loop_;

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
    let provider = match crate::ai::gate::build_system_provider(&cfg) {
        Ok(p) => p,
        Err(e) => return FinishOutcome::Error(format!("provider unavailable: {e}")),
    };
    let mut tools = crate::agent::tools::registry::default_registry();
    tools.set_guardrails(loop_::guardrails_from_cfg(&cfg));
    tools.set_approval(loop_::approval_from_cfg(&cfg));
    // MCP attach (best-effort) — handles dropped at end of fn.
    let _mcp_handles = loop_::attach_mcp_servers_for_cli(&mut tools, &cfg).await;
    let stream_sink: Arc<dyn crate::agent::llm::accumulate::StreamSink> = Arc::new(JobStreamSink {
        job_id: job.id.clone(),
    });
    let progress_sink: Arc<dyn crate::agent::runtime::progress::ProgressSink> =
        Arc::new(JobProgressSink {
            job_id: job.id.clone(),
        });

    let result = if let Some(sid) = job.session_id.as_deref() {
        match crate::agent::memory::sqlite_fts::MemoryDb::open_default() {
            Ok(db) => {
                if let Some(context) = job
                    .branch_context
                    .as_deref()
                    .filter(|context| !context.trim().is_empty())
                {
                    if let Err(error) = seed_branch_context(&db, sid, context) {
                        tracing::warn!(
                            session_id = sid,
                            %error,
                            "failed to seed retry branch context"
                        );
                    }
                }
                // Replay prior turns so multi-turn task.stream sessions (the
                // desktop agent UI is the main caller) see continuous context
                // instead of treating every job.submit as a fresh exchange.
                loop_::ask_with_stream_continuation_scoped(
                    provider.clone(),
                    &cfg,
                    &job.prompt,
                    job.context.as_deref(),
                    &tools,
                    &db,
                    sid,
                    100,
                    stream_sink.clone(),
                    progress_sink.clone(),
                    &job.id,
                )
                .await
            }
            Err(_) => {
                loop_::ask_with_stream_scoped(
                    provider.clone(),
                    &cfg,
                    &job.prompt,
                    job.context.as_deref(),
                    &tools,
                    None,
                    stream_sink.clone(),
                    progress_sink.clone(),
                    &job.id,
                )
                .await
            }
        }
    } else {
        loop_::ask_with_stream_scoped(
            provider.clone(),
            &cfg,
            &job.prompt,
            job.context.as_deref(),
            &tools,
            None,
            stream_sink,
            progress_sink,
            &job.id,
        )
        .await
    };

    match result {
        Ok(r) => FinishOutcome::Ok {
            response: r.answer,
            turns_used: r.turns,
            provider: r.provider,
            model: r.model,
            evidence: Box::new(Some(r.evidence)),
            fallback: Box::new(r.fallback),
        },
        Err(e) => FinishOutcome::Error(e.to_string()),
    }
}

fn seed_branch_context(
    db: &crate::agent::memory::sqlite_fts::MemoryDb,
    session_id: &str,
    context: &str,
) -> Result<(), String> {
    let bounded = clip_progress_text(context.trim(), 32 * 1024);
    let wrapped = crate::agent::safety::untrusted::wrap_untrusted(
        crate::agent::safety::untrusted::APP_CONTEXT_TAG,
        &bounded,
    );
    let already_seeded = db
        .recent(session_id, 20)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|row| row.role == "system" && row.content == wrapped);
    if !already_seeded {
        db.record_message(session_id, "system", &wrapped)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

struct JobStreamSink {
    job_id: String,
}

struct JobProgressSink {
    job_id: String,
}

fn clip_progress_text(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut clipped = value.chars().take(max_chars).collect::<String>();
    clipped.push_str(" …");
    clipped
}

impl crate::agent::runtime::progress::ProgressSink for JobProgressSink {
    fn on_tool_start(&self, id: &str, name: &str, _input: &Value) {
        let progress = json!({
            "kind": "tool_start",
            "id": id,
            "name": name,
        });
        if let Err(err) = Store::open_default()
            .and_then(|store| store.append_stream_progress(&self.job_id, progress))
        {
            tracing::warn!(
                job_id = %self.job_id,
                error = %err,
                "failed to append agent tool-start progress"
            );
        }
    }

    fn on_tool_result(
        &self,
        id: &str,
        name: &str,
        ok: bool,
        latency_ms: u64,
        _bytes_returned: usize,
        _content_preview: &str,
    ) {
        let progress = json!({
            "kind": "tool_result",
            "id": id,
            "name": name,
            "ok": ok,
            "latency_ms": latency_ms,
        });
        if let Err(err) = Store::open_default()
            .and_then(|store| store.append_stream_progress(&self.job_id, progress))
        {
            tracing::warn!(
                job_id = %self.job_id,
                error = %err,
                "failed to append agent tool-result progress"
            );
        }
    }
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

fn job_session_info(job: &Job) -> Result<Option<crate::proc::SessionInfo>, String> {
    let Some(session_id) = job.session_id.as_deref() else {
        return Ok(None);
    };
    let sid = session_id
        .parse::<crate::session::SessionId>()
        .map_err(|err| err.to_string())?;
    if !crate::session::session_dir(&sid).exists() {
        return Ok(None);
    }
    crate::clawd::session_scope::trusted_session_info(&sid, "clawd-agent-worker").map(Some)
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

    #[test]
    fn tool_progress_round_trips_through_task_stream() {
        let root = fresh_root();
        let store = Store::with_root(root.path().to_path_buf()).unwrap();
        let job = store.submit("test".into(), None, None, None, None).unwrap();
        store
            .append_stream_progress(
                &job.id,
                json!({
                    "kind": "tool_result",
                    "id": "tool-1",
                    "name": "fs.read",
                    "ok": true,
                    "preview": "done",
                }),
            )
            .unwrap();

        let (cursor, events) = store.read_stream_events(&job.id, 0).unwrap();
        assert_eq!(cursor, 1);
        assert_eq!(events[0]["progress"]["kind"], "tool_result");
        assert_eq!(events[0]["progress"]["id"], "tool-1");
    }

    #[test]
    fn incomplete_stream_tail_is_not_consumed() {
        use std::io::Write as _;

        let root = fresh_root();
        let store = Store::with_root(root.path().to_path_buf()).unwrap();
        let job = store.submit("test".into(), None, None, None, None).unwrap();
        store
            .append_stream_progress(&job.id, json!({ "kind": "tool_start", "id": "one" }))
            .unwrap();
        let path = store.stream_path(&job.id);
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"progress":{"kind":"tool_result""#)
            .unwrap();
        drop(file);

        let (cursor, events) = store.read_stream_events(&job.id, 0).unwrap();
        assert_eq!(cursor, 1);
        assert_eq!(events.len(), 1);

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#","id":"two"}}"#).unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);
        let (cursor, events) = store.read_stream_events(&job.id, cursor).unwrap();
        assert_eq!(cursor, 2);
        assert_eq!(events[0]["progress"]["id"], "two");
    }

    #[test]
    fn progress_text_clipping_is_bounded() {
        const MAX_CHARS: usize = 4096;
        let oversized = "x".repeat(MAX_CHARS + 100);
        assert!(
            clip_progress_text(&oversized, MAX_CHARS).chars().count() <= MAX_CHARS + 2
        );
    }

    #[test]
    fn retry_branch_context_is_seeded_once_as_hidden_system_memory() {
        let db = crate::agent::memory::sqlite_fts::MemoryDb::open_in_memory().unwrap();
        let context = format!("User: earlier question {}", "x".repeat(40 * 1024));
        seed_branch_context(&db, "branch-session", &context).unwrap();
        seed_branch_context(&db, "branch-session", &context).unwrap();
        let rows = db.recent("branch-session", 20).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].role, "system");
        assert!(rows[0].content.contains("<untrusted_app_context>"));
        assert!(rows[0].content.chars().count() < 34 * 1024);
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

    /// A job stranded in running/ by a dead worker is requeued to
    /// pending/ with worker_pid/started_at cleared and recovery_count
    /// incremented, so a fresh worker can re-claim it.
    #[test]
    fn recover_requeues_job_whose_worker_is_dead() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("interrupted work".into(), None, None, None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        // Simulate the worker dying: overwrite the running file with a
        // pid that is certainly not alive (0 is treated as dead).
        let running = dir.path().join("running").join(format!("{}.json", claimed.id));
        let mut stranded: Job =
            serde_json::from_str(&std::fs::read_to_string(&running).unwrap()).unwrap();
        stranded.worker_pid = Some(0);
        std::fs::write(&running, serde_json::to_string_pretty(&stranded).unwrap()).unwrap();

        let (requeued, failed) = store.recover_orphaned_jobs().unwrap();
        assert_eq!((requeued, failed), (1, 0));

        // Back in pending/, reset for re-claiming.
        let (bucket, recovered) = store.locate(&job.id).unwrap().unwrap();
        assert_eq!(bucket, JobStatus::Pending);
        assert_eq!(recovered.status, JobStatus::Pending);
        assert!(recovered.worker_pid.is_none());
        assert!(recovered.started_at.is_none());
        assert_eq!(recovered.recovery_count, 1);
        assert!(store.claim_one().unwrap().is_some(), "recovered job must be re-claimable");
    }

    /// A job whose worker is still alive must NOT be touched by recovery.
    #[test]
    fn recover_leaves_job_with_live_worker_alone() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("in flight".into(), None, None, None, None).unwrap();
        // claim_one stamps the current (live) process pid.
        let _claimed = store.claim_one().unwrap().unwrap();
        let (requeued, failed) = store.recover_orphaned_jobs().unwrap();
        assert_eq!((requeued, failed), (0, 0));
        let (bucket, _) = store.locate(&job.id).unwrap().unwrap();
        assert_eq!(bucket, JobStatus::Running, "live job must stay running");
    }

    /// A poison job that keeps killing its worker is failed (not requeued)
    /// once recovery_count exceeds MAX_RECOVERIES, so it can't starve the
    /// queue in an endless crash loop.
    #[test]
    fn recover_fails_poison_job_after_max_recoveries() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        let job = store.submit("poison".into(), None, None, None, None).unwrap();
        let claimed = store.claim_one().unwrap().unwrap();
        // Pre-set recovery_count at the cap with a dead worker, so the
        // next recovery pass tips it over MAX_RECOVERIES → fail.
        let running = dir.path().join("running").join(format!("{}.json", claimed.id));
        let mut stranded: Job =
            serde_json::from_str(&std::fs::read_to_string(&running).unwrap()).unwrap();
        stranded.worker_pid = Some(0);
        stranded.recovery_count = MAX_RECOVERIES;
        std::fs::write(&running, serde_json::to_string_pretty(&stranded).unwrap()).unwrap();

        let (requeued, failed) = store.recover_orphaned_jobs().unwrap();
        assert_eq!((requeued, failed), (0, 1));
        let (bucket, done) = store.locate(&job.id).unwrap().unwrap();
        assert_eq!(bucket, JobStatus::Ok); // done bucket
        assert_eq!(done.status, JobStatus::Error);
        assert!(done.error.as_deref().unwrap().contains("abandoned"));
    }

    #[test]
    fn recover_is_noop_with_empty_running_bucket() {
        let dir = fresh_root();
        let store = Store::with_root(dir.path().to_path_buf()).unwrap();
        assert_eq!(store.recover_orphaned_jobs().unwrap(), (0, 0));
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
                    evidence: Box::new(None),
                    fallback: Box::new(None),
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
                    evidence: Box::new(None),
                    fallback: Box::new(None),
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
                        evidence: Box::new(None),
                        fallback: Box::new(None),
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
        let mut j = Job::new_pending("a".repeat(100), None, None, None, None, None, None);
        j.id = "fixed".into();
        assert_eq!(j.preview(10), "aaaaaaaaaa…");
        let short = Job::new_pending("hi".into(), None, None, None, None, None, None);
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
                    evidence: Box::new(None),
                    fallback: Box::new(None),
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
                        evidence: Box::new(None),
                        fallback: Box::new(None),
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
                    evidence: Box::new(None),
                    fallback: Box::new(None),
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
