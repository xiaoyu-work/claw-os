//! @agent-file
//! Responsibility: project bounded owner-scoped Job records for one conversation.
//! Key dependencies: the canonical Agent job store and conversation session identity.
//! Constraints: expose actual records only and do not claim message-binding completeness.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::agent::service::{Job, JobStatus, Store};
use crate::session::SessionId;

const MAX_JOBS: usize = 1_000;
const MAX_JOB_BYTES: usize = 2 * 1024 * 1024;
pub(super) const UNVERIFIED_BINDINGS: &str = "execution records are not a verified replay view";

#[derive(Debug, Serialize)]
pub(super) struct ConversationJob {
    pub(super) id: String,
    pub(super) status: JobStatus,
    pub(super) prompt: String,
    pub(super) created_at: String,
    pub(super) started_at: Option<String>,
    pub(super) finished_at: Option<String>,
    pub(super) session_id: String,
    pub(super) error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ConversationJobs {
    pub(super) jobs: Vec<ConversationJob>,
    pub(super) job_count: u64,
    pub(super) jobs_truncated: bool,
    pub(super) task_bindings_complete: bool,
    pub(super) task_bindings_error: String,
}

impl Default for ConversationJobs {
    fn default() -> Self {
        Self {
            jobs: Vec::new(),
            job_count: 0,
            jobs_truncated: false,
            task_bindings_complete: false,
            task_bindings_error: UNVERIFIED_BINDINGS.to_string(),
        }
    }
}

pub(super) fn load(sid: &SessionId, owner_uid: u32) -> Result<ConversationJobs, String> {
    match std::fs::metadata(crate::paths::agent_jobs_dir()) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err("agent job store is not a directory".to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConversationJobs::default());
        }
        Err(error) => return Err(format!("inspect agent job store: {error}")),
    }
    let store = Store::open_default().map_err(|error| format!("open agent jobs: {error}"))?;
    collect(&store, sid, owner_uid)
}

fn collect(store: &Store, sid: &SessionId, owner_uid: u32) -> Result<ConversationJobs, String> {
    let mut observed = BTreeMap::new();
    for bucket in [
        JobStatus::Pending,
        JobStatus::Running,
        JobStatus::WaitingApproval,
        JobStatus::Ok,
    ] {
        for job in store
            .list_bucket_for_owner(bucket, None, Some(owner_uid))
            .map_err(|error| format!("read conversation jobs: {error}"))?
        {
            if job.session_id.as_deref() == Some(sid.as_str()) {
                observed.insert(job.id.clone(), job);
            }
        }
    }
    bounded_projection(observed.into_values().collect(), sid)
}

fn bounded_projection(jobs: Vec<Job>, sid: &SessionId) -> Result<ConversationJobs, String> {
    let mut ordered = jobs
        .into_iter()
        .map(|job| {
            let created = super::timestamp(&job.created_at)?;
            for timestamp in [&job.started_at, &job.finished_at].into_iter().flatten() {
                super::timestamp(timestamp)?;
            }
            Ok((created, job))
        })
        .collect::<Result<Vec<_>, String>>()?;
    ordered.sort_by(|(left_time, left), (right_time, right)| {
        right_time
            .cmp(left_time)
            .then_with(|| right.id.cmp(&left.id))
    });

    let job_count = u64::try_from(ordered.len())
        .map_err(|_| "conversation job count is invalid".to_string())?;
    let mut jobs = Vec::new();
    let mut response_bytes = 2usize;
    for (_, job) in ordered.into_iter().take(MAX_JOBS) {
        let session_id = job
            .session_id
            .ok_or_else(|| "conversation job has no recorded session".to_string())?;
        if session_id != sid.as_str() {
            return Err("conversation job is associated with another session".to_string());
        }
        let projected = ConversationJob {
            id: job.id,
            status: job.status,
            prompt: job.prompt,
            created_at: job.created_at,
            started_at: job.started_at,
            finished_at: job.finished_at,
            session_id,
            error: job.error,
        };
        let separator = usize::from(!jobs.is_empty());
        let job_bytes = serde_json::to_vec(&projected)
            .map_err(|error| format!("encode conversation job: {error}"))?
            .len();
        if response_bytes
            .saturating_add(separator)
            .saturating_add(job_bytes)
            > MAX_JOB_BYTES
        {
            if jobs.is_empty() {
                return Err("conversation job exceeds the bounded metadata response".to_string());
            }
            break;
        }
        response_bytes = response_bytes
            .saturating_add(separator)
            .saturating_add(job_bytes);
        jobs.push(projected);
    }
    jobs.reverse();
    Ok(ConversationJobs {
        jobs_truncated: job_count > jobs.len() as u64,
        job_count,
        jobs,
        ..ConversationJobs::default()
    })
}

#[cfg(test)]
mod tests {
    include!("../../../test/unit/clawd/conversations/jobs.rs");
}
