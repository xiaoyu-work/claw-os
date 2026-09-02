//! `GET /api/sessions` and friends — owner-local conversation memory.

use std::collections::BTreeMap;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::agent::memory::history::load_history;
use crate::agent::memory::sqlite_fts::{MemoryDb, SessionSummary};
use crate::agent::web::state::AppState;

pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let mut by_id = BTreeMap::<String, SessionSummary>::new();
    for db in open_owner_databases(&state)? {
        for row in db
            .sessions(200)
            .map_err(|error| internal(format!("read sessions: {error}")))?
        {
            match by_id.get(&row.session_id) {
                Some(current) if current.last_ts_ms >= row.last_ts_ms => {}
                _ => {
                    by_id.insert(row.session_id.clone(), row);
                }
            }
        }
    }
    let mut rows = by_id.into_values().collect::<Vec<_>>();
    rows.sort_by_key(|row| std::cmp::Reverse(row.last_ts_ms));
    rows.truncate(200);
    let sessions = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.session_id,
                "title": row.title,
                "last_ts_ms": row.last_ts_ms,
                "message_count": row.message_count,
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "n": sessions.len(), "sessions": sessions })))
}

pub async fn detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    for db in open_owner_databases(&state)? {
        if db
            .has_session(&id)
            .map_err(|error| internal(format!("read session: {error}")))?
        {
            let title = db
                .title_for(&id)
                .map_err(|error| internal(format!("read session title: {error}")))?;
            return Ok(Json(json!({ "id": id, "title": title })));
        }
    }
    Err((
        StatusCode::NOT_FOUND,
        Json(json!({ "error": format!("session not found: {id}") })),
    ))
}

pub async fn history(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    for db in open_owner_databases(&state)? {
        if !db
            .has_session(&id)
            .map_err(|error| internal(format!("read session: {error}")))?
        {
            continue;
        }
        let mut messages = load_history(&db, &id, 500)
            .map_err(|error| internal(format!("read history: {error}")))?;
        messages.retain(|message| message.role != "system");
        return Ok(Json(json!({
            "session_id": id,
            "n": messages.len(),
            "messages": messages,
        })));
    }
    Ok(Json(json!({ "session_id": id, "n": 0, "messages": [] })))
}

fn open_owner_databases(state: &AppState) -> Result<Vec<MemoryDb>, (StatusCode, Json<Value>)> {
    let paths = [
        crate::paths::system_agent_memory_db_path(state.inner.owner_uid),
        crate::paths::agent_memory_db_path(),
    ];
    let mut databases = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        if !path.is_file() || paths[..index].contains(path) {
            continue;
        }
        databases.push(
            MemoryDb::open_read_only(path)
                .map_err(|error| internal(format!("open memory: {error}")))?,
        );
    }
    Ok(databases)
}

fn internal(msg: String) -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": msg })),
    )
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/web/routes/sessions.rs"
    ));
}
