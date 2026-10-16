//! Precision-safe presentation of the shared Activity work contract.

use super::*;
use crate::activities::{ActivityWork, ActivityWorkDraft, ActivityWorkRecord};
use crate::clawd::wire::requests::{ActivityWorkEnabled, ActivityWorkGet, ActivityWorkSet};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkSet {
    #[serde(deserialize_with = "required_nullable")]
    expected_revision: Option<String>,
    work: ActivityWorkDraft,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkEnabled {
    expected_revision: String,
    enabled: bool,
    next_task: Option<String>,
    review_note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkView {
    schema: u32,
    activity_id: String,
    #[serde(deserialize_with = "required_nullable")]
    work: Option<ActivityWork>,
    history: Vec<ActivityWorkRecord>,
}

fn project(work: ActivityWork, id: &str, owner: u32) -> Result<Value, ApiError> {
    work.validate()
        .map_err(|error| bad_gateway(format!("invalid work state: {error}")))?;
    if !same_activity(&work.activity_id, id) || work.owner_uid != owner {
        return Err(bad_gateway("Activity work owner or identity differs"));
    }
    let mut value = serde_json::to_value(&work).map_err(|error| bad_gateway(error.to_string()))?;
    value["revision"] = json!(work.revision.to_string());
    value["event_after"] = json!(work.event_after.to_string());
    Ok(value)
}

pub async fn get(
    Extension(auth): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    query: Result<Query<NoBody>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    query.map_err(|error| bad_request(error.body_text()))?;
    let Json(value) = request(
        Command::ActivityWorkGet,
        with_id::<ActivityWorkGet>(id.clone(), json!({}))?,
    )
    .await?;
    let view: WorkView = serde_json::from_value(value)
        .map_err(|error| bad_gateway(format!("invalid work view: {error}")))?;
    if view.schema != 1 || !same_activity(&view.activity_id, &id) || view.history.len() > 20 {
        return Err(bad_gateway(
            "Activity work view identity or history bound differs",
        ));
    }
    for record in &view.history {
        record
            .validate()
            .map_err(|error| bad_gateway(error.to_string()))?;
    }
    let work = view
        .work
        .map(|work| project(work, &id, auth.uid))
        .transpose()?;
    Ok(Json(
        json!({"schema":1,"activity_id":view.activity_id,"work":work,"history":view.history}),
    ))
}

pub async fn set(
    Extension(auth): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    body: Result<Json<WorkSet>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let body = json_body(body)?;
    body.work
        .validate()
        .map_err(|error| bad_request(error.to_string()))?;
    let revision = body
        .expected_revision
        .as_deref()
        .map(|value| decimal_u64("expected_revision", value, i64::MAX as u64))
        .transpose()?;
    let Json(value) = request(
        Command::ActivityWorkSet,
        with_id::<ActivityWorkSet>(
            id.clone(),
            json!({
                "expected_revision":revision,"work":body.work,
            }),
        )?,
    )
    .await?;
    let work: ActivityWork =
        serde_json::from_value(value).map_err(|error| bad_gateway(error.to_string()))?;
    if work.revision != revision.unwrap_or(0) + 1 || work.enabled || work.config != body.work {
        return Err(bad_gateway(
            "Activity work configuration acknowledgement differs",
        ));
    }
    project(work, &id, auth.uid).map(Json)
}

pub async fn enabled(
    Extension(auth): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    body: Result<Json<WorkEnabled>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let body = json_body(body)?;
    let revision = decimal_u64(
        "expected_revision",
        &body.expected_revision,
        i64::MAX as u64 - 1,
    )?;
    let Json(value) = request(
        Command::ActivityWorkEnabled,
        with_id::<ActivityWorkEnabled>(
            id.clone(),
            json!({
                "expected_revision":revision,"enabled":body.enabled,
                "next_task":body.next_task,"review_note":body.review_note,
            }),
        )?,
    )
    .await?;
    let work: ActivityWork =
        serde_json::from_value(value).map_err(|error| bad_gateway(error.to_string()))?;
    if work.revision != revision + 1 || work.enabled != body.enabled {
        return Err(bad_gateway(
            "Activity work enablement acknowledgement differs",
        ));
    }
    project(work, &id, auth.uid).map(Json)
}
