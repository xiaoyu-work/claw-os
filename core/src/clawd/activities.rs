//! Owner-scoped Activity operations shared by terminal, Web, and desktop clients.

use std::collections::BTreeSet;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::activities::{
    self, ActivityDraft, ActivityError, ActivityPatch, ActivityService, DEFAULT_LIST_LIMIT,
    MAX_LIST_LIMIT,
};
use crate::agent::service::{Job, Store};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;

// Activity presentation versions do not follow SQLite migration versions.
pub const WIRE_SCHEMA_VERSION: u32 = 1;

pub fn create(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let draft: ActivityDraft = decode(params)?;
    draft.validate().map_err(service_error)?;
    let activity = activities::open_default()
        .map_err(service_error)?
        .create(owner, draft)
        .map_err(service_error)?;
    encode(activity)
}

pub fn list(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityList = decode(params)?;
    let limit = list_limit(request.limit)?;
    let records = activities::open_default()
        .map_err(service_error)?
        .list(owner, request.state, limit)
        .map_err(service_error)?;
    Ok(json!({
        "schema": WIRE_SCHEMA_VERSION,
        "activities": records,
    }))
}

pub fn get(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityGet = decode(params)?;
    let limit = list_limit(request.limit)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let activity = activities::open_default()
        .map_err(service_error)?
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    // Resolve ownership before opening or inspecting the task store.
    let jobs = Store::open_default()
        .and_then(|store| store.list_for_activity(owner, &activity.id, limit))
        .map_err(|error| BrokerError::unavailable(format!("read Activity jobs: {error}")))?;
    let sessions: BTreeSet<_> = jobs
        .iter()
        .filter_map(|job| job.session_id.as_deref())
        .collect();
    let views: Vec<_> = jobs.iter().map(job_view).collect();
    Ok(json!({
        "schema": WIRE_SCHEMA_VERSION,
        "activity": activity,
        "jobs": views,
        "sessions": sessions,
        "job_limit": limit,
    }))
}

pub fn update(mut params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityUpdate = decode(params.clone())?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let fields = params
        .as_object_mut()
        .ok_or_else(|| BrokerError::execution("Activity update must be an object"))?;
    fields.remove("id");
    let patch: ActivityPatch = decode(params)?;
    patch.validate().map_err(service_error)?;
    let activity = activities::open_default()
        .map_err(service_error)?
        .update(owner, request.id.as_str(), patch)
        .map_err(service_error)?;
    encode(activity)
}

pub fn transition(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityTransition = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let activity = activities::open_default()
        .map_err(service_error)?
        .transition(
            owner,
            request.id.as_str(),
            request.state,
            request
                .completion_note
                .map(|note| note.as_str().to_string()),
        )
        .map_err(service_error)?;
    encode(activity)
}

pub async fn run(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityRun = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let activity = activities::open_default()
        .map_err(service_error)?
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    if !activity.state.allows_work() {
        return Err(BrokerError::execution(
            "Activity is not active; explicitly resume it before submitting work",
        )
        .classified("activity_not_active"));
    }
    let prompt = match request.prompt {
        Some(prompt) if prompt.as_str().trim().is_empty() => {
            return Err(BrokerError::execution(
                "Activity task prompt must not be empty",
            ));
        }
        Some(prompt) => prompt.as_str().to_string(),
        None => activity.goal,
    };
    let mut task = json!({
        "activity_id": activity.id,
        "prompt": prompt,
    });
    if let Some(session_id) = request.session_id {
        task["session_id"] = json!(session_id.as_str());
    }
    if let Some(max_turns) = request.max_turns {
        task["max_turns"] = json!(max_turns);
    }
    if let Some(use_memory) = request.use_memory {
        task["use_memory"] = json!(use_memory);
    }
    // Submission owns capability/session derivation and rechecks Activity
    // admission; an Activity never supplies authority of its own.
    super::tasks::submit(task, client)
        .await
        .map_err(BrokerError::from)
}

pub(super) fn owner(client: &ClientIdentity) -> Result<u32, BrokerError> {
    client.require_uid().map_err(BrokerError::authorization)
}

fn list_limit(limit: Option<u64>) -> Result<usize, BrokerError> {
    match limit {
        None => Ok(DEFAULT_LIST_LIMIT),
        Some(limit) if (1..=MAX_LIST_LIMIT as u64).contains(&limit) => Ok(limit as usize),
        Some(_) => Err(BrokerError::execution(format!(
            "Activity limit must be between 1 and {MAX_LIST_LIMIT}"
        ))),
    }
}

pub(super) fn decode<T: DeserializeOwned>(params: Value) -> Result<T, BrokerError> {
    serde_json::from_value(params)
        .map_err(|error| BrokerError::execution(format!("invalid Activity request: {error}")))
}

pub(super) fn encode(value: impl serde::Serialize) -> Result<Value, BrokerError> {
    serde_json::to_value(value)
        .map_err(|error| BrokerError::unavailable(format!("encode Activity response: {error}")))
}

pub(super) fn service_error(error: ActivityError) -> BrokerError {
    let message = error.to_string();
    match error {
        ActivityError::Invalid(_) => BrokerError::execution(message).classified("activity_invalid"),
        ActivityError::NotFound => BrokerError::execution(message).classified("activity_not_found"),
        ActivityError::Conflict(_) => {
            BrokerError::execution(message).classified("activity_conflict")
        }
        ActivityError::LimitReached => BrokerError::execution(message).classified("activity_limit"),
        ActivityError::ExecutionBlocked(_) => {
            BrokerError::execution(message).classified("activity_execution_blocked")
        }
        _ => BrokerError::unavailable(message).classified("activity_unavailable"),
    }
}

fn job_view(job: &Job) -> Value {
    json!({
        "id": job.id,
        "title": preview(&job.prompt, 160),
        "status": job.status.as_str(),
        "session_id": job.session_id,
        "created_at": job.created_at,
        "finished_at": job.finished_at,
        "response": job.response.as_deref().map(|text| preview(text, 4096)),
        "error": job.error.as_deref().map(|text| preview(text, 1024)),
        "waiting_on": job.waiting_on,
    })
}

fn preview(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut output: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        output.push_str("... [open the task for the full result]");
    }
    output
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/activities.rs"
    ));
}
