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
const CONTINUITY_IMPORT_MAX_HTTP_BYTES: usize = 512 * 1024;
const CHAT_MAX_HTTP_BYTES: usize = 1024 * 1024;

pub fn api() -> Router<AppState> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route(
            "/chat",
            post(chat::stream_chat).layer(DefaultBodyLimit::max(CHAT_MAX_HTTP_BYTES)),
        )
        .route("/chat/:task_id/cancel", post(chat::cancel_chat))
        .route(
            "/activities",
            get(activities::list).post(activities::create),
        )
        .route(
            "/activities/:id",
            get(activities::get).patch(activities::update),
        )
        .route(
            "/activities/:id/continuity/export",
            get(activities::continuity::export),
        )
        .route(
            "/activities/continuity/import",
            post(activities::continuity::import)
                .layer(DefaultBodyLimit::max(CONTINUITY_IMPORT_MAX_HTTP_BYTES)),
        )
        .route("/activities/:id/receipts", get(activities::receipts))
        .route(
            "/activities/:id/capability-policy",
            get(activities::capability_policy::get).post(activities::capability_policy::set),
        )
        .route(
            "/activities/:id/capability-policy/enabled",
            post(activities::capability_policy::enabled),
        )
        .route(
            "/activities/:id/execution-limits",
            get(activities::execution_limits::get).post(activities::execution_limits::set),
        )
        .route(
            "/activities/:id/execution-limits/enabled",
            post(activities::execution_limits::enabled),
        )
        .route(
            "/activities/:id/monetary-budget",
            get(activities::monetary_budget::get).post(activities::monetary_budget::set),
        )
        .route(
            "/activities/:id/monetary-budget/enabled",
            post(activities::monetary_budget::enabled),
        )
        .route(
            "/activities/:id/scheduling-priority",
            get(activities::scheduling_priority::get).post(activities::scheduling_priority::set),
        )
        .route(
            "/activities/:id/object-state",
            get(activities::object_state_list).post(activities::object_state_record),
        )
        .route(
            "/activities/:id/objects",
            get(activities::objects).post(activities::attach_object),
        )
        .route(
            "/activities/:id/operation-preview",
            post(activities::operation_preview),
        )
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
