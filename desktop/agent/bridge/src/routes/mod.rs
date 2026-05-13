//! HTTP routes for `cos-agent-bridge`.
//!
//! `/api/*` carries the actual JSON+SSE API. The non-`/api` routes
//! are a plain static file server for the pre-built React SPA so
//! one process serves both halves and the React app can call
//! same-origin `/api/...` without CORS preflights.

use axum::{Router, routing::{get, post}};
use tower_http::services::ServeDir;

use crate::state::AppState;

mod chat;
mod models;
mod sessions;
mod voice;

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/chat", post(chat::stream_chat))
        .route("/sessions", get(sessions::list))
        .route(
            "/sessions/:id",
            get(sessions::get).delete(sessions::delete_one),
        )
        .route("/models", get(models::list))
        .route("/voice/upload", post(voice::upload))
}

pub fn static_files(state: &AppState) -> Router<AppState> {
    Router::new().fallback_service(
        ServeDir::new(&state.web_root).append_index_html_on_directories(true),
    )
}
