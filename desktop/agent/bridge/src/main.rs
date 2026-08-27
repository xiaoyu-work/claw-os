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
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tracing::info;
use tracing_subscriber::EnvFilter;

mod routes;
mod state;

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

    let api =
        routes::api().route_layer(middleware::from_fn_with_state(state.clone(), require_auth));
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
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
    next.run(request).await
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
