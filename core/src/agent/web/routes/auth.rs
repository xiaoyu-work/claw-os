use axum::extract::{Json as JsonExtract, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::agent::web::auth;
use crate::agent::web::state::AppState;

#[derive(Debug, Default, Deserialize)]
pub struct TokenRequest {
    #[serde(default)]
    pub ttl_seconds: Option<u64>,
}

pub async fn token(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Option<JsonExtract<TokenRequest>>,
) -> Response {
    let bootstrap = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bootstrap "))
        .unwrap_or_default();
    if bootstrap.is_empty() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "missing bootstrap authorization",
                "hint": "set Authorization: Bootstrap <serve.token>",
            })),
        )
            .into_response();
    }
    let ttl = body.and_then(|JsonExtract(body)| body.ttl_seconds);
    match auth::exchange_bootstrap_token(bootstrap, state.inner.owner_uid, ttl) {
        Ok(issued) => (
            StatusCode::OK,
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::PRAGMA, "no-cache"),
            ],
            Json(json!(issued)),
        )
            .into_response(),
        Err(auth::TokenExchangeError::InvalidBootstrap) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid bootstrap token"})),
        )
            .into_response(),
        Err(auth::TokenExchangeError::Internal(error)) => {
            tracing::error!(%error, "failed to issue web access token");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "token issuance failed"})),
            )
                .into_response()
        }
    }
}
