//! `cos-agent-bridge` — local HTTP+SSE daemon connecting the ClawOS
//! Agent UI (the desktop App at `com.clawos.Agent`) to `clawd`.
//!
//! The bridge is intentionally thin: it owns no LLM state, holds no
//! credentials, and persists nothing of its own. Every chat turn is
//! submitted to the user-session daemon and re-framed as Server-Sent
//! Events for the native UI.
//!
//! Bound to `127.0.0.1` only. There is no authentication — the
//! socket is single-user OS local and trusted by construction.
//!
//! See `desktop/agent/README.md` for the full architecture.

use std::net::SocketAddr;

use anyhow::Context;
use axum::Router;
use tracing::info;
use tracing_subscriber::EnvFilter;

mod clawd;
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

    let app: Router = Router::new().nest("/api", routes::api()).with_state(state);

    let addr: SocketAddr = format!("127.0.0.1:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    let bound = listener.local_addr()?;
    info!(%bound, "cos-agent-bridge listening");

    state::write_port_file(bound.port())?;

    axum::serve(listener, app).await?;
    Ok(())
}
