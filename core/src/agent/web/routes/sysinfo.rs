//! `GET /api/sysinfo/:command` — single-shot wrapper around
//! [`crate::sysinfo::run`].
//!
//! The web UI calls this from its dashboard ("Now" tab) every
//! refresh cycle. Each command is read-only (sys.observe) so the
//! permission gate inside `sysinfo::run` is the only thing standing
//! between the browser and the underlying kernel surface; we do not
//! add a second layer here.

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Deserialize, Default)]
pub struct CommandQuery {
    /// Optional space-separated argument list. Mirrors what the
    /// CLI does positionally (e.g. `?args=-n%2010` for `top -n 10`).
    #[serde(default)]
    pub args: Option<String>,
}

pub async fn command(
    Path(name): Path<String>,
    Query(q): Query<CommandQuery>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let argv: Vec<String> = q
        .args
        .as_deref()
        .map(|s| s.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    let v = crate::sysinfo::run(&name, &argv).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": e, "command": name })),
        )
    })?;
    Ok(Json(v))
}
