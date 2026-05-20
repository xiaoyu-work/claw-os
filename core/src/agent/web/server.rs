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
        .route("/favicon.png", get(assets::favicon))
        .route("/clawos-symbol.png", get(assets::brand_symbol_light))
        .route("/clawos-symbol-dark.png", get(assets::brand_symbol_dark))
        .route("/assets/{file}", get(assets::asset))
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
        // Setup / configuration — surfaces the same wizard the CLI has,
        // so the web UI can be a complete first-run onboarding surface.
        .route("/api/setup/status", get(routes::setup::status_all))
        .route("/api/setup/status/{modality}", get(routes::setup::status_modality))
        .route("/api/setup/providers/{modality}", get(routes::setup::providers_modality))
        .route("/api/setup/models/{modality}/{provider}", get(routes::setup::list_models_for_provider))
        .route("/api/setup/apply", post(routes::setup::apply))
        .route("/api/setup/test/{modality}", post(routes::setup::test))
        .route("/api/setup/reset/{modality}", post(routes::setup::reset_modality))
        .route("/api/setup/oauth/start", post(routes::setup::oauth_start))
        .route("/api/setup/oauth/poll", post(routes::setup::oauth_poll))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_token,
        ))
        .layer(cors)
        .with_state(state)
}
