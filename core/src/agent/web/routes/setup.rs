//! `cos agent setup` endpoints — surfaces the same modality status /
//! provider catalogue / apply / test commands the CLI wizard uses, so
//! the web UI can be a first-class onboarding surface (the SSE chat
//! route deliberately *doesn't* gate on `is_ready`, see `web::mod`).
//!
//! Every endpoint is a thin wrapper around `crate::agent::setup::run`
//! — we build a `Vec<String>` argv as if the user had typed it on the
//! CLI, hand it to the existing dispatcher, and return the JSON it
//! produces verbatim. Keeps the web surface and the terminal surface
//! in lockstep with no duplicated business logic.

use axum::extract::{Json as JsonExtract, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::setup;
use crate::agent::web::state::AppState;

async fn run_args(args: Vec<String>) -> Response {
    // setup::run is sync and uses its own `block_on` for HTTP I/O
    // (OAuth, verification). Calling it directly inside an axum tokio
    // task would panic. `block_in_place` runs the blocking work on the
    // *current* task instead of spawning to a separate worker — that
    // both avoids the nested-runtime panic and keeps the
    // `with_override` task-local visible from inside `setup::run`.
    // `spawn_blocking` would NOT inherit task-locals, so the config
    // override below would be silently ignored.
    //
    // We refresh the on-disk config and install it as a per-task
    // override. Without this the daemon's process-wide `CONFIG`
    // (`OnceLock`) keeps the snapshot taken at startup forever — every
    // status call after an `apply` (including the one this very handler
    // just performed via `setup::run`) returns the stale pre-apply
    // state and the UI shows "not configured" right after the user has
    // just configured the provider.
    let cfg = crate::config::intern_user_config();
    crate::config::with_override(cfg, async move {
        let outcome = tokio::task::block_in_place(|| setup::run(&args));
        match outcome {
            Ok(v) => Json(v).into_response(),
            Err(e) => {
                let body =
                    serde_json::from_str::<Value>(&e).unwrap_or_else(|_| json!({"error": e}));
                (StatusCode::BAD_REQUEST, Json(body)).into_response()
            }
        }
    })
    .await
}

pub async fn status_all(State(_): State<AppState>) -> Response {
    run_args(vec!["all".into(), "--status".into()]).await
}

pub async fn status_modality(
    State(_): State<AppState>,
    Path(modality): Path<String>,
) -> Response {
    run_args(vec![modality, "--status".into()]).await
}

pub async fn providers_modality(
    State(_): State<AppState>,
    Path(modality): Path<String>,
) -> Response {
    run_args(vec!["providers".into(), modality]).await
}

#[derive(Deserialize)]
pub struct ApplyReq {
    pub modality: String,
    pub provider: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_key_credential: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_version: Option<String>,
    #[serde(default)]
    pub verify: bool,
}

pub async fn apply(
    State(_): State<AppState>,
    JsonExtract(req): JsonExtract<ApplyReq>,
) -> Response {
    let mut args: Vec<String> = vec!["apply".into(), req.modality, "--provider".into(), req.provider];
    if let Some(v) = req.model {
        args.push("--model".into());
        args.push(v);
    }
    if let Some(v) = req.api_key {
        args.push("--api-key".into());
        args.push(v);
    } else if let Some(v) = req.api_key_credential {
        args.push("--api-key".into());
        args.push(v);
    } else if let Some(v) = req.api_key_env {
        args.push("--api-key-env".into());
        args.push(v);
    }
    if let Some(v) = req.base_url {
        args.push("--base-url".into());
        args.push(v);
    }
    if let Some(v) = req.api_version {
        args.push("--api-version".into());
        args.push(v);
    }
    if !req.verify {
        args.push("--no-verify".into());
    }
    run_args(args).await
}

pub async fn test(
    State(_): State<AppState>,
    Path(modality): Path<String>,
) -> Response {
    run_args(vec![modality, "--verify-only".into()]).await
}

#[derive(Deserialize)]
pub struct OauthStartReq {
    pub provider: String,
    #[serde(default = "default_text_modality")]
    pub modality: String,
}

fn default_text_modality() -> String {
    "text".into()
}

pub async fn oauth_start(
    State(_): State<AppState>,
    JsonExtract(req): JsonExtract<OauthStartReq>,
) -> Response {
    run_args(vec![
        "oauth-start".into(),
        req.modality,
        "--provider".into(),
        req.provider,
    ])
    .await
}

#[derive(Deserialize)]
pub struct OauthPollReq {
    pub provider: String,
    pub device_code: String,
    #[serde(default = "default_text_modality")]
    pub modality: String,
}

pub async fn oauth_poll(
    State(_): State<AppState>,
    JsonExtract(req): JsonExtract<OauthPollReq>,
) -> Response {
    run_args(vec![
        "oauth-poll".into(),
        req.modality,
        "--provider".into(),
        req.provider,
        "--device-code".into(),
        req.device_code,
    ])
    .await
}

pub async fn list_models_for_provider(
    State(_): State<AppState>,
    Path((modality, provider)): Path<(String, String)>,
) -> Response {
    run_args(vec![
        "models".into(),
        modality,
        "--provider".into(),
        provider,
    ])
    .await
}

pub async fn reset_modality(
    State(_): State<AppState>,
    Path(modality): Path<String>,
) -> Response {
    run_args(vec![modality, "--reset".into()]).await
}
