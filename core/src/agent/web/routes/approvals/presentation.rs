//! Separate capability and package-impact views over the protected OS review contract.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use clawd_client::system_review::{
    PendingReviews, PermissionChoice, PermissionSelection, ReviewAction, ReviewDecision,
    ReviewKind, ReviewStatus, SystemReview,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::{bad_request, internal, validate_decision_input};
use crate::agent::web::routes::clawd;
use crate::clawd::routes::Command;

type Failure = (StatusCode, Json<Value>);

#[derive(Debug, Serialize)]
struct ApprovalView {
    id: String,
    revision: String,
    status: ReviewStatus,
    label: String,
    description: String,
    target: String,
    reason: String,
    risk: clawd_client::system_review::ReviewRisk,
    choices: Vec<PermissionChoice>,
    verb: String,
    details: Vec<clawd_client::system_review::ReviewDetail>,
    diagnostic: Option<String>,
}

#[derive(Debug, Serialize)]
struct SystemApprovalView {
    id: String,
    revision: String,
    kind: ReviewKind,
    status: ReviewStatus,
    status_message: Option<String>,
    subject: clawd_client::system_review::ReviewSubject,
    initiator: String,
    context: Vec<clawd_client::system_review::ReviewDetail>,
    disclosures: Vec<clawd_client::system_review::ReviewDetail>,
    contract_digest: Option<String>,
    actions: Vec<ReviewAction>,
}

fn project_system(review: SystemReview) -> Result<SystemApprovalView, Failure> {
    review
        .validate()
        .map_err(|error| internal(error.to_string()))?;
    if review.kind != ReviewKind::PackageImpact || !review.permissions.is_empty() {
        return Err(bad_request(
            "This is not a separate package-impact OS review.".into(),
        ));
    }
    Ok(SystemApprovalView {
        id: review.id,
        revision: review.revision.to_string(),
        kind: review.kind,
        status: review.status,
        status_message: review.status_message,
        subject: review.subject,
        initiator: review.initiator,
        context: review.context,
        disclosures: review.disclosures,
        contract_digest: review.contract_digest,
        actions: review.actions,
    })
}

fn project(review: SystemReview) -> Result<ApprovalView, Failure> {
    review
        .validate()
        .map_err(|error| internal(error.to_string()))?;
    if review.kind != ReviewKind::Capability || review.permissions.len() != 1 {
        return Err(bad_request(
            "This request requires the OS App-review presenter.".into(),
        ));
    }
    let permission = &review.permissions[0];
    if permission.id != review.id {
        return Err(internal(
            "Capability review identity differs from its permission.".into(),
        ));
    }
    let diagnostic = review.status_message.or_else(|| {
        if review.status == ReviewStatus::Pending {
            permission.unsupported_reason.clone()
        } else {
            None
        }
    });
    Ok(ApprovalView {
        id: review.id,
        revision: review.revision.to_string(),
        status: review.status,
        label: permission.label.clone(),
        description: permission.blurb.clone(),
        target: permission.scope.description.clone(),
        reason: review
            .context
            .iter()
            .find(|detail| detail.label == "Requester-supplied reason")
            .map(|detail| detail.value.clone())
            .unwrap_or_default(),
        risk: permission.risk,
        choices: permission.supported_choices.clone(),
        verb: permission.verb.clone(),
        details: review.context,
        diagnostic,
    })
}

pub(super) async fn show(id: &str) -> Result<SystemReview, Failure> {
    validate_decision_input(id, None)?;
    let value = clawd::request(Command::SystemReviewShow, json!({"id":id}))
        .await
        .map_err(clawd::RpcError::into_api_error)?;
    let review: SystemReview = serde_json::from_value(value)
        .map_err(|error| internal(format!("Invalid OS review: {error}")))?;
    if review.id != id {
        return Err(internal("OS review identity mismatch.".into()));
    }
    review
        .validate()
        .map_err(|error| internal(error.to_string()))?;
    Ok(review)
}

pub async fn pending_reviews() -> Result<Json<Value>, Failure> {
    let value = clawd::request(Command::SystemReviewPending, json!({"limit":100}))
        .await
        .map_err(clawd::RpcError::into_api_error)?;
    let pending: PendingReviews = serde_json::from_value(value)
        .map_err(|error| internal(format!("Invalid OS review inventory: {error}")))?;
    let mut reviews = Vec::new();
    let mut app_reviews = Vec::new();
    let mut system_reviews = Vec::new();
    for review in pending.reviews {
        if review.kind == ReviewKind::Capability {
            reviews.push(project(review)?);
        } else if review.kind == ReviewKind::PackageImpact {
            system_reviews.push(project_system(review)?);
        } else {
            review
                .validate()
                .map_err(|error| internal(error.to_string()))?;
            app_reviews.push(json!({"id":review.id,"label":review.subject.name}));
        }
    }
    Ok(Json(
        json!({"reviews":reviews,"app_reviews":app_reviews,"system_reviews":system_reviews}),
    ))
}

pub async fn task_reviews(Path(id): Path<String>) -> Result<Json<Value>, Failure> {
    let task = clawd::request(Command::TaskGet, json!({"id":id}))
        .await
        .map_err(clawd::RpcError::into_api_error)?;
    if task.get("id").and_then(Value::as_str) != Some(id.as_str()) {
        return Err(internal(
            "Task approval response has a different task identity.".into(),
        ));
    }
    let ids = match task.get("waiting_on") {
        Some(Value::Array(ids)) => ids.clone(),
        None => Vec::new(),
        Some(_) => return Err(internal("Task approval references are malformed.".into())),
    };
    if ids.len() > 64 {
        return Err(internal("Task returned too many approval requests.".into()));
    }
    let mut reviews = Vec::new();
    let mut app_reviews = Vec::new();
    let mut system_reviews = Vec::new();
    for request_id in ids {
        let request_id = request_id
            .as_str()
            .ok_or_else(|| internal("Invalid task approval identity.".into()))?;
        let review = show(request_id).await?;
        if review.kind == ReviewKind::Capability {
            reviews.push(project(review)?);
        } else if review.kind == ReviewKind::PackageImpact {
            system_reviews.push(project_system(review)?);
        } else {
            app_reviews.push(json!({"id":review.id,"label":review.subject.name}));
        }
    }
    Ok(Json(
        json!({"task_id":id,"reviews":reviews,"app_reviews":app_reviews,"system_reviews":system_reviews}),
    ))
}

pub async fn recent_reviews() -> Result<Json<Value>, Failure> {
    let value = clawd::request(Command::PermissionRecent, json!({"limit":50}))
        .await
        .map_err(clawd::RpcError::into_api_error)?;
    let requests = value
        .get("requests")
        .and_then(Value::as_array)
        .ok_or_else(|| internal("Invalid recent approval inventory.".into()))?;
    let mut reviews = Vec::new();
    for request in requests {
        let id = request
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| internal("Recent approval omitted its identity.".into()))?;
        reviews.push(project(show(id).await?)?);
    }
    Ok(Json(
        json!({"reviews":reviews,"app_reviews":[],"system_reviews":[]}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemDecisionBody {
    pub revision: String,
}

pub(super) fn system_decision_for(
    review: &SystemReview,
    body: SystemDecisionBody,
    cancel: bool,
) -> Result<ReviewDecision, Failure> {
    if review.kind != ReviewKind::PackageImpact {
        return Err(bad_request(
            "App and capability reviews cannot approve a package-impact stop.".into(),
        ));
    }
    let decision = ReviewDecision {
        id: review.id.clone(),
        revision: body
            .revision
            .parse::<u64>()
            .map_err(|_| bad_request("Invalid review revision.".into()))?,
        action: if cancel {
            ReviewAction::Cancel
        } else {
            ReviewAction::ConfirmGlobalAppStop
        },
        choices: Vec::new(),
    };
    decision.validate_for(review).map_err(|error| {
        (
            StatusCode::CONFLICT,
            Json(json!({
                "error":error.to_string(), "code":"stale_review",
                "hint":"Refresh and inspect the current OS plan and impact before deciding."
            })),
        )
    })?;
    Ok(decision)
}

pub async fn cancel_system_review(
    State(state): State<crate::agent::web::state::AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SystemDecisionBody>,
) -> Result<Json<Value>, Failure> {
    super::authorization::origin(&headers, &state)?;
    let review = show(&id).await?;
    let decision = system_decision_for(&review, body, true)?;
    clawd::request(Command::SystemReviewCancel, json!({"review":decision}))
        .await
        .map_err(clawd::RpcError::into_api_error)?;
    let refreshed = show(&id).await?;
    if refreshed.status == ReviewStatus::Pending {
        return Err(internal(
            "OS cancellation is unconfirmed; refresh instead of resubmitting.".into(),
        ));
    }
    Ok(Json(
        serde_json::to_value(project_system(refreshed)?)
            .map_err(|error| internal(error.to_string()))?,
    ))
}

pub async fn system_review(Path(id): Path<String>) -> Result<Json<Value>, Failure> {
    Ok(Json(
        serde_json::to_value(project_system(show(&id).await?)?)
            .map_err(|error| internal(error.to_string()))?,
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionBody {
    pub revision: String,
    pub choice: PermissionChoice,
}

pub(super) fn decision_for(
    review: &SystemReview,
    body: DecisionBody,
) -> Result<ReviewDecision, Failure> {
    let revision = body
        .revision
        .parse::<u64>()
        .map_err(|_| bad_request("Invalid review revision.".into()))?;
    if review.kind != ReviewKind::Capability
        || review.permissions.len() != 1
        || review.permissions[0].id != review.id
        || !review.permissions[0]
            .supported_choices
            .contains(&body.choice)
    {
        return Err(bad_request(
            "The OS does not support this choice for this request.".into(),
        ));
    }
    let decision = ReviewDecision {
        id: review.id.clone(),
        revision,
        action: if body.choice == PermissionChoice::Deny {
            ReviewAction::Cancel
        } else {
            ReviewAction::ApplyChoices
        },
        choices: if body.choice == PermissionChoice::Deny {
            Vec::new()
        } else {
            vec![PermissionSelection {
                permission_id: review.permissions[0].id.clone(),
                choice: body.choice,
            }]
        },
    };
    decision.validate_for(review).map_err(|error| (StatusCode::CONFLICT, Json(json!({
        "error":error.to_string(), "code":"stale_review", "hint":"Refresh and review the request again before deciding."
    }))))?;
    Ok(decision)
}

pub async fn decide_review(
    Path(id): Path<String>,
    Json(body): Json<DecisionBody>,
) -> Result<Json<Value>, Failure> {
    let review = show(&id).await?;
    let decision = decision_for(&review, body)?;
    if decision.action == ReviewAction::Cancel {
        clawd::request(Command::SystemReviewCancel, json!({"review":decision}))
            .await
            .map_err(clawd::RpcError::into_api_error)?;
    } else {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "Use the Web OS authorization dialog. Refresh this page before authorizing.",
                "code": "authorization_dialog_required",
            })),
        ));
    }
    let refreshed = show(&id).await?;
    if refreshed.status == ReviewStatus::Pending {
        return Err(internal(
            "OS decision outcome is not confirmed. Refresh to inspect the request.".into(),
        ));
    }
    Ok(Json(
        serde_json::to_value(project(refreshed)?).map_err(|error| internal(error.to_string()))?,
    ))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/approvals/presentation.rs"
    ));
}
