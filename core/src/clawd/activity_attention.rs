//! Read-only Activity attention projected from durable jobs, consent and notifications.

use std::collections::BTreeSet;

use serde::Serialize;
use serde_json::{json, Value};

use crate::activities::ActivityService;
use crate::agent::service::{ExecutionPhase, Job, JobStatus, Store};
use crate::approvals::{self, presentation::LegacyReview};
use crate::caps::{Risk, Scope, Verb};
use crate::notifications::{NotificationService, TaskNotificationPage};

use super::activities::{decode, list_limit, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests::ActivityGet;

const MAX_SCOPE_BYTES: usize = 4096;
const MAX_PREVIEW_CHARS: usize = 1024;

#[derive(Default, Serialize)]
struct Counts {
    queued: u64,
    running: u64,
    waiting: u64,
    completed: u64,
    failed: u64,
    cancelled: u64,
    indeterminate: u64,
    pending_decisions: u64,
    unavailable_decisions: u64,
    unread_notifications: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum DecisionStatus {
    Pending,
    Unavailable,
    Approved,
    Denied,
}

impl DecisionStatus {
    fn priority(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::Unavailable => 1,
            Self::Approved | Self::Denied => 2,
        }
    }
}

#[derive(Serialize)]
struct Decision {
    id: String,
    job_id: String,
    session_id: Option<String>,
    status: DecisionStatus,
    requested_at: Option<u64>,
    verb: Option<String>,
    scope: Option<Scope>,
    risk: Option<Risk>,
    reason: Option<String>,
    review_id: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum IssueKind {
    Indeterminate,
    WaitingApproval,
    Failed,
}

impl IssueKind {
    fn priority(self) -> u8 {
        match self {
            Self::Indeterminate => 0,
            Self::WaitingApproval => 1,
            Self::Failed => 2,
        }
    }
}

#[derive(Serialize)]
struct Issue {
    job_id: String,
    session_id: Option<String>,
    kind: IssueKind,
    status: &'static str,
    execution_phase: &'static str,
    title: String,
    created_at: String,
    finished_at: Option<String>,
    message: String,
}

pub fn get(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner_uid = owner(client)?;
    let request: ActivityGet = decode(params)?;
    let limit = list_limit(request.limit)?;
    crate::activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let activity = crate::activities::open_default()
        .map_err(service_error)?
        .get(owner_uid, request.id.as_str())
        .map_err(service_error)?;
    let jobs = Store::open_default()
        .and_then(|store| store.list_for_activity(owner_uid, &activity.id, usize::MAX))
        .map_err(|error| {
            BrokerError::unavailable(format!("read Activity attention jobs: {error}"))
        })?;
    let notifications = if jobs.is_empty() {
        TaskNotificationPage::default()
    } else {
        crate::notifications::open_default()
            .and_then(|service| {
                service.list_tasks(
                    owner_uid,
                    &jobs.iter().map(|job| job.id.clone()).collect::<Vec<_>>(),
                    limit,
                )
            })
            .map_err(|error| {
                BrokerError::unavailable(format!("read Activity notifications: {error}"))
            })?
    };
    let (mut counts, mut decisions, mut issues) = project_jobs(owner_uid, &jobs);
    counts.unread_notifications = notifications.unread;
    let decision_total = decisions.len();
    let issue_total = issues.len();
    decisions.sort_by(|left, right| {
        left.status
            .priority()
            .cmp(&right.status.priority())
            .then_with(|| left.requested_at.cmp(&right.requested_at))
            .then_with(|| left.job_id.cmp(&right.job_id))
            .then_with(|| left.id.cmp(&right.id))
    });
    issues.sort_by(|left, right| {
        left.kind
            .priority()
            .cmp(&right.kind.priority())
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.job_id.cmp(&right.job_id))
    });
    decisions.truncate(limit);
    issues.truncate(limit);
    Ok(json!({
        "schema": 1,
        "activity_id": activity.id,
        "activity_state": activity.state,
        "limit": limit,
        "counts": counts,
        "decisions": decisions,
        "issues": issues,
        "totals": {
            "decisions": decision_total,
            "issues": issue_total,
            "notifications": notifications.total,
        },
        "has_more": {
            "decisions": decision_total > limit,
            "issues": issue_total > limit,
            "notifications": notifications.total > notifications.notifications.len() as u64,
        },
        "notifications": notifications.notifications,
    }))
}

fn project_jobs(owner_uid: u32, jobs: &[Job]) -> (Counts, Vec<Decision>, Vec<Issue>) {
    let mut counts = Counts::default();
    let mut decisions = Vec::new();
    let mut issues = Vec::new();
    for job in jobs {
        match job.status {
            JobStatus::Pending => counts.queued += 1,
            JobStatus::Running => counts.running += 1,
            JobStatus::WaitingApproval => counts.waiting += 1,
            JobStatus::Ok => counts.completed += 1,
            JobStatus::Error => counts.failed += 1,
            JobStatus::Cancelled => counts.cancelled += 1,
        }
        let kind = if matches!(
            job.execution_phase,
            ExecutionPhase::Indeterminate | ExecutionPhase::LegacyUnknown
        ) {
            counts.indeterminate += 1;
            Some(IssueKind::Indeterminate)
        } else {
            match job.status {
                JobStatus::WaitingApproval => Some(IssueKind::WaitingApproval),
                JobStatus::Error => Some(IssueKind::Failed),
                _ => None,
            }
        };
        if let Some(kind) = kind {
            let message = match kind {
                IssueKind::Indeterminate => job.error.as_deref().unwrap_or(
                    "Execution outcome is not established; inspect the task before retrying.",
                ),
                IssueKind::WaitingApproval => {
                    "This task is waiting for permission decisions; notification acknowledgement is not consent."
                }
                IssueKind::Failed => job
                    .error
                    .as_deref()
                    .unwrap_or("This attempt failed; open the task to inspect its result."),
            };
            issues.push(Issue {
                job_id: job.id.clone(),
                session_id: job.session_id.clone(),
                kind,
                status: job.status.as_str(),
                execution_phase: job.execution_phase.as_str(),
                title: excerpt(&job.prompt, 160),
                created_at: job.created_at.clone(),
                finished_at: job.finished_at.clone(),
                message: excerpt(message, MAX_PREVIEW_CHARS),
            });
        }
        for id in job
            .waiting_on
            .iter()
            .chain(&job.resumed_after_approval)
            .collect::<BTreeSet<_>>()
        {
            let decision = read_decision(owner_uid, job, id);
            if job.status == JobStatus::WaitingApproval
                || decision.status != DecisionStatus::Pending
            {
                counts.pending_decisions += u64::from(decision.status == DecisionStatus::Pending);
                counts.unavailable_decisions +=
                    u64::from(decision.status == DecisionStatus::Unavailable);
                decisions.push(decision);
            }
        }
    }
    (counts, decisions, issues)
}

fn read_decision(owner_uid: u32, job: &Job, id: &str) -> Decision {
    let (request, status) = match approvals::presentation::legacy_get(owner_uid, id) {
        Ok(LegacyReview::Pending(request)) => (*request, DecisionStatus::Pending),
        Ok(LegacyReview::Resolved(resolved)) => {
            let status = match resolved.decision.outcome {
                approvals::Outcome::Approved => DecisionStatus::Approved,
                approvals::Outcome::Denied => DecisionStatus::Denied,
            };
            (resolved.request, status)
        }
        Err(error) => {
            tracing::warn!(
                owner_uid,
                task = %job.id,
                approval_id = id,
                error_digest = %crate::crypto::sha256_hex(error.as_bytes()),
                "Activity approval details are unavailable"
            );
            return unavailable(
                job,
                id,
                "Approval details are unavailable or changing; refresh the task or open OS reviews.",
            );
        }
    };
    if request.owner_uid != Some(owner_uid)
        || job.session_id.as_deref() != Some(request.session.as_str())
        || !request
            .execution
            .as_ref()
            .is_some_and(|execution| execution.identity.task_id == job.id)
    {
        return unavailable(job, id, "The request is not linked to this Activity task.");
    }
    let Some(verb) = Verb::parse(&request.verb) else {
        return unavailable(job, id, "The request names an unavailable capability.");
    };
    let (capability, risk) = match approvals::canonical_capability(verb, request.scope.clone()) {
        Ok(capability) => capability,
        Err(_) => return unavailable(job, id, "The request capability is unavailable."),
    };
    match serde_json::to_vec(&capability.scope) {
        Ok(bytes) if bytes.len() <= MAX_SCOPE_BYTES => {}
        Ok(_) => return unavailable(
            job,
            id,
            "The capability scope exceeds the Activity summary limit; inspect it in OS reviews.",
        ),
        Err(_) => return unavailable(job, id, "The request scope cannot be rendered safely."),
    }
    Decision {
        id: id.to_string(),
        job_id: job.id.clone(),
        session_id: job.session_id.clone(),
        status,
        requested_at: Some(request.requested_at),
        verb: Some(capability.verb.as_str().to_string()),
        scope: Some(capability.scope),
        risk: Some(risk),
        reason: Some(excerpt(&request.reason, MAX_PREVIEW_CHARS)),
        review_id: Some(request.id),
        error: None,
    }
}

fn unavailable(job: &Job, id: &str, message: &str) -> Decision {
    Decision {
        id: id.to_string(),
        job_id: job.id.clone(),
        session_id: job.session_id.clone(),
        status: DecisionStatus::Unavailable,
        requested_at: None,
        verb: None,
        scope: None,
        risk: None,
        reason: None,
        review_id: None,
        error: Some(excerpt(message, MAX_PREVIEW_CHARS)),
    }
}

fn excerpt(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        crate::agent::llm::truncate_for_display(text, max_chars - 1)
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/activity_attention.rs"
    ));
}
