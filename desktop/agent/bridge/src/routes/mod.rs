//! HTTP routes for `cos-agent-bridge`.
//!
//! Pure `/api/*` JSON+SSE surface — no static file serving. The
//! React SPA was retired in favour of `cos-agent-ui` (native
//! libcosmic), which talks to these endpoints directly. If a stray
//! browser hits `/`, axum 404s, which is what we want.

use axum::{
    Router,
    routing::{get, post},
};

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
