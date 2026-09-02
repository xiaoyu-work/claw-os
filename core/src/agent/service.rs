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

use std::collections::BTreeMap;
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
const JOB_SCHEMA_VERSION: u32 = 2;
const APPROVAL_WAIT_TIMEOUT_SECS: i64 = 8 * 60 * 60;
const STREAM_PRUNE_TOMBSTONE_SUFFIX: &str = ".jsonl.prune";

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
    #[serde(rename = "waiting_approval")]
    WaitingApproval,
    Ok,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionPhase {
    Unprepared,
    Preparing,
    Prepared,
    Committed,
    Indeterminate,
    LegacyUnknown,
}

impl ExecutionPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unprepared => "unprepared",
            Self::Preparing => "preparing",
            Self::Prepared => "prepared",
            Self::Committed => "committed",
            Self::Indeterminate => "indeterminate",
            Self::LegacyUnknown => "legacy_unknown",
        }
    }

    fn replay_is_proven_safe(self) -> bool {
        matches!(self, Self::Unprepared | Self::Preparing | Self::Prepared)
    }
}

fn legacy_execution_phase() -> ExecutionPhase {
    ExecutionPhase::LegacyUnknown
}

fn legacy_job_schema_version() -> u32 {
    1
}

impl JobStatus {
    fn bucket(self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::WaitingApproval => "waiting",
            JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled => "done",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::WaitingApproval => "waiting_approval",
            JobStatus::Ok => "ok",
            JobStatus::Error => "error",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

/// Persistent representation of a single agent job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    #[serde(default = "legacy_job_schema_version")]
    pub schema_version: u32,
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
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub use_memory: bool,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub waiting_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resumed_after_approval: Vec<String>,
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
    /// Broker-derived client/source metadata snapshotted at submission.
    /// Request JSON cannot set this field.
    #[serde(default)]
    pub client: crate::session::SessionClient,
    /// How many times this job has been recovered from `running/` after
    /// the worker executing it died (see [`Store::recover_orphaned_jobs`]).
    /// Bounds crash-loop blast radius: a job that repeatedly kills its
    /// worker is failed instead of requeued once this exceeds
    /// [`MAX_RECOVERIES`]. Defaults to 0; absent in old on-disk files.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub recovery_count: u32,
    #[serde(default = "legacy_execution_phase")]
    pub execution_phase: ExecutionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_prepare_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_commit_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_generation: Option<String>,
}

/// serde skip predicate for the common `recovery_count == 0` case so
/// existing job files stay byte-compatible.
fn is_zero_u32(n: &u32) -> bool {
    *n == 0
}

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
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
        Self::new_pending_with_client(
            prompt,
            context,
            branch_context,
            session_id,
            max_turns,
            owner_uid,
            owner_home,
            crate::session::SessionClient::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_pending_with_client(
        prompt: String,
        context: Option<String>,
        branch_context: Option<String>,
        session_id: Option<String>,
        max_turns: Option<u32>,
        owner_uid: Option<u32>,
        owner_home: Option<String>,
        mut client: crate::session::SessionClient,
    ) -> Self {
        client.attended = false;
        Self {
            schema_version: JOB_SCHEMA_VERSION,
            id: uuid::Uuid::new_v4().to_string(),
            prompt,
            context,
            branch_context,
            session_id,
            max_turns,
            use_memory: true,
            status: JobStatus::Pending,
            created_at: now_iso(),
            started_at: None,
            finished_at: None,
            worker_pid: None,
            worker_start_time_ticks: None,
            cancel_requested_at: None,
            waiting_on: Vec::new(),
            waiting_since: None,
            resumed_after_approval: Vec::new(),
            response: None,
            error: None,
            turns_used: None,
            provider: None,
            model: None,
            evidence: None,
            fallback: None,
            owner_uid,
            owner_home,
            client,
            recovery_count: 0,
            execution_phase: ExecutionPhase::Unprepared,
            execution_prepare_nonce: None,
            execution_commit_nonce: None,
            execution_generation: None,
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
    publish_notifications: bool,
}

impl Store {
    pub fn open_default() -> io::Result<Self> {
        Self::open(agent_jobs_dir(), true)
    }

    pub fn with_root(root: PathBuf) -> io::Result<Self> {
        Self::open(root, false)
    }

    fn open(root: PathBuf, publish_notifications: bool) -> io::Result<Self> {
        crate::agent::util::ensure_durable_private_dir(&root)?;
        for sub in ["pending", "running", "waiting", "done", "locks", "streams"] {
            crate::agent::util::ensure_durable_private_dir(&root.join(sub))?;
        }
        Ok(Self {
            root,
            publish_notifications,
        })
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

    fn stream_prune_tombstone_path(&self, id: &str) -> PathBuf {
        self.root
            .join("streams")
            .join(format!("{id}{STREAM_PRUNE_TOMBSTONE_SUFFIX}"))
    }

    fn job_lock_path(&self, id: &str) -> PathBuf {
        self.root.join("locks").join(format!("{id}.lock"))
    }

    fn notify(&self, job: &Job, phase: &str) {
        if self.publish_notifications {
            publish_task_notification(job, phase);
        }
    }

    fn active_job_exists(&self, id: &str) -> io::Result<bool> {
        for status in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::WaitingApproval,
        ] {
            if path_exists(&self.path_for(status, id))? {
                return Ok(true);
            }
        }
        Ok(false)
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
        for bucket in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::WaitingApproval,
            JobStatus::Ok,
        ] {
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
        self.submit_with_context_and_client(
            prompt,
            None,
            None,
            session_id,
            max_turns,
            owner_uid,
            owner_home,
            crate::session::SessionClient::default(),
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
        self.submit_with_context_and_client(
            prompt,
            context,
            branch_context,
            session_id,
            max_turns,
            owner_uid,
            owner_home,
            crate::session::SessionClient::default(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit_with_context_and_client(
        &self,
        prompt: String,
        context: Option<String>,
        branch_context: Option<String>,
        session_id: Option<String>,
        max_turns: Option<u32>,
        owner_uid: Option<u32>,
        owner_home: Option<String>,
        client: crate::session::SessionClient,
    ) -> io::Result<Job> {
        let job = Job::new_pending_with_client(
            prompt,
            context,
            branch_context,
            session_id,
            max_turns,
            owner_uid,
            owner_home,
            client,
        );
        self.publish(job)
    }

    pub(crate) fn publish(&self, job: Job) -> io::Result<Job> {
        let path = self.path_for(JobStatus::Pending, &job.id);
        write_json_atomic(&path, &job)?;
        crate::clawd::audit::record_task_event("clawd.task.submitted", &job);
        self.notify(&job, "submitted");
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
        self.reconcile_duplicate_job_ids()?;
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
            let _lock = self.lock_for_id(&id)?;
            // Re-check existence after taking the lock — cancel may have
            // already moved the file while we were waiting.
            if !src.exists() {
                continue;
            }
            let raw = match fs::read_to_string(&src) {
                Ok(raw) => raw,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let mut job: Job = match serde_json::from_str(&raw) {
                Ok(job) => job,
                Err(_) => {
                    self.finish_indeterminate(
                        &src,
                        &id,
                        corrupt_job(&id),
                        "pending queue record is malformed; execution history cannot be proven and replay is refused",
                    )?;
                    continue;
                }
            };
            if job.id != id {
                self.finish_indeterminate(
                    &src,
                    &id,
                    corrupt_job(&id),
                    "pending queue record identity conflicts with its filename; replay is refused",
                )?;
                continue;
            }
            if job.schema_version != JOB_SCHEMA_VERSION
                || job.status != JobStatus::Pending
                || job.execution_phase != ExecutionPhase::Unprepared
            {
                self.finish_indeterminate(
                    &src,
                    &id,
                    job,
                    "pending queue record lacks current durable pre-execution proof; legacy or unsupported jobs are not replayed",
                )?;
                continue;
            }
            let dst = self.path_for(JobStatus::Running, &id);
            match durable_rename(&src, &dst) {
                Ok(()) => {
                    // We won — mutate the record already validated before
                    // the move. Legacy/unsupported Pending records never
                    // reach this phase transition.
                    job.status = JobStatus::Running;
                    job.started_at = Some(now_iso());
                    let worker_pid = std::process::id();
                    job.worker_pid = Some(worker_pid);
                    job.worker_start_time_ticks =
                        crate::proc::read_start_time_ticks_pub(worker_pid);
                    job.execution_phase = ExecutionPhase::Preparing;
                    job.execution_prepare_nonce = None;
                    job.execution_commit_nonce = None;
                    job.execution_generation = None;
                    write_json_atomic(&dst, &job)?;
                    crate::clawd::audit::record_task_event("clawd.task.started", &job);
                    self.notify(&job, "started");
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
    /// Returns `(requeued, failed)` counts for logging. Malformed running
    /// records are terminalized because their execution history is unknown.
    pub fn recover_orphaned_jobs(&self) -> io::Result<(usize, usize)> {
        self.reconcile_duplicate_job_ids()?;
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
            let _lock = self.lock_for_id(&id)?;
            // Re-read under the lock; it may have moved since the listing.
            let raw = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            let mut job: Job = match serde_json::from_str(&raw) {
                Ok(j) => j,
                Err(_) => {
                    self.finish_indeterminate(
                        &path,
                        &id,
                        corrupt_job(&id),
                        "running queue record is malformed; execution history is indeterminate and replay is refused",
                    )?;
                    failed += 1;
                    continue;
                }
            };

            if job.cancel_requested_at.is_some() {
                deny_waiting_approvals(&job, "Associated task was cancelled.");
                job.waiting_on.clear();
                job.waiting_since = None;
                job.status = JobStatus::Cancelled;
                job.finished_at = Some(now_iso());
                write_json_atomic(&path, &job)?;
                let done = self.path_for(JobStatus::Ok, &id);
                durable_rename(&path, &done)?;
                let _ = finish_durable_session(&job);
                crate::clawd::audit::record_task_event("clawd.task.cancelled", &job);
                self.notify(&job, "cancelled");
                continue;
            }

            if !job.waiting_on.is_empty() {
                job.status = JobStatus::WaitingApproval;
                job.waiting_since.get_or_insert_with(now_iso);
                job.worker_pid = None;
                job.worker_start_time_ticks = None;
                write_json_atomic(&path, &job)?;
                durable_rename(&path, &self.path_for(JobStatus::WaitingApproval, &id))?;
                crate::clawd::audit::record_task_event("clawd.task.waiting-approval", &job);
                continue;
            }

            if job.schema_version != JOB_SCHEMA_VERSION
                || !job.execution_phase.replay_is_proven_safe()
            {
                self.finish_indeterminate(
                    &path,
                    &id,
                    job,
                    "broker restarted after execution may have begun; outcome is indeterminate and replay is refused",
                )?;
                failed += 1;
                continue;
            }

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
                job.execution_phase = ExecutionPhase::Indeterminate;
                job.error = Some(
                    "worker PID is alive but its start-time identity is unavailable; \
                     refusing to retry a potentially active job"
                        .to_string(),
                );
                job.finished_at = Some(now_iso());
                write_json_atomic(&path, &job)?;
                let done = self.path_for(JobStatus::Ok, &id);
                durable_rename(&path, &done)?;
                let _ = finish_durable_session(&job);
                crate::clawd::audit::record_task_event(
                    "clawd.task.worker_identity_unverifiable",
                    &job,
                );
                failed += 1;
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
                durable_rename(&path, &done)?;
                let _ = finish_durable_session(&job);
                crate::clawd::audit::record_task_event("clawd.task.abandoned", &job);
                self.notify(&job, "failed");
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
            job.execution_phase = ExecutionPhase::Unprepared;
            job.execution_prepare_nonce = None;
            job.execution_commit_nonce = None;
            job.execution_generation = None;
            write_json_atomic(&path, &job)?;
            let pending = self.path_for(JobStatus::Pending, &id);
            durable_rename(&path, &pending)?;
            crate::clawd::audit::record_task_event("clawd.task.recovered", &audit_job);
            self.notify(&audit_job, "recovered");
            requeued += 1;
        }

        Ok((requeued, failed))
    }

    fn reconcile_duplicate_job_ids(&self) -> io::Result<usize> {
        let mut paths = BTreeMap::<String, Vec<(JobStatus, PathBuf)>>::new();
        for bucket in [
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::WaitingApproval,
            JobStatus::Ok,
        ] {
            for entry in fs::read_dir(self.bucket_dir(bucket))? {
                let entry = entry?;
                let path = entry.path();
                if path.extension().and_then(|value| value.to_str()) != Some("json") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
                    continue;
                };
                if validate_job_id(id).is_ok() {
                    paths
                        .entry(id.to_string())
                        .or_default()
                        .push((bucket, path));
                }
            }
        }

        let mut reconciled = 0usize;
        for (id, candidates) in paths {
            if candidates.len() < 2 {
                continue;
            }
            let _lock = self.lock_for_id(&id)?;
            let all_paths = candidates
                .iter()
                .map(|(_, path)| path.clone())
                .collect::<Vec<_>>();
            let mut records = Vec::new();
            let mut corrupt = false;
            for (bucket, path) in candidates {
                match fs::read_to_string(&path)
                    .and_then(|raw| serde_json::from_str::<Job>(&raw).map_err(io_other))
                {
                    Ok(job) if job.id == id => records.push((bucket, path, job)),
                    Ok(_) | Err(_) => corrupt = true,
                }
            }
            if records.len() < 2 && !corrupt {
                continue;
            }
            let identity_conflict = records.first().is_some_and(|(_, _, first)| {
                records
                    .iter()
                    .skip(1)
                    .any(|(_, _, job)| !same_logical_job(first, job))
            });
            let unsafe_copy = records.iter().any(|(bucket, _, job)| {
                *bucket == JobStatus::Ok
                    || job.schema_version != JOB_SCHEMA_VERSION
                    || !job.execution_phase.replay_is_proven_safe()
                    || matches!(
                        job.status,
                        JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled
                    )
            });
            if corrupt || identity_conflict || unsafe_copy {
                let mut dominant = records
                    .iter()
                    .max_by_key(|(bucket, _, job)| {
                        (
                            execution_phase_rank(job.execution_phase),
                            bucket_rank(*bucket),
                        )
                    })
                    .map(|(_, _, job)| job.clone())
                    .unwrap_or_else(|| corrupt_job(&id));
                dominant.status = JobStatus::Error;
                dominant.execution_phase = ExecutionPhase::Indeterminate;
                dominant.error = Some(
                    "conflicting or duplicate durable queue records make execution indeterminate; replay refused"
                        .to_string(),
                );
                dominant.finished_at = Some(now_iso());
                let done = self.path_for(JobStatus::Ok, &id);
                write_json_atomic(&done, &dominant)?;
                for path in &all_paths {
                    if *path != done {
                        durable_remove(path)?;
                    }
                }
                crate::clawd::audit::record_task_event(
                    "clawd.task.duplicate_indeterminate",
                    &dominant,
                );
            } else {
                let Some((_, keep_path, keep_job)) =
                    records.iter().max_by_key(|(bucket, _, job)| {
                        (
                            execution_phase_rank(job.execution_phase),
                            bucket_rank(*bucket),
                        )
                    })
                else {
                    continue;
                };
                for (_, path, _) in &records {
                    if path != keep_path {
                        durable_remove(path)?;
                    }
                }
                crate::clawd::audit::record_task_event(
                    "clawd.task.duplicate_precommit_reconciled",
                    keep_job,
                );
            }
            reconciled += 1;
        }
        Ok(reconciled)
    }

    /// Rebind a claimed job from the broker that claimed it to the
    /// worker process that actually executes it. Recovery and audit
    /// then track the worker's kernel identity rather than `clawd`'s,
    /// which is what makes an abandoned lease detectable.
    pub fn bind_worker(
        &self,
        id: &str,
        worker_pid: u32,
        worker_start_time_ticks: Option<u64>,
    ) -> io::Result<Job> {
        validate_job_id(id)?;
        self.reconcile_duplicate_job_ids()?;
        let _lock = self.lock_for_id(id)?;
        let path = self.path_for(JobStatus::Running, id);
        let raw = fs::read_to_string(&path)?;
        let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
        validate_job_id(&job.id)?;
        if job.id != id
            || job.schema_version != JOB_SCHEMA_VERSION
            || job.status != JobStatus::Running
            || job.execution_phase != ExecutionPhase::Preparing
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker bind requires a current preparing queue record",
            ));
        }
        job.worker_pid = Some(worker_pid);
        job.worker_start_time_ticks = worker_start_time_ticks;
        write_json_atomic(&path, &job)?;
        crate::clawd::audit::record_task_event("clawd.task.worker_bound", &job);
        Ok(job)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_execution_prepared(
        &self,
        id: &str,
        worker_pid: u32,
        worker_start_time_ticks: Option<u64>,
        prepare_nonce: &str,
        commit_nonce: &str,
        generation: &str,
    ) -> io::Result<Job> {
        validate_execution_binding(prepare_nonce, commit_nonce, generation)?;
        validate_job_id(id)?;
        self.reconcile_duplicate_job_ids()?;
        let _lock = self.lock_for_id(id)?;
        let path = self.path_for(JobStatus::Running, id);
        let raw = fs::read_to_string(&path)?;
        let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
        if job.id != id
            || job.schema_version != JOB_SCHEMA_VERSION
            || job.worker_pid != Some(worker_pid)
            || job.worker_start_time_ticks != worker_start_time_ticks
            || job.execution_phase != ExecutionPhase::Preparing
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "job preparation does not match the running worker lease",
            ));
        }
        job.schema_version = JOB_SCHEMA_VERSION;
        job.execution_phase = ExecutionPhase::Prepared;
        job.execution_prepare_nonce = Some(prepare_nonce.to_string());
        job.execution_commit_nonce = Some(commit_nonce.to_string());
        job.execution_generation = Some(generation.to_string());
        write_json_atomic(&path, &job)?;
        crate::clawd::audit::record_task_event("clawd.task.execution_prepared", &job);
        Ok(job)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn commit_execution(
        &self,
        id: &str,
        worker_pid: u32,
        worker_start_time_ticks: Option<u64>,
        prepare_nonce: &str,
        commit_nonce: &str,
        generation: &str,
    ) -> io::Result<Job> {
        validate_execution_binding(prepare_nonce, commit_nonce, generation)?;
        validate_job_id(id)?;
        self.reconcile_duplicate_job_ids()?;
        let _lock = self.lock_for_id(id)?;
        let path = self.path_for(JobStatus::Running, id);
        let raw = fs::read_to_string(&path)?;
        let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
        if job.id != id
            || job.schema_version != JOB_SCHEMA_VERSION
            || job.worker_pid != Some(worker_pid)
            || job.worker_start_time_ticks != worker_start_time_ticks
            || job.execution_phase != ExecutionPhase::Prepared
            || job.execution_prepare_nonce.as_deref() != Some(prepare_nonce)
            || job.execution_commit_nonce.as_deref() != Some(commit_nonce)
            || job.execution_generation.as_deref() != Some(generation)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "job commit does not match the prepared worker lease",
            ));
        }
        job.execution_phase = ExecutionPhase::Committed;
        write_json_atomic(&path, &job)?;
        crate::clawd::audit::record_task_event("clawd.task.execution_committed", &job);
        Ok(job)
    }

    pub fn record_waiting_approval(&self, id: &str, request_id: &str) -> io::Result<Job> {
        validate_job_id(id)?;
        if request_id.is_empty()
            || request_id.len() > 128
            || !request_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid approval request id",
            ));
        }
        let _lock = self.lock_for_id(id)?;
        let path = self.path_for(JobStatus::Running, id);
        let raw = fs::read_to_string(&path)?;
        let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
        if !job.waiting_on.iter().any(|value| value == request_id) {
            job.waiting_on.push(request_id.to_string());
            write_json_atomic(&path, &job)?;
            crate::clawd::audit::record_task_event("clawd.task.approval-requested", &job);
        }
        Ok(job)
    }

    fn finish_indeterminate(
        &self,
        path: &Path,
        id: &str,
        mut job: Job,
        reason: &str,
    ) -> io::Result<Job> {
        job.status = JobStatus::Error;
        job.execution_phase = ExecutionPhase::Indeterminate;
        job.error = Some(reason.to_string());
        job.finished_at = Some(now_iso());
        write_json_atomic(path, &job)?;
        durable_rename(path, &self.path_for(JobStatus::Ok, id))?;
        finish_durable_session(&job)?;
        crate::clawd::audit::record_task_event("clawd.task.execution_indeterminate", &job);
        Ok(job)
    }

    /// Hand a running job back to the queue after its worker failed
    /// without reporting an outcome. Shares the recovery budget with
    /// [`Store::recover_orphaned_jobs`], so a task that keeps killing
    /// workers is failed instead of retried forever.
    ///
    /// Returns the job in its new state, or `Ok(None)` when it is no
    /// longer running (already cancelled or finished).
    pub fn release_for_retry(&self, id: &str, reason: &str) -> io::Result<Option<Job>> {
        validate_job_id(id)?;
        self.reconcile_duplicate_job_ids()?;
        let _lock = self.lock_for_id(id)?;
        let path = self.path_for(JobStatus::Running, id);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
        validate_job_id(&job.id)?;

        if job.cancel_requested_at.is_some() {
            deny_waiting_approvals(&job, "Associated task was cancelled.");
            job.waiting_on.clear();
            job.waiting_since = None;
            job.status = JobStatus::Cancelled;
            job.error = Some("cancelled by user".to_string());
            job.finished_at = Some(now_iso());
            write_json_atomic(&path, &job)?;
            durable_rename(&path, &self.path_for(JobStatus::Ok, id))?;
            let _ = finish_durable_session(&job);
            crate::clawd::audit::record_task_event("clawd.task.cancelled", &job);
            self.notify(&job, "cancelled");
            return Ok(Some(job));
        }

        if !job.waiting_on.is_empty() {
            job.status = JobStatus::WaitingApproval;
            job.waiting_since.get_or_insert_with(now_iso);
            job.worker_pid = None;
            job.worker_start_time_ticks = None;
            write_json_atomic(&path, &job)?;
            durable_rename(&path, &self.path_for(JobStatus::WaitingApproval, id))?;
            crate::clawd::audit::record_task_event("clawd.task.waiting-approval", &job);
            return Ok(Some(job));
        }

        if job.schema_version != JOB_SCHEMA_VERSION || !job.execution_phase.replay_is_proven_safe()
        {
            return self
                .finish_indeterminate(
                    &path,
                    id,
                    job,
                    "worker outcome is unknown after durable execution commit; refusing replay",
                )
                .map(Some);
        }

        job.recovery_count = job.recovery_count.saturating_add(1);
        if job.recovery_count > MAX_RECOVERIES {
            job.status = JobStatus::Error;
            job.error = Some(format!(
                "job abandoned after {} interrupted run(s): {reason}",
                job.recovery_count - 1
            ));
            job.finished_at = Some(now_iso());
            write_json_atomic(&path, &job)?;
            durable_rename(&path, &self.path_for(JobStatus::Ok, id))?;
            let _ = finish_durable_session(&job);
            crate::clawd::audit::record_task_event("clawd.task.abandoned", &job);
            self.notify(&job, "failed");
            return Ok(Some(job));
        }

        job.status = JobStatus::Pending;
        job.worker_pid = None;
        job.worker_start_time_ticks = None;
        job.started_at = None;
        job.waiting_on.clear();
        job.waiting_since = None;
        job.execution_phase = ExecutionPhase::Unprepared;
        job.execution_prepare_nonce = None;
        job.execution_commit_nonce = None;
        job.execution_generation = None;
        write_json_atomic(&path, &job)?;
        durable_rename(&path, &self.path_for(JobStatus::Pending, id))?;
        crate::clawd::audit::record_task_event("clawd.task.recovered", &job);
        self.notify(&job, "recovered");
        Ok(Some(job))
    }

    pub fn finish(&self, job: Job, outcome: FinishOutcome) -> io::Result<Job> {
        validate_job_id(&job.id)?;
        self.reconcile_duplicate_job_ids()?;
        let _lock = self.lock_for_id(&job.id)?;
        let running_path = self.path_for(JobStatus::Running, &job.id);
        let raw = fs::read_to_string(&running_path)?;
        let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
        validate_job_id(&job.id)?;
        let outcome = if job.cancel_requested_at.is_some() {
            if let FinishOutcome::WaitingApproval { request_ids } = &outcome {
                job.waiting_on = request_ids.clone();
            }
            FinishOutcome::Cancelled
        } else {
            outcome
        };
        let revoke_after_pending_cleanup =
            !matches!(&outcome, FinishOutcome::WaitingApproval { .. })
                && !job.waiting_on.is_empty();
        if revoke_after_pending_cleanup {
            deny_waiting_approvals(&job, "The associated task ended.");
            job.waiting_on.clear();
            job.waiting_since = None;
        }
        match outcome {
            FinishOutcome::Ok {
                response,
                turns_used,
                provider,
                model,
                evidence,
                fallback,
            } => {
                if job.execution_phase != ExecutionPhase::Committed {
                    job.status = JobStatus::Error;
                    job.execution_phase = ExecutionPhase::Indeterminate;
                    job.error = Some(
                        "worker reported success without a durable execution commit; result rejected"
                            .to_string(),
                    );
                } else {
                    job.status = JobStatus::Ok;
                    job.response = Some(response);
                    job.turns_used = Some(turns_used);
                    job.provider = Some(provider);
                    job.model = Some(model);
                    job.evidence = *evidence;
                    job.fallback = *fallback;
                }
            }
            FinishOutcome::Error(msg) => {
                job.status = JobStatus::Error;
                job.error = Some(msg);
            }
            FinishOutcome::Indeterminate(msg) => {
                job.status = JobStatus::Error;
                job.execution_phase = ExecutionPhase::Indeterminate;
                job.error = Some(msg);
            }
            FinishOutcome::Cancelled => {
                job.status = JobStatus::Cancelled;
                job.error = Some("cancelled by user".to_string());
            }
            FinishOutcome::WaitingApproval { mut request_ids } => {
                request_ids.sort();
                request_ids.dedup();
                if request_ids.is_empty() {
                    return Err(io::Error::new(
                        ErrorKind::InvalidInput,
                        "waiting task must name at least one approval request",
                    ));
                }
                job.status = JobStatus::WaitingApproval;
                job.waiting_on = request_ids;
                job.waiting_since = Some(now_iso());
                job.worker_pid = None;
                job.worker_start_time_ticks = None;
                job.error = None;
            }
        }
        job.finished_at = (job.status != JobStatus::WaitingApproval).then(now_iso);
        write_json_atomic(&running_path, &job)?;
        durable_rename(&running_path, &self.path_for(job.status, &job.id))?;
        if job.status == JobStatus::WaitingApproval {
            if let Err(error) = self.append_stream_progress(
                &job.id,
                json!({
                    "kind": "waiting_approval",
                    "request_ids": job.waiting_on,
                }),
            ) {
                tracing::warn!(
                    task = %job.id,
                    %error,
                    "failed to append approval-wait progress"
                );
            }
            crate::clawd::audit::record_task_event("clawd.task.waiting-approval", &job);
        } else {
            finish_durable_session(&job)?;
            if revoke_after_pending_cleanup {
                revoke_job_session(&job);
            }
            crate::clawd::audit::record_task_event(
                if job.execution_phase == ExecutionPhase::Indeterminate {
                    "clawd.task.execution_indeterminate"
                } else {
                    "clawd.task.finished"
                },
                &job,
            );
            self.notify(
                &job,
                match job.status {
                    JobStatus::Ok => "completed",
                    JobStatus::Error => "failed",
                    JobStatus::Cancelled => "cancelled",
                    JobStatus::Pending | JobStatus::Running | JobStatus::WaitingApproval => {
                        "finished"
                    }
                },
            );
        }
        Ok(job)
    }

    /// Reconcile tasks that released their worker while waiting for consent.
    pub fn reconcile_waiting_approvals(&self) -> io::Result<(usize, usize)> {
        let waiting = self.bucket_dir(JobStatus::WaitingApproval);
        let entries = match fs::read_dir(&waiting) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok((0, 0)),
            Err(error) => return Err(error),
        };
        let mut resumed = 0usize;
        let mut failed = 0usize;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            let _lock = match self.lock_for_id(id) {
                Ok(lock) => lock,
                Err(_) => continue,
            };
            let raw = match fs::read_to_string(&path) {
                Ok(raw) => raw,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let mut job: Job = match serde_json::from_str(&raw) {
                Ok(job) => job,
                Err(error) => {
                    tracing::warn!(
                        "agent approval reconciliation: skipping malformed job {path:?}: {error}"
                    );
                    continue;
                }
            };
            if job.status != JobStatus::WaitingApproval || job.waiting_on.is_empty() {
                self.fail_waiting_job(
                    &path,
                    id,
                    job,
                    "task has invalid approval-wait state".to_string(),
                    "clawd.task.approval-state-invalid",
                )?;
                failed += 1;
                continue;
            }
            let Some(wait_started) = job
                .waiting_since
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
            else {
                self.fail_waiting_job(
                    &path,
                    id,
                    job,
                    "task has invalid approval-wait timestamp".to_string(),
                    "clawd.task.approval-state-invalid",
                )?;
                failed += 1;
                continue;
            };
            if chrono::Utc::now()
                .signed_duration_since(wait_started.with_timezone(&chrono::Utc))
                .num_seconds()
                > APPROVAL_WAIT_TIMEOUT_SECS
            {
                self.fail_waiting_job(
                    &path,
                    id,
                    job,
                    "task approval wait expired after 8 hours".to_string(),
                    "clawd.task.approval-timeout",
                )?;
                failed += 1;
                continue;
            }

            let statuses = job
                .waiting_on
                .iter()
                .map(|request_id| {
                    (
                        request_id.clone(),
                        crate::approvals::status_for_owner(request_id, job.owner_uid),
                    )
                })
                .collect::<Vec<_>>();
            if statuses.iter().all(|(_, status)| {
                matches!(
                    status,
                    crate::approvals::RequestStatus::Approved
                        | crate::approvals::RequestStatus::Consumed
                )
            }) {
                if job.session_id.is_none() {
                    self.fail_waiting_job(
                        &path,
                        id,
                        job,
                        "waiting task has no durable session".to_string(),
                        "clawd.task.approval-state-invalid",
                    )?;
                    failed += 1;
                    continue;
                }
                job.resumed_after_approval = std::mem::take(&mut job.waiting_on);
                job.waiting_since = None;
                job.status = JobStatus::Pending;
                job.started_at = None;
                job.finished_at = None;
                job.worker_pid = None;
                job.worker_start_time_ticks = None;
                job.cancel_requested_at = None;
                job.error = None;
                job.execution_phase = ExecutionPhase::Unprepared;
                job.execution_prepare_nonce = None;
                job.execution_commit_nonce = None;
                job.execution_generation = None;
                write_json_atomic(&path, &job)?;
                durable_rename(&path, &self.path_for(JobStatus::Pending, id))?;
                if let Err(error) = self.append_stream_progress(
                    id,
                    json!({
                        "kind": "approval_resumed",
                        "request_ids": job.resumed_after_approval,
                    }),
                ) {
                    tracing::warn!(
                        task = %job.id,
                        %error,
                        "failed to append approval-resume progress"
                    );
                }
                crate::clawd::audit::record_task_event("clawd.task.approval-granted", &job);
                self.notify(&job, "resumed");
                resumed += 1;
                continue;
            }
            let terminal = statuses.iter().find(|(_, status)| {
                matches!(
                    status,
                    crate::approvals::RequestStatus::Denied
                        | crate::approvals::RequestStatus::Unknown
                )
            });
            let Some((request_id, status)) = terminal else {
                continue;
            };
            self.fail_waiting_job(
                &path,
                id,
                job,
                format!(
                    "approval request {request_id} is {}; task will not resume",
                    status.as_str()
                ),
                "clawd.task.approval-refused",
            )?;
            failed += 1;
        }

        Ok((resumed, failed))
    }

    fn fail_waiting_job(
        &self,
        path: &Path,
        id: &str,
        mut job: Job,
        message: String,
        event: &'static str,
    ) -> io::Result<Job> {
        deny_waiting_approvals(&job, &message);
        job.status = JobStatus::Error;
        job.error = Some(message);
        job.finished_at = Some(now_iso());
        write_json_atomic(path, &job)?;
        durable_rename(path, &self.path_for(JobStatus::Ok, id))?;
        finish_durable_session(&job)?;
        revoke_job_session(&job);
        crate::clawd::audit::record_task_event(event, &job);
        self.notify(&job, "failed");
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
        self.reconcile_duplicate_job_ids()?;
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
                durable_rename(&src, &dst)?;
                finish_durable_session(&job)?;
                crate::clawd::audit::record_task_event("clawd.task.cancelled", &job);
                self.notify(&job, "cancelled");
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
        self.reconcile_duplicate_job_ids()?;
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
                durable_rename(&pending, &done)?;
                finish_durable_session(&job)?;
                crate::clawd::audit::record_task_event("clawd.task.cancelled", &job);
                self.notify(&job, "cancelled");
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
                    crate::clawd::audit::record_task_event("clawd.task.cancel-requested", &job);
                }
                return Ok(Some((job, false)));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let waiting = self.path_for(JobStatus::WaitingApproval, id);
        match fs::read_to_string(&waiting) {
            Ok(raw) => {
                let mut job: Job = serde_json::from_str(&raw).map_err(io_other)?;
                if !job_visible_to(&job, owner_uid) {
                    return Ok(None);
                }
                job.status = JobStatus::Cancelled;
                job.cancel_requested_at = Some(now_iso());
                job.finished_at = Some(now_iso());
                deny_waiting_approvals(&job, "Associated task was cancelled.");
                job.waiting_on.clear();
                job.waiting_since = None;
                write_json_atomic(&waiting, &job)?;
                durable_rename(&waiting, &done)?;
                finish_durable_session(&job)?;
                crate::clawd::audit::record_task_event("clawd.task.cancelled", &job);
                self.notify(&job, "cancelled");
                Ok(Some((job, true)))
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

    /// Resolve stream staging left by an interrupted prune before selecting
    /// new terminal records: restore it when the record survived, otherwise
    /// finish removing the orphaned payload.
    fn recover_prune_tombstones(&self) -> io::Result<()> {
        let stream_dir = self.root.join("streams");
        let mut tombstones = Vec::new();
        for entry in fs::read_dir(&stream_dir)? {
            let entry = entry?;
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(id) = file_name.strip_suffix(STREAM_PRUNE_TOMBSTONE_SUFFIX) else {
                continue;
            };
            if validate_job_id(id).is_err() {
                continue;
            }
            tombstones.push((id.to_string(), path));
        }

        for (id, tombstone_path) in tombstones {
            let _job_lock = match self.lock_for_id(&id) {
                Ok(lock) => lock,
                Err(_) => continue,
            };
            if !matches!(self.active_job_exists(&id), Ok(false)) {
                continue;
            }

            let stream_path = self.stream_path(&id);
            let record_path = self.path_for(JobStatus::Ok, &id);
            crate::filelock::with_exclusive_path_lock(&stream_path, || {
                let record_exists = path_exists(&record_path)
                    .map_err(|error| format!("inspect {}: {error}", record_path.display()))?;
                let stream_exists = path_exists(&stream_path)
                    .map_err(|error| format!("inspect {}: {error}", stream_path.display()))?;

                if record_exists && !stream_exists {
                    durable_rename(&tombstone_path, &stream_path).map_err(|error| {
                        format!(
                            "restore {} to {}: {error}",
                            tombstone_path.display(),
                            stream_path.display()
                        )
                    })?;
                } else if record_exists {
                    durable_remove(&tombstone_path)
                        .map_err(|error| format!("remove {}: {error}", tombstone_path.display()))?;
                } else {
                    let stream_removed = durable_remove(&stream_path)
                        .map_err(|error| format!("remove {}: {error}", stream_path.display()))?;
                    let tombstone_removed = durable_remove(&tombstone_path)
                        .map_err(|error| format!("remove {}: {error}", tombstone_path.display()))?;
                    let _ = (stream_removed, tombstone_removed);
                }
                Ok(())
            })
            .map_err(io_other)?;
        }

        Ok(())
    }

    /// Delete terminal jobs older than `older_than` (mtime-based) and
    /// beyond the most recent `keep_last`, including their stream payloads.
    /// Returns the number of complete job records removed.
    pub fn prune(&self, older_than: Duration, keep_last: usize) -> io::Result<usize> {
        self.prune_with_record_remove(older_than, keep_last, remove_file_if_exists)
    }

    fn prune_with_record_remove<F>(
        &self,
        older_than: Duration,
        keep_last: usize,
        mut remove_record: F,
    ) -> io::Result<usize>
    where
        F: FnMut(&Path) -> io::Result<bool>,
    {
        self.recover_prune_tombstones()?;
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
            if age < older_than {
                continue;
            }
            let Some(id) = p.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let _job_lock = match self.lock_for_id(id) {
                Ok(lock) => lock,
                Err(_) => continue,
            };

            // A stale/duplicated done entry must never cause the payload for
            // a live job with the same id to be removed.
            if !matches!(self.active_job_exists(id), Ok(false)) {
                continue;
            }

            let stream_path = self.stream_path(id);
            let tombstone_path = self.stream_prune_tombstone_path(id);
            let cleanup = crate::filelock::with_exclusive_path_lock(&stream_path, || {
                if path_exists(&tombstone_path)
                    .map_err(|error| format!("inspect {}: {error}", tombstone_path.display()))?
                {
                    return Err(format!(
                        "stream prune tombstone already exists: {}",
                        tombstone_path.display()
                    ));
                }

                let stream_staged = match fs::symlink_metadata(&stream_path) {
                    Ok(metadata) => {
                        let file_type = metadata.file_type();
                        if !file_type.is_file() && !file_type.is_symlink() {
                            return Err(format!(
                                "stream payload is not a file: {}",
                                stream_path.display()
                            ));
                        }
                        durable_rename(&stream_path, &tombstone_path).map_err(|error| {
                            format!(
                                "stage {} as {}: {error}",
                                stream_path.display(),
                                tombstone_path.display()
                            )
                        })?;
                        true
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => false,
                    Err(error) => {
                        return Err(format!("inspect {}: {error}", stream_path.display()));
                    }
                };

                let record_removed = match remove_record(&p) {
                    Ok(removed) => removed,
                    Err(error) => {
                        if stream_staged {
                            restore_staged_stream(
                                &stream_path,
                                &tombstone_path,
                                &self.root.join("streams"),
                            )
                            .map_err(|restore_error| {
                                format!(
                                    "remove {}: {error}; restore stream: {restore_error}",
                                    p.display()
                                )
                            })?;
                        }
                        return Err(format!("remove {}: {error}", p.display()));
                    }
                };

                if record_removed {
                    crate::agent::util::sync_dir(&dir)
                        .map_err(|error| format!("sync {}: {error}", dir.display()))?;
                } else {
                    match path_exists(&p) {
                        Ok(false) => {}
                        Ok(true) => {
                            if stream_staged {
                                restore_staged_stream(
                                    &stream_path,
                                    &tombstone_path,
                                    &self.root.join("streams"),
                                )
                                .map_err(|error| format!("restore stream: {error}"))?;
                            }
                            return Err(format!(
                                "record remover left terminal record in place: {}",
                                p.display()
                            ));
                        }
                        Err(error) => {
                            if stream_staged {
                                restore_staged_stream(
                                    &stream_path,
                                    &tombstone_path,
                                    &self.root.join("streams"),
                                )
                                .map_err(|restore_error| {
                                    format!(
                                        "inspect {}: {error}; restore stream: {restore_error}",
                                        p.display()
                                    )
                                })?;
                            }
                            return Err(format!("inspect {}: {error}", p.display()));
                        }
                    }
                }

                if stream_staged {
                    durable_remove(&tombstone_path)
                        .map_err(|error| format!("remove {}: {error}", tombstone_path.display()))?;
                }
                Ok(record_removed)
            });

            let Ok(record_removed) = cleanup else {
                continue;
            };

            // Keep both lock sentinels. Unlinking either after releasing its
            // flock can split queued waiters and new callers across inodes.
            if record_removed {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn counts(&self) -> io::Result<(usize, usize, usize, usize)> {
        let p = count_json(&self.bucket_dir(JobStatus::Pending))?;
        let r = count_json(&self.bucket_dir(JobStatus::Running))?;
        let w = count_json(&self.bucket_dir(JobStatus::WaitingApproval))?;
        let d = count_json(&self.bucket_dir(JobStatus::Ok))?;
        Ok((p, r, w, d))
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
        crate::agent::util::ensure_durable_private_dir(&lock_dir)?;
        let lock_path = self.job_lock_path(id);
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

fn same_logical_job(left: &Job, right: &Job) -> bool {
    left.id == right.id
        && left.prompt == right.prompt
        && left.context == right.context
        && left.branch_context == right.branch_context
        && left.session_id == right.session_id
        && left.max_turns == right.max_turns
        && left.use_memory == right.use_memory
        && left.created_at == right.created_at
        && left.owner_uid == right.owner_uid
        && left.owner_home == right.owner_home
        && left.client == right.client
}

fn execution_phase_rank(phase: ExecutionPhase) -> u8 {
    match phase {
        ExecutionPhase::Unprepared => 0,
        ExecutionPhase::Preparing => 1,
        ExecutionPhase::Prepared => 2,
        ExecutionPhase::Committed => 3,
        ExecutionPhase::LegacyUnknown => 4,
        ExecutionPhase::Indeterminate => 5,
    }
}

fn bucket_rank(status: JobStatus) -> u8 {
    match status {
        JobStatus::Pending => 0,
        JobStatus::Running => 1,
        JobStatus::WaitingApproval => 2,
        JobStatus::Ok | JobStatus::Error | JobStatus::Cancelled => 3,
    }
}

fn corrupt_job(id: &str) -> Job {
    let mut job = Job::new_pending(
        "queue record corruption".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    job.id = id.to_string();
    job.status = JobStatus::Error;
    job.execution_phase = ExecutionPhase::Indeterminate;
    job
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

fn validate_execution_binding(
    prepare_nonce: &str,
    commit_nonce: &str,
    generation: &str,
) -> io::Result<()> {
    let nonce_is_valid = |value: &str| {
        value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    };
    if !nonce_is_valid(prepare_nonce)
        || !nonce_is_valid(commit_nonce)
        || prepare_nonce == commit_nonce
        || generation.is_empty()
        || generation.len() > 128
        || !generation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid durable execution binding",
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
    Indeterminate(String),
    Cancelled,
    WaitingApproval {
        request_ids: Vec<String>,
    },
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
        JobStatus::WaitingApproval => JobStatus::WaitingApproval,
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

fn remove_file_if_exists(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn path_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn restore_staged_stream(
    stream_path: &Path,
    tombstone_path: &Path,
    stream_dir: &Path,
) -> io::Result<()> {
    if path_exists(stream_path)? {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "refusing to replace stream created during rollback: {}",
                stream_path.display()
            ),
        ));
    }
    let _ = stream_dir;
    durable_rename(tombstone_path, stream_path)
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

fn durable_rename(source: &Path, destination: &Path) -> io::Result<()> {
    let source_dir = source
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"))?;
    let destination_dir = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    crate::agent::util::persistence_barrier("cross_rename")?;
    fs::rename(source, destination)?;
    let result = (|| {
        crate::agent::util::persistence_barrier("after_cross_rename")?;
        crate::agent::util::sync_dir(destination_dir)?;
        crate::agent::util::persistence_barrier("after_destination_dir_fsync")?;
        if source_dir != destination_dir {
            crate::agent::util::persistence_barrier("before_source_dir_fsync")?;
            crate::agent::util::sync_dir(source_dir)?;
        }
        Ok(())
    })();
    result.map_err(queue_visible_mutation_error)
}

fn durable_remove(path: &Path) -> io::Result<bool> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"))?;
    match fs::remove_file(path) {
        Ok(()) => crate::agent::util::sync_dir(parent)
            .map_err(queue_visible_mutation_error)
            .map(|()| true),

        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn queue_visible_mutation_error(error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "queue durability is indeterminate after a visible mutation; do not retry until reconciliation: {error}"
        ),
    )
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
        JobStatus::Pending | JobStatus::Running | JobStatus::WaitingApproval => return Ok(()),
    };
    crate::session::end(&sid, status).map_err(io_other)
}

fn deny_waiting_approvals(job: &Job, note: &str) {
    for request_id in &job.waiting_on {
        if let Err(error) = crate::approvals::deny_if_pending_for_owner(
            request_id,
            None,
            Some(note.to_string()),
            job.owner_uid,
        ) {
            tracing::warn!(
                task = %job.id,
                approval = request_id,
                %error,
                "failed to retire approval for terminal task"
            );
        }
    }
}

fn revoke_job_session(job: &Job) {
    let Some(session_id) = job.session_id.as_deref() else {
        return;
    };
    match job.owner_uid {
        Some(owner_uid) => crate::clawd::authority::revoke_session_for_owner(session_id, owner_uid),
        None => crate::clawd::authority::revoke_session(session_id),
    }
}

fn publish_task_notification(job: &Job, phase: &str) {
    let Some(owner_uid) = job.owner_uid.filter(|uid| *uid != 0) else {
        return;
    };
    let trigger = job
        .session_id
        .as_deref()
        .and_then(|session_id| session_id.parse::<crate::session::SessionId>().ok())
        .and_then(|session_id| crate::session::get_meta(&session_id).ok())
        .is_some_and(|meta| meta.origin == Some(crate::session::SessionOrigin::TriggerDelegation));
    let source = if trigger { "trigger" } else { "agent" };
    let kind = if trigger {
        format!("trigger.agent.{phase}")
    } else {
        format!("agent.{phase}")
    };
    let (severity, title, body, activity) = match phase {
        "submitted" => (
            crate::notifications::Severity::Info,
            "Agent task queued",
            "A background Agent task is waiting to run.",
            true,
        ),
        "started" => (
            crate::notifications::Severity::Info,
            "Agent task started",
            "A background Agent task has started.",
            true,
        ),
        "recovered" => (
            crate::notifications::Severity::Warning,
            "Agent task restarted",
            "A background Agent task was recovered after its worker stopped.",
            true,
        ),
        "resumed" => (
            crate::notifications::Severity::Info,
            "Agent task resumed",
            "A background Agent task resumed after approval.",
            true,
        ),
        "completed" => (
            crate::notifications::Severity::Info,
            "Agent task completed",
            "A background Agent task finished successfully.",
            false,
        ),
        "cancelled" => (
            crate::notifications::Severity::Warning,
            "Agent task cancelled",
            "A background Agent task was cancelled.",
            false,
        ),
        _ => (
            crate::notifications::Severity::Error,
            "Agent task failed",
            "A background Agent task failed. Open the Agent to inspect the result.",
            false,
        ),
    };
    let mut draft =
        crate::notifications::NotificationDraft::new(source, kind, severity, title, body)
            .dedupe(format!("task:{}:{phase}", job.id));
    if activity {
        draft = draft.activity();
    }
    draft.task_id = Some(job.id.clone());
    draft.session_id = job.session_id.clone();
    if let Some(session_id) = job.session_id.as_deref() {
        draft
            .actions
            .push(crate::notifications::NotificationAction {
                id: "open-agent".to_string(),
                label: "Open Agent".to_string(),
                uri: format!("clawos://agent/session/{session_id}"),
            });
    }
    if let Err(error) = crate::clawd::notifications::publish_for_owner(owner_uid, draft) {
        tracing::warn!(
            task = %job.id,
            owner_uid,
            phase,
            %error,
            "failed to publish Agent task notification"
        );
    }
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
        "schema_version": job.schema_version,
        "execution_phase": job.execution_phase.as_str(),
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
        Some(JobStatus::WaitingApproval) => vec![JobStatus::WaitingApproval],
        Some(JobStatus::Ok) | Some(JobStatus::Error) | Some(JobStatus::Cancelled) => {
            vec![JobStatus::Ok]
        }
        None => vec![
            JobStatus::Pending,
            JobStatus::Running,
            JobStatus::WaitingApproval,
            JobStatus::Ok,
        ],
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
        let (p, r, w, d) = store.counts().map_err(|e| e.to_string())?;
        return Ok(json!({
            "queue_dir": store.root().display().to_string(),
            "pending": p,
            "running": r,
            "waiting_approval": w,
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
    match store
        .request_cancel_for_owner(id, None)
        .map_err(|e| e.to_string())?
    {
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
    // The in-process loop is the model/tool runtime. It exists for
    // stand-alone `cos agent service work` operators; inside `clawd`
    // agent work is spawned into an unprivileged `claw-agentd` process
    // by `agentd::supervisor` instead.
    crate::agentd::guard::ensure_agent_runtime_allowed("in-process agent worker loop")?;
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
                let prepare_nonce = uuid::Uuid::new_v4().simple().to_string();
                let commit_nonce = uuid::Uuid::new_v4().simple().to_string();
                let generation = crate::crypto::sha256_hex(b"cos.agent.service.direct-worker.v1");
                let prepared = store.record_execution_prepared(
                    &job.id,
                    job.worker_pid.unwrap_or_default(),
                    job.worker_start_time_ticks,
                    &prepare_nonce,
                    &commit_nonce,
                    &generation,
                );
                let committed = prepared.and_then(|_| {
                    store.commit_execution(
                        &job.id,
                        job.worker_pid.unwrap_or_default(),
                        job.worker_start_time_ticks,
                        &prepare_nonce,
                        &commit_nonce,
                        &generation,
                    )
                });
                let mut outcome = match committed {
                    Ok(_) => runtime.block_on(run_one_job(&job)),
                    Err(error) => FinishOutcome::Indeterminate(format!(
                        "direct worker could not durably commit execution: {error}"
                    )),
                };
                if store.cancellation_requested(&job.id).unwrap_or(false) {
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
    // running the rest of the job. Every `config::current_snapshot()` inside the
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
        let cfg = crate::config::load_for_home(&home_path);
        let run = crate::config::with_snapshot(cfg, run_one_job_inner(job));
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
    let stream_sink: Arc<dyn crate::agent::llm::accumulate::StreamSink> = Arc::new(JobStreamSink {
        job_id: job.id.clone(),
    });
    let progress_sink: Arc<dyn crate::agent::runtime::progress::ProgressSink> =
        Arc::new(JobProgressSink {
            job_id: job.id.clone(),
        });
    execute_job(
        JobExecution {
            id: job.id.clone(),
            prompt: job.prompt.clone(),
            context: job.context.clone(),
            branch_context: job.branch_context.clone(),
            session_id: job.session_id.clone(),
            max_turns: job.max_turns,
            use_memory: job.use_memory,
            presence: None,
        },
        stream_sink,
        progress_sink,
    )
    .await
}

/// One unit of agent work, decoupled from where the job record lives.
/// The `agentd` worker builds this from a supervisor assignment and
/// never touches the queue; the in-process loop builds it from a
/// claimed [`Job`].
#[derive(Debug, Clone)]
pub struct JobExecution {
    pub id: String,
    pub prompt: String,
    pub context: Option<String>,
    pub branch_context: Option<String>,
    pub session_id: Option<String>,
    pub max_turns: Option<u32>,
    pub use_memory: bool,
    pub presence: Option<crate::session::SessionPresence>,
}

fn standalone_runtime_hooks() -> crate::agent::runtime::hooks::HookRegistry {
    let hooks = crate::agent::runtime::hooks::HookRegistry::new();
    hooks.register(crate::clawd::audit::runtime_hook());
    hooks
}

/// Run the agent loop for one task.
///
/// This is the model/tool runtime proper — provider construction, MCP
/// attachment, the guarded tool registry and prompt assembly all happen
/// here — so it refuses to execute inside the privileged broker.
pub async fn execute_job(
    job: JobExecution,
    stream_sink: Arc<dyn crate::agent::llm::accumulate::StreamSink>,
    progress_sink: Arc<dyn crate::agent::runtime::progress::ProgressSink>,
) -> FinishOutcome {
    execute_job_with_hooks(
        job,
        stream_sink,
        progress_sink,
        standalone_runtime_hooks(),
    )
    .await
}

pub async fn execute_job_with_hooks(
    job: JobExecution,
    stream_sink: Arc<dyn crate::agent::llm::accumulate::StreamSink>,
    progress_sink: Arc<dyn crate::agent::runtime::progress::ProgressSink>,
    hooks: crate::agent::runtime::hooks::HookRegistry,
) -> FinishOutcome {
    use crate::agent::runtime::loop_;

    if let Err(error) = crate::agentd::guard::ensure_agent_runtime_allowed("agent job execution") {
        return FinishOutcome::Error(error);
    }

    let current_config = crate::config::current_snapshot();
    let base = current_config.agent.clone();
    let mut cfg = base;
    if let Some(n) = job.max_turns {
        cfg.max_turns = n;
    }
    let provider = match crate::ai::gate::build_system_provider(&cfg) {
        Ok(p) => p,
        Err(e) => return FinishOutcome::Error(format!("provider unavailable: {e}")),
    };
    let guardrails = loop_::guardrails_from_cfg(&cfg);
    let exposure =
        match crate::agent::tools::exposure::ToolExposureContext::from_current_session_with_presence(
            job.session_id.as_deref(),
            Some(&job.id),
            crate::agent::tools::exposure::ExecutionHost::AgentWorker,
            guardrails.clone(),
            job.presence,
        ) {
            Ok(context) => context,
            Err(error) if job.session_id.is_none() => {
                tracing::warn!(%error, "agent job has no authenticated session; exposing only unscoped tools");
                crate::agent::tools::exposure::ToolExposureContext::isolated(
                    loop_::guardrails_from_cfg(&cfg),
                )
            }
            Err(error) => return FinishOutcome::Error(error),
        };
    let mut effective_config = (*current_config).clone();
    effective_config.agent = cfg.clone();
    let registry_deps = crate::agent::tools::registry::RegistryDeps::load_with_hooks(
        Arc::new(effective_config),
        crate::agent::tools::registry::RegistryPaths::from_process(),
        hooks.clone(),
    );
    let runtime_deps = registry_deps.runtime.clone();
    let mut tools = crate::agent::tools::registry::default_registry_with_deps(&registry_deps);
    tools.set_guardrails(guardrails);
    tools.set_approval(loop_::approval_from_cfg(&cfg));
    let _mcp_handles = loop_::attach_mcp_servers_for_cli(&mut tools, &cfg, &exposure).await;

    let request = loop_::RuntimeRequest::streaming(
        provider,
        &cfg,
        &job.prompt,
        &tools,
        stream_sink,
        progress_sink,
    )
    .with_exposure(&exposure)
    .with_transient_context(job.context.as_deref())
    .with_interrupt_scope(&job.id);
    let request = if job.use_memory {
        match (job.session_id.as_deref(), registry_deps.memory.as_ref()) {
            (Some(sid), Some(db)) => {
                if let Some(context) = job
                    .branch_context
                    .as_deref()
                    .filter(|context| !context.trim().is_empty())
                {
                    if let Err(error) = seed_branch_context(db, sid, context) {
                        tracing::warn!(
                            session_id = sid,
                            %error,
                            "failed to seed retry branch context"
                        );
                    }
                }
                request.with_continuation(db, sid, 100)
            }
            _ => request,
        }
    } else {
        request
    };
    let result = loop_::run_with_deps(&runtime_deps, request).await;

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
    use crate::agent::trust::{envelope, LabeledSegment, SourceKind};

    let bounded = clip_progress_text(context.trim(), 32 * 1024);
    let segment = LabeledSegment::of(SourceKind::TransientAppContext, bounded);
    let wrapped = segment.render_fenced(envelope::process_seal());
    let already_seeded = db
        .recent(session_id, 20)
        .map_err(|error| error.to_string())?
        .iter()
        .any(|row| {
            row.role == "system"
                && LabeledSegment::from_stored(&row.content).content() == segment.content()
        });
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
        "waiting" | "waiting_approval" | "approval" => Ok(JobStatus::WaitingApproval),
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/service.rs"
    ));
}
