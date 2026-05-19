//! `GET /api/meta` — provider / model / version / token-status.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::web::state::AppState;

pub async fn handler(State(state): State<AppState>) -> Json<Value> {
    let cfg = &state.inner.cfg;
    let version = std::env::var("COS_VERSION").unwrap_or_else(|_| "dev".into());
    let hostname = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "localhost".into());

    Json(json!({
        "provider": cfg.provider,
        "model": cfg.model,
        "version": version,
        "hostname": hostname,
        "started_at": state.inner.started_at_unix,
        "ui": {
            "title": format!("claw-os agent — {hostname}"),
            "subtitle": format!("{} · {}", cfg.provider, cfg.model),
        },
    }))
}
