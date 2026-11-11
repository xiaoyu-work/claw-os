//! Store and isolated App UI adapters; all installation authority stays in clawd.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use clawd_client::system_review::{
    ReviewAction, ReviewDecision, ReviewKind, ReviewStatus, SystemReview,
};
use serde::Deserialize;
use serde_json::{json, Value};

use super::clawd::{self, ApiError};
use crate::agent::web::auth::AuthenticatedToken;
use crate::agent::web::state::AppState;
use crate::clawd::routes::Command;
use crate::provenance::runtime::PackageRef;
use crate::provenance::{PackageKind, VerifyOptions};

fn failure(message: impl Into<String>) -> ApiError {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
}

fn project_review(mut value: Value) -> Result<Value, ApiError> {
    if let Some(review) = value.get_mut("review") {
        if review.get("kind").and_then(Value::as_str) == Some("package_impact") {
            return Err(failure(
                "Package-impact confirmation belongs in OS Approvals, not App installation",
            ));
        }
        let revision = review
            .get("revision")
            .and_then(Value::as_u64)
            .ok_or_else(|| failure("The OS App review has an invalid displayed revision"))?;
        review["revision"] = json!(revision.to_string());
    }
    Ok(value)
}

pub async fn catalog() -> Result<Json<Value>, ApiError> {
    clawd::request(Command::AppStoreList, json!({}))
        .await
        .map(Json)
        .map_err(|error| error.into_api_error())
}

pub async fn installed() -> Result<Json<Value>, ApiError> {
    clawd::request(Command::AppStoreInstalled, json!({}))
        .await
        .map(Json)
        .map_err(|error| error.into_api_error())
}

pub async fn requests() -> Result<Json<Value>, ApiError> {
    clawd::request(Command::AppStoreRequests, json!({}))
        .await
        .map(Json)
        .map_err(|error| error.into_api_error())
}

pub async fn review(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    let value = clawd::request(Command::SystemReviewShow, json!({"id": id}))
        .await
        .map_err(|error| error.into_api_error())?;
    project_review(json!({"review": value})).map(Json)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallBody {
    sha256: String,
}

pub async fn prepare(
    Path(id): Path<String>,
    Json(body): Json<InstallBody>,
) -> Result<Json<Value>, ApiError> {
    clawd::request(
        Command::AppStorePrepare,
        json!({"app_id": id, "sha256": body.sha256}),
    )
    .await
    .map_err(|error| error.into_api_error())
    .and_then(project_review)
    .map(Json)
}

pub async fn commit(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    clawd::request(Command::AppStoreCommit, json!({"request_id": id}))
        .await
        .map(Json)
        .map_err(|error| error.into_api_error())
}

pub async fn cancel(Path(id): Path<String>) -> Result<Json<Value>, ApiError> {
    clawd::request(Command::AppStoreCancel, json!({"request_id": id}))
        .await
        .map(Json)
        .map_err(|error| error.into_api_error())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmBody {
    revision: String,
}

fn app_confirmation_action(kind: ReviewKind) -> Result<ReviewAction, ApiError> {
    match kind {
        ReviewKind::Install => Ok(ReviewAction::ConfirmInstall),
        ReviewKind::Activation => Ok(ReviewAction::ConfirmActivation),
        ReviewKind::Update => Ok(ReviewAction::ConfirmUpdate),
        ReviewKind::PackageImpact => Err(failure(
            "Use top-level OS Approvals for the separate all-App stop confirmation",
        )),
        ReviewKind::Capability => Err(failure("App confirmation cannot grant a capability")),
    }
}

pub async fn authorize(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ConfirmBody>,
) -> Result<Json<Value>, ApiError> {
    let origin = super::approvals::authorization::origin(&headers, &state)?;
    let value = clawd::request(Command::SystemReviewShow, json!({"id": id}))
        .await
        .map_err(|error| error.into_api_error())?;
    let review: SystemReview = serde_json::from_value(value)
        .map_err(|_| failure("The OS returned an invalid App review"))?;
    review
        .validate()
        .map_err(|error| failure(error.to_string()))?;
    if review.status != ReviewStatus::Pending || review.revision.to_string() != body.revision {
        return Err(failure(
            "The displayed App review changed; refresh before confirming",
        ));
    }
    let action = app_confirmation_action(review.kind)?;
    let decision = ReviewDecision {
        id: review.id.clone(),
        revision: review.revision,
        action,
        choices: Vec::new(),
    };
    let snapshot = state
        .inner
        .authorizations
        .start(&caller, origin, review, decision)
        .await
        .map_err(failure)?;
    serde_json::to_value(snapshot)
        .map(Json)
        .map_err(|error| failure(error.to_string()))
}

pub async fn open(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let value = clawd::request(Command::AppStoreOpen, json!({"app_id": id}))
        .await
        .map_err(|error| error.into_api_error())?;
    if value.get("status").and_then(Value::as_str) == Some("review_required") {
        return project_review(value).map(Json);
    }
    if value.get("status").and_then(Value::as_str) != Some("ready") {
        return Err(failure("The OS did not admit this App's Web interface"));
    }
    let directory = value
        .get("package_dir")
        .and_then(Value::as_str)
        .ok_or_else(|| failure("The OS returned no verified App directory"))?;
    let expected: PackageRef = serde_json::from_value(value["package"].clone())
        .map_err(|_| failure("The OS returned an invalid App package binding"))?;
    let package = crate::provenance::verify::verify_package(
        &PathBuf::from(directory),
        &VerifyOptions::new(PackageKind::App).expect_id(&id),
        &crate::provenance::trust_store(),
    )
    .map_err(|error| failure(error.to_string()))?;
    let current = PackageRef::of(&package);
    if current.id != expected.id
        || current.content_digest != expected.content_digest
        || current.publisher_key_id != expected.publisher_key_id
        || current.tier != expected.tier
    {
        return Err(failure(
            "The App package changed while opening its Web interface",
        ));
    }
    let app = crate::apps::store::web_metadata(&package).map_err(failure)?;
    let view_id = state
        .inner
        .web_apps
        .create(&caller, Arc::new(package), expected, app.clone())
        .map_err(failure)?;
    Ok(Json(json!({
        "status": "ready", "view_id": view_id, "nonce": view_id,
        "src": format!("/app-ui/{view_id}/index.html"), "app": app,
    })))
}

pub async fn close(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    state.inner.web_apps.close(&id, &caller).map_err(failure)?;
    Ok(Json(json!({"closed": true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallBody {
    view_id: String,
    tool: String,
    arguments: Value,
}

pub async fn call(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    Json(body): Json<CallBody>,
) -> Result<Json<Value>, ApiError> {
    let view = state
        .inner
        .web_apps
        .owned(&body.view_id, &caller)
        .map_err(failure)?;
    if view.package.id() != id
        || !body.arguments.is_object()
        || !view.app["tools"].as_array().is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.get("name").and_then(Value::as_str) == Some(body.tool.as_str()))
        })
    {
        return Err(failure(
            "Web App messages may call only the owning App's declared tools",
        ));
    }
    let result = clawd::request(
        Command::AppServiceCliCall,
        json!({
            "app_id": id, "tool": body.tool, "arguments": body.arguments,
            "expected_package": view.expected,
        }),
    )
    .await
    .map_err(|error| error.into_api_error())?;
    Ok(Json(json!({"ok": true, "result": result})))
}

const APP_CSP: &str = concat!(
    "default-src 'none'; base-uri 'none'; connect-src 'none'; ",
    "script-src 'self'; script-src-attr 'none'; style-src 'self' 'unsafe-inline'; ",
    "img-src 'self' data:; font-src 'self'; object-src 'none'; worker-src 'none'; ",
    "form-action 'none'; frame-src 'none'; frame-ancestors 'self'"
);

pub async fn resource(
    State(state): State<AppState>,
    Path((id, file)): Path<(String, String)>,
) -> Response {
    let result = (|| {
        let view = state.inner.web_apps.resource(&id)?;
        let relative = format!("web/{file}");
        crate::provenance::envelope::validate_tree_path(&relative)?;
        let bytes = view
            .package
            .read_verified(&relative)
            .map_err(|error| error.to_string())?;
        if bytes.len() > crate::apps::store::MAX_METADATA {
            return Err("Web App resource exceeds its limit".to_string());
        }
        let mime = match std::path::Path::new(&file)
            .extension()
            .and_then(|value| value.to_str())
        {
            Some("html") => "text/html; charset=utf-8",
            Some("js") => "text/javascript; charset=utf-8",
            Some("css") => "text/css; charset=utf-8",
            Some("json") => "application/json",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("webp") => "image/webp",
            Some("woff2") => "font/woff2",
            Some("txt") => "text/plain; charset=utf-8",
            _ => return Err("Unsupported Web App resource".to_string()),
        };
        Ok((bytes, mime))
    })();
    match result {
        Ok((bytes, mime)) => (
            [
                (header::CONTENT_TYPE, mime),
                (header::CACHE_CONTROL, "no-store"),
                (header::CONTENT_SECURITY_POLICY, APP_CSP),
                (header::REFERRER_POLICY, "no-referrer"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "Web App resource is unavailable").into_response(),
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/apps.rs"
    ));
}
