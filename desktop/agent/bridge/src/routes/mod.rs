//! HTTP routes for `cos-agent-bridge`.
//!
//! Pure `/api/*` JSON+SSE surface — no static file serving. The
//! React SPA was retired in favour of `cos-agent-ui` (native
//! libcosmic), which talks to these endpoints directly. If a stray
//! browser hits `/`, axum 404s, which is what we want.

use axum::{
    Router,
    extract::DefaultBodyLimit,
    routing::{get, post},
};

use crate::state::AppState;

mod activities;
mod chat;
mod models;
mod sessions;
mod voice;

/// Voice uploads carry raw audio (e.g. `audio/webm`); a few minutes of speech
/// easily exceeds axum's 2 MiB default body limit, so raise it on this route.
const VOICE_MAX_BYTES: usize = 25 * 1024 * 1024;

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/chat", post(chat::stream_chat))
        .route("/chat/:task_id/cancel", post(chat::cancel_chat))
        .route("/activities", get(activities::list).post(activities::create))
        .route("/activities/:id", get(activities::get).patch(activities::update))
        .route("/activities/:id/transition", post(activities::transition))
        .route("/activities/:id/run", post(activities::run))
        .route("/tasks/:task_id/retry", post(activities::retry_job))
        .route("/sessions", get(sessions::list))
        .route(
            "/sessions/:id",
            get(sessions::get).delete(sessions::delete_one),
        )
        .route("/sessions/:id/history", get(sessions::history))
        .route("/models", get(models::list))
        .route(
            "/voice/upload",
            post(voice::upload).layer(DefaultBodyLimit::max(VOICE_MAX_BYTES)),
        )
}
