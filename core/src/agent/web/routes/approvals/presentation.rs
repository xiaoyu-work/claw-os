//! Capability review presentation over the shared protected OS review contract.

use std::process::Stdio;

use axum::{extract::Path, http::StatusCode, Json};
use clawd_client::system_review::{
    PendingReviews, PermissionChoice, PermissionSelection, ReviewAction, ReviewDecision,
    ReviewKind, ReviewStatus, SystemReview, MAX_DECISION_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

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
        diagnostic: review
            .status_message
            .or_else(|| permission.unsupported_reason.clone()),
    })
}

async fn show(id: &str) -> Result<SystemReview, Failure> {
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
    for review in pending.reviews {
        if review.kind == ReviewKind::Capability {
            reviews.push(project(review)?);
        } else {
            review
                .validate()
                .map_err(|error| internal(error.to_string()))?;
            app_reviews.push(json!({"id":review.id,"label":review.subject.name}));
        }
    }
    Ok(Json(json!({"reviews":reviews,"app_reviews":app_reviews})))
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
    for request_id in ids {
        let request_id = request_id
            .as_str()
            .ok_or_else(|| internal("Invalid task approval identity.".into()))?;
        let review = show(request_id).await?;
        if review.kind == ReviewKind::Capability {
            reviews.push(project(review)?);
        } else {
            app_reviews.push(json!({"id":review.id,"label":review.subject.name}));
        }
    }
    Ok(Json(
        json!({"task_id":id,"reviews":reviews,"app_reviews":app_reviews}),
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
    Ok(Json(json!({"reviews":reviews,"app_reviews":[]})))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionBody {
    pub revision: String,
    pub choice: PermissionChoice,
}

fn decision_for(review: &SystemReview, body: DecisionBody) -> Result<ReviewDecision, Failure> {
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
        let encoded = serde_json::to_vec(&decision).map_err(|error| internal(error.to_string()))?;
        if encoded.len() > MAX_DECISION_BYTES {
            return Err(bad_request(
                "Review decision exceeds its byte limit.".into(),
            ));
        }
        let mut child = tokio::process::Command::new("/usr/bin/pkexec")
            .arg("/usr/local/bin/claw-approval-helper")
            .arg("--system-review-json")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| internal(format!("Launch OS approval helper: {error}")))?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| internal("OS helper has no decision input.".into()))?;
        input
            .write_all(&encoded)
            .await
            .map_err(|error| internal(format!("Send OS review decision: {error}")))?;
        drop(input);
        let output = child
            .wait_with_output()
            .await
            .map_err(|error| internal(format!("Wait for OS approval helper: {error}")))?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err((
                StatusCode::FORBIDDEN,
                Json(json!({
                    "error":if message.is_empty() { "OS authorization was cancelled or denied." } else { message.as_str() },
                    "hint":"Authorization requires the OS authentication prompt. On a headless system use cos review show for this request."
                })),
            ));
        }
        let returned: SystemReview = serde_json::from_slice(&output.stdout)
            .map_err(|error| internal(format!("Invalid OS decision acknowledgement: {error}")))?;
        returned
            .validate()
            .map_err(|error| internal(error.to_string()))?;
        if returned.id != id || returned.status == ReviewStatus::Pending {
            return Err(internal(
                "OS decision was not acknowledged. Refresh to inspect its outcome.".into(),
            ));
        }
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
