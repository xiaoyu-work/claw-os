//! Explicit projection of the shared Activity and task services. No local
//! Activity state is inferred from job status or approval outcomes.

use cos_agent_protocol::{
    ActivityApprovalView, ActivityDetailResponse, ActivityJobView, ActivityListResponse,
    ActivityObjectResourceView, ActivityObjectStatus, ActivityObjectsResponse,
    ActivityOperationPreview, ActivityResource, ActivityState, ActivityView, ActivityWorkResponse,
};
use serde::Deserialize;
use serde_json::Value;

const PREVIEW_CHARS: usize = 4096;

#[derive(Deserialize)]
struct CoreResource {
    label: String,
    reference: String,
}

#[derive(Deserialize)]
struct CoreActivity {
    id: String,
    title: String,
    goal: String,
    completion_criteria: String,
    boundaries: String,
    resources: Vec<CoreResource>,
    state: ActivityState,
    completion_note: Option<String>,
    created_at: String,
    updated_at: String,
}

impl CoreActivity {
    fn into_view(self) -> Result<ActivityView, String> {
        if self.id.trim().is_empty() {
            return Err("clawd returned an empty Activity id".into());
        }
        Ok(ActivityView {
            id: self.id,
            title: self.title,
            goal: self.goal,
            state: self.state,
            completion_criteria: self.completion_criteria,
            boundaries: self.boundaries,
            resources: self
                .resources
                .into_iter()
                .map(|resource| ActivityResource {
                    label: resource.label,
                    reference: resource.reference,
                })
                .collect(),
            completion_note: self.completion_note,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[derive(Deserialize)]
struct CoreActivityJob {
    id: String,
    title: String,
    status: String,
    session_id: Option<String>,
    created_at: String,
    finished_at: Option<String>,
    response: Option<String>,
    error: Option<String>,
    #[serde(default)]
    waiting_on: Vec<String>,
}

fn preview(value: Option<String>) -> Option<String> {
    value.map(|text| {
        let mut chars = text.chars();
        let mut bounded: String = chars.by_ref().take(PREVIEW_CHARS).collect();
        if chars.next().is_some() {
            bounded.push('…');
        }
        bounded
    })
}

pub fn activity(value: Value) -> Result<ActivityView, String> {
    serde_json::from_value::<CoreActivity>(value)
        .map_err(|error| format!("invalid Activity result: {error}"))?
        .into_view()
}

pub fn objects(value: Value) -> Result<ActivityObjectsResponse, String> {
    #[derive(Deserialize)]
    struct Envelope {
        schema: u32,
        activity_id: String,
        objects: Vec<ActivityObjectResourceView>,
    }
    let envelope: Envelope = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.objects result: {error}"))?;
    check_schema(envelope.schema)?;
    if envelope.activity_id.trim().is_empty() {
        return Err("clawd returned an empty Activity id for object descriptions".into());
    }
    for object in &envelope.objects {
        // Resource text may be noncanonical. Only the broker interprets it
        // and supplies the catalogue's canonical description.reference.
        match (object.status, &object.description) {
            (ActivityObjectStatus::Declared, Some(description))
                if description.object.app_id == description.invocation.app_id => {}
            (ActivityObjectStatus::Unavailable | ActivityObjectStatus::Invalid, None) => {}
            _ => return Err("inconsistent activity.objects declaration or App identity".into()),
        }
    }
    Ok(ActivityObjectsResponse {
        activity_id: envelope.activity_id,
        objects: envelope.objects,
    })
}

pub fn operation_preview(value: Value) -> Result<ActivityOperationPreview, String> {
    let preview: ActivityOperationPreview = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.operation.preview result: {error}"))?;
    if !preview.is_metadata_only() {
        return Err("activity.operation.preview did not return schema-1 metadata-only data".into());
    }
    Ok(preview)
}

pub fn list(value: Value) -> Result<ActivityListResponse, String> {
    #[derive(Deserialize)]
    struct Envelope {
        schema: u32,
        activities: Vec<CoreActivity>,
    }
    let envelope: Envelope = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.list result: {error}"))?;
    check_schema(envelope.schema)?;
    Ok(ActivityListResponse {
        activities: envelope
            .activities
            .into_iter()
            .map(CoreActivity::into_view)
            .collect::<Result<_, _>>()?,
    })
}

pub fn detail(value: Value) -> Result<ActivityDetailResponse, String> {
    #[derive(Deserialize)]
    struct Envelope {
        schema: u32,
        activity: CoreActivity,
        jobs: Vec<CoreActivityJob>,
        sessions: Vec<String>,
    }
    let envelope: Envelope = serde_json::from_value(value)
        .map_err(|error| format!("invalid activity.get result: {error}"))?;
    check_schema(envelope.schema)?;
    let jobs = envelope
        .jobs
        .into_iter()
        .map(|job| {
            if job.id.trim().is_empty() || job.status.trim().is_empty() {
                return Err("clawd returned an invalid Activity job".to_string());
            }
            Ok(ActivityJobView {
                id: job.id,
                title: job.title,
                status: job.status,
                session_id: job.session_id,
                created_at: job.created_at,
                finished_at: job.finished_at,
                response: preview(job.response),
                error: preview(job.error),
                waiting_on: job.waiting_on,
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(ActivityDetailResponse {
        activity: envelope.activity.into_view()?,
        jobs,
        sessions: envelope.sessions,
        pending_approvals: Vec::new(),
        approvals_error: None,
    })
}

pub fn work(value: Value) -> Result<ActivityWorkResponse, String> {
    #[derive(Deserialize)]
    struct Submission {
        id: String,
        status: String,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        activity_id: Option<String>,
    }
    let job: Submission = serde_json::from_value(value)
        .map_err(|error| format!("invalid Activity work result: {error}"))?;
    if job.id.trim().is_empty() || job.status.trim().is_empty() {
        return Err("clawd returned an invalid Activity work acknowledgement".into());
    }
    Ok(ActivityWorkResponse {
        id: job.id,
        status: job.status,
        session_id: job.session_id,
        activity_id: job.activity_id,
    })
}

pub fn approvals(
    value: Value,
    detail: &ActivityDetailResponse,
) -> Result<Vec<ActivityApprovalView>, String> {
    #[derive(Deserialize)]
    struct Meta {
        label: String,
    }
    #[derive(Deserialize)]
    struct Request {
        id: String,
        session: String,
        verb: String,
        reason: String,
        #[serde(default)]
        meta: Option<Meta>,
    }
    #[derive(Deserialize)]
    struct Envelope {
        requests: Vec<Request>,
    }
    let envelope: Envelope = serde_json::from_value(value)
        .map_err(|error| format!("invalid permission.pending result: {error}"))?;
    Ok(envelope
        .requests
        .into_iter()
        .filter(|request| {
            !request.session.is_empty()
                && (detail.sessions.contains(&request.session)
                    || detail
                        .jobs
                        .iter()
                        .any(|job| job.session_id.as_deref() == Some(request.session.as_str())))
        })
        .map(|request| ActivityApprovalView {
            id: request.id,
            session_id: request.session,
            label: request
                .meta
                .map(|meta| meta.label)
                .filter(|label| !label.trim().is_empty())
                .unwrap_or(request.verb),
            reason: request.reason,
        })
        .collect())
}

fn check_schema(schema: u32) -> Result<(), String> {
    if schema == 1 {
        Ok(())
    } else {
        Err(format!(
            "unsupported clawd Activity schema {schema}; expected 1"
        ))
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/translation/activities.rs"
    ));
}
