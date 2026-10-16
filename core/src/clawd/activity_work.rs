//! Owner controls over shared work state. Enabling reuses scheduler consent.

use super::activities::{decode, encode, owner, service_error};
use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;
use super::wire::requests as body;
use crate::activities::{self, ActivityService, ActivityWorkChange};
use serde_json::{json, Value};

pub fn get(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityWorkGet = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    let activity = service
        .get(owner, request.id.as_str())
        .map_err(service_error)?;
    let work = service.work(owner, &activity.id).map_err(service_error)?;
    let history = service
        .work_history(owner, &activity.id, 20)
        .map_err(service_error)?;
    Ok(json!({"schema":1,"activity_id":activity.id,"work":work,"history":history}))
}

pub fn set(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityWorkSet = decode(params)?;
    request.work.validate().map_err(service_error)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    encode(
        activities::open_default()
            .map_err(service_error)?
            .change_work(
                owner,
                request.id.as_str(),
                request.expected_revision,
                ActivityWorkChange::Configure(request.work),
            )
            .map_err(service_error)?,
    )
}

pub async fn enabled(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let owner = owner(client)?;
    let request: body::ActivityWorkEnabled = decode(params)?;
    activities::validate_id(request.id.as_str()).map_err(service_error)?;
    let service = activities::open_default().map_err(service_error)?;
    let work = service
        .work(owner, request.id.as_str())
        .map_err(service_error)?
        .ok_or_else(|| BrokerError::execution("configure Activity work first"))?;
    if work.revision != request.expected_revision {
        return Err(service_error(activities::ActivityError::Conflict(
            "Activity work changed; refresh before changing it".into(),
        )));
    }
    if !request.enabled {
        if request.next_task.is_some() || request.review_note.is_some() {
            return Err(BrokerError::execution(
                "stopping work does not accept a next task or review note",
            ));
        }
        let stopped = service
            .change_work(
                owner,
                &work.activity_id,
                Some(work.revision),
                ActivityWorkChange::Disable,
            )
            .map_err(service_error)?;
        if crate::triggers::work::binding(owner, &work.activity_id)?.is_some() {
            super::scheduler::run(json!({
                "subsystem":"triggers","command":"disable",
                "args":[crate::triggers::work::rule_id(&work.activity_id)]
            }),client).await.map_err(|error| BrokerError::execution(format!(
                "Automatic progression is durably stopped, but scheduler binding cleanup failed: {error}"
            )))?;
        }
        super::heartbeat::wake_activity_work();
        return encode(stopped);
    }
    let (source, event_type) = match &work.config.wake_event {
        Some(event) => (event.source.as_str(), event.event_type.as_str()),
        None => ("activity-work-disabled-events", "none"),
    };
    let mut args = vec![
        "--id".to_string(),
        crate::triggers::work::rule_id(&work.activity_id),
        "--activity".into(),
        work.activity_id.clone(),
        "--activity-work".into(),
        "true".into(),
        "--work-revision".into(),
        work.revision.to_string(),
        "--prompt".into(),
        work.config.task.clone(),
        "--source".into(),
        source.into(),
        "--event-type".into(),
        event_type.into(),
    ];
    if let Some(task) = request.next_task {
        activities::validate_work_task(task.as_str()).map_err(service_error)?;
        args.extend(["--work-next-task".into(), task.as_str().to_string()]);
    }
    if let Some(note) = request.review_note {
        args.extend(["--work-review-note".into(), note.as_str().to_string()]);
    }
    let result = super::scheduler::run(
        json!({
            "subsystem":"triggers","command":"add","args":args,
        }),
        client,
    )
    .await?;
    let state = result.get("work").cloned().ok_or_else(|| {
        BrokerError::unavailable("scheduler returned no Activity work acknowledgement")
    })?;
    let confirmed: activities::ActivityWork = decode(state)?;
    if confirmed.activity_id != work.activity_id
        || confirmed.owner_uid != owner
        || !confirmed.enabled
        || confirmed.revision != work.revision + 1
    {
        return Err(BrokerError::unavailable(
            "scheduler Activity work acknowledgement differs",
        ));
    }
    super::heartbeat::wake_activity_work();
    encode(confirmed)
}
