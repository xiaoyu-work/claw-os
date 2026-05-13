//! `GET /api/models` — list configured providers + models.
//!
//! Stub: returns a fixed list. Will subprocess
//! `cos agent setup --status` and the provider registry once wired.

use axum::{Json, extract::State};
use serde::Serialize;

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct Model {
    pub id: String,
    pub provider: String,
    pub label: String,
}

pub async fn list(State(_state): State<AppState>) -> Json<Vec<Model>> {
    Json(vec![Model {
        id: "claude-sonnet-4.5".into(),
        provider: "anthropic".into(),
        label: "Claude Sonnet 4.5".into(),
    }])
}
