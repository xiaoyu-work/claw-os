use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::{Extension, Json};
use clawd_client::system_review::{PermissionChoice, ReviewStatus};
use serde::Deserialize;
use serde_json::{json, Value};
use zeroize::Zeroizing;

use super::presentation::{decision_for, show, DecisionBody};
use crate::agent::web::auth::AuthenticatedToken;
use crate::agent::web::authorization::Snapshot;
use crate::agent::web::state::AppState;

type Failure = (StatusCode, Json<Value>);
type Reply = ([(header::HeaderName, &'static str); 1], Json<Value>);

fn reply(snapshot: Snapshot) -> Result<Reply, Failure> {
    let value = serde_json::to_value(snapshot).map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"OS authorization state could not be presented."})),
        )
    })?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(value)))
}

fn failure(message: String) -> Failure {
    (StatusCode::CONFLICT, Json(json!({"error":message})))
}

pub(crate) fn origin(headers: &HeaderMap, state: &AppState) -> Result<String, Failure> {
    let value = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| failure("OS authorization requires a same-origin Web request.".into()))?;
    let parsed = url::Url::parse(value)
        .map_err(|_| failure("OS authorization origin is invalid.".into()))?;
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| failure("OS authorization request has no host.".into()))?;
    let expected = url::Url::parse(&format!("{}://{host}", parsed.scheme()))
        .map_err(|_| failure("OS authorization host is invalid.".into()))?;
    let loopback = match parsed.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.origin() != expected.origin()
        || parsed.scheme() == "http" && !loopback
        || !state.inner.local_only && parsed.scheme() != "https"
    {
        return Err(failure(
            "OS authorization requires the serving origin and HTTPS outside loopback.".into(),
        ));
    }
    Ok(parsed.origin().ascii_serialization())
}

pub async fn start_authorization(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DecisionBody>,
) -> Result<Reply, Failure> {
    let origin = origin(&headers, &state)?;
    if body.choice == PermissionChoice::Deny {
        return Err(failure("Denial does not require OS authentication.".into()));
    }
    let review = show(&id).await?;
    if review.status != ReviewStatus::Pending {
        return Err(failure(
            "This OS request is no longer pending. Refresh it first.".into(),
        ));
    }
    let decision = decision_for(&review, body)?;
    let snapshot = state
        .inner
        .authorizations
        .start(&caller, origin, review, decision)
        .await
        .map_err(failure)?;
    reply(snapshot)
}

pub async fn authorization_status(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
) -> Result<Reply, Failure> {
    let snapshot = state
        .inner
        .authorizations
        .status(&id, &caller)
        .map_err(failure)?;
    reply(snapshot)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseBody {
    prompt_id: String,
    response: Zeroizing<String>,
}

pub async fn authorization_response(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Result<Json<ResponseBody>, JsonRejection>,
) -> Result<Reply, Failure> {
    let Json(body) = body.map_err(|_| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({"error":"The OS authentication response has an invalid shape."})),
        )
    })?;
    let origin = origin(&headers, &state)?;
    let snapshot = state
        .inner
        .authorizations
        .respond(&id, &caller, &origin, &body.prompt_id, body.response)
        .map_err(failure)?;
    reply(snapshot)
}

pub async fn cancel_authorization(
    State(state): State<AppState>,
    Extension(caller): Extension<AuthenticatedToken>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Reply, Failure> {
    let origin = origin(&headers, &state)?;
    let snapshot = state
        .inner
        .authorizations
        .cancel(&id, &caller, &origin)
        .map_err(failure)?;
    reply(snapshot)
}
