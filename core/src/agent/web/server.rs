//! Axum router construction.

use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use crate::agent::web::assets;
use crate::agent::web::auth::require_token;
use crate::agent::web::routes;
use crate::agent::web::state::AppState;

pub fn build_app(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_methods(Any)
        .allow_headers(Any)
        .allow_origin(Any);

    Router::new()
        // Static shell.
        .route("/", get(assets::index))
        .route("/index.html", get(assets::index))
        .route("/favicon.ico", get(assets::favicon))
        // JSON API.
        .route("/api/meta", get(routes::meta::handler))
        .route("/api/chat", post(routes::chat::handler))
        .route("/api/sessions", get(routes::sessions::list))
        .route("/api/sessions/{id}", get(routes::sessions::detail))
        .route("/api/sessions/{id}/history", get(routes::sessions::history))
        .route("/api/tasks", get(routes::tasks::list))
        .route("/api/tasks/{id}", get(routes::tasks::show))
        .route("/api/tasks/{id}/stop", post(routes::tasks::stop))
        .route("/api/tasks/{id}/undo", post(routes::tasks::undo))
        .route("/api/tasks/{id}/resume", post(routes::tasks::resume))
        .route("/api/approvals/pending", get(routes::approvals::pending))
        .route("/api/approvals/recent", get(routes::approvals::recent))
        .route("/api/approvals/{id}/approve", post(routes::approvals::approve))
        .route("/api/approvals/{id}/deny", post(routes::approvals::deny))
        .route("/api/sysinfo/{command}", get(routes::sysinfo::command))
        .route("/api/inbox", get(routes::inbox::list))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ))
        .layer(cors)
        .with_state(state)
}
