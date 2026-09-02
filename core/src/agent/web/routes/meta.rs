//! `GET /api/meta` — provider / model / version / token-status.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::web::state::AppState;

pub async fn handler(State(state): State<AppState>) -> Json<Value> {
    // Re-read the agent config every time so a Copilot sign-in / model
    // switch performed against a running daemon is reflected here on
    // the very next request — see the matching note in `chat.rs` for
    // why the startup snapshot in `state.inner.cfg` is not enough.
    let fresh = crate::config::load_user_config().agent.clone();
    let cfg = if fresh.provider.is_empty() && !state.inner.cfg.provider.is_empty() {
        state.inner.cfg.clone()
    } else {
        fresh
    };
    let version = std::env::var("COS_VERSION").unwrap_or_else(|_| "dev".into());
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "localhost".into());

    Json(json!({
        "provider": cfg.provider,
        "model": cfg.model,
        "version": version,
        "hostname": hostname,
        "owner_uid": state.inner.owner_uid,
        "started_at": state.inner.started_at_unix,
        "ui": {
            "title": format!("claw-os agent — {hostname}"),
            "subtitle": format!("{} · {}", cfg.provider, cfg.model),
        },
    }))
}
