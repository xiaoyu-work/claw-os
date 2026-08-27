//! `cos-agent-bridge` — local HTTP+SSE daemon connecting the ClawOS
//! Agent UI (the desktop App at `com.clawos.Agent`) to `clawd`.
//!
//! The bridge is intentionally thin: it owns no LLM state, holds no
//! credentials, and persists nothing of its own. Every chat turn is
//! submitted to the user-session daemon and re-framed as Server-Sent
//! Events for the native UI.
//!
//! Bound to `127.0.0.1` only. Every route also requires a random bearer
//! token published inside the owning user's private runtime directory,
//! because the loopback interface is shared by all local users.
//!
//! See `desktop/agent/README.md` for the full architecture.

use std::net::SocketAddr;

use anyhow::Context;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use cos_agent_protocol::{
    CURRENT_PROTOCOL_VERSION, CURRENT_PROTOCOL_VERSION_HEADER_VALUE, ErrorCode, ErrorEnvelope,
    PROTOCOL_VERSION_HEADER, ProtocolVersion,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod api_error;
mod routes;
mod state;
mod translation;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("cos_agent_bridge=info")),
        )
        .init();

    let state = state::AppState::from_env()?;
    let port = state.port;

    let api = routes::api()
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .route_layer(middleware::from_fn(require_protocol_version));
    let app: Router = Router::new().nest("/api", api).with_state(state.clone());

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let bound = listener.local_addr()?;
    info!(%bound, "cos-agent-bridge listening");

    state::publish_endpoint(bound.port(), &state.auth_token)?;

    axum::serve(listener, app).await?;
    Ok(())
}

async fn require_auth(
    State(state): State<state::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let supplied = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !supplied
        .map(|token| constant_time_eq(token.as_bytes(), state.auth_token.as_bytes()))
        .unwrap_or(false)
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorEnvelope::new(ErrorCode::Unauthorized, "Unauthorized")),
        )
            .into_response();
    }
    next.run(request).await
}

async fn require_protocol_version(request: Request, next: Next) -> Response {
    let supplied = request
        .headers()
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u16>().ok())
        .map(ProtocolVersion);
    let Some(version) = supplied else {
        return protocol_error(
            ErrorCode::ProtocolVersionRequired,
            "desktop Agent protocol version header is required",
        );
    };
    if !version.is_supported() {
        return protocol_error(
            ErrorCode::IncompatibleProtocolVersion,
            format!(
                "desktop Agent protocol version {} is incompatible; supported version is {}",
                version.0, CURRENT_PROTOCOL_VERSION
            ),
        );
    }

    let mut response = next.run(request).await;
    response.headers_mut().insert(
        PROTOCOL_VERSION_HEADER,
        HeaderValue::from_static(CURRENT_PROTOCOL_VERSION_HEADER_VALUE),
    );
    response
}

fn protocol_error(code: ErrorCode, message: impl Into<String>) -> Response {
    let mut response = (
        StatusCode::UPGRADE_REQUIRED,
        Json(ErrorEnvelope::new(code, message)),
    )
        .into_response();
    response.headers_mut().insert(
        PROTOCOL_VERSION_HEADER,
        HeaderValue::from_static(CURRENT_PROTOCOL_VERSION_HEADER_VALUE),
    );
    response
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/main.rs"));
}
