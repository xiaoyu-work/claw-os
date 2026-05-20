//! Internal app→agent-memory bridge for bundled and user apps.
//!
//! This module is **not** a user-facing CLI namespace. Apps shell out
//! through `cos __memory <subcommand>` to push a structured summary
//! into the agent's memory; the user inspects or redacts what has
//! been stored from `cos agent memory`.
//!
//! Output envelope is a single JSON object on stdout. Errors are
//! returned as `Err(String)` by [`run`] which the router maps to a
//! non-zero exit + stderr.
//!
//! Authorization: every write goes through
//! [`caps::require(Verb::MEMORY_WRITE, Scope::self_ref(<source>))`].
//! The capability has `ScopeKind::SelfRef`, so the kernel only allows
//! it when the calling session's manifest scoped `memory.write` to its
//! own app id. Apps cannot impersonate each other.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::memory::app_memory::{
    self, AppMemoryEntry, AppMemoryRow, RememberError, RememberOutcome,
};
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::caps::{require, Scope, Verb};

/// Entry point for the hidden `cos __memory` bridge. The first
/// argument is the subcommand (`remember` | `list` | `show` |
/// `search` | `forget`); the rest are subcommand-specific.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    match command {
        "remember" => remember(args),
        "list" => list(args),
        "show" => show(args),
        "search" => search(args),
        "forget" => forget(args),
        _ => Err(format!("unknown internal memory command: {command}")),
    }
}

// ---------------------------------------------------------------------------
// remember
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RememberPayload {
    source: String,
    text: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    entity_id: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    link: Option<String>,
    /// Default true — set to false to skip the semantic embed.
    #[serde(default = "default_indexable")]
    indexable: bool,
}

fn default_indexable() -> bool {
    true
}

fn remember(args: &[String]) -> Result<Value, String> {
    let payload_str = take_json_arg(args, "remember")?;
    let payload: RememberPayload = serde_json::from_str(&payload_str)
        .map_err(|e| format!("memory remember: invalid --json payload: {e}"))?;

    // Capability check: app may only write to its own source. The
    // kernel resolves Scope::SelfRef against the session manifest,
    // so an app whose `memory.write` is bound to `self:expense-tracker`
    // cannot pass `source = "calendar"`.
    require(Verb::MEMORY_WRITE, Scope::self_ref(&payload.source))
        .map_err(|d| format!("memory remember denied: {}", d.to_json()))?;

    let entry = AppMemoryEntry {
        source: payload.source.clone(),
        text: payload.text,
        kind: payload.kind,
        entity_id: payload.entity_id,
        tags: payload.tags,
        link: payload.link,
    };

    let db = open_db()?;
    let store = app_memory::open_default_store();

    let outcome: RememberOutcome = runtime()
        .block_on(app_memory::remember(&db, store.as_ref(), entry, payload.indexable))
        .map_err(remember_error_to_string)?;

    Ok(json!({
        "ok": true,
        "row_id": outcome.row_id,
        "session_id": outcome.session_id,
        "stored_bytes": outcome.stored_bytes,
        "indexed_semantic": outcome.indexed_semantic,
        "text": outcome.text,
    }))
}

fn remember_error_to_string(e: RememberError) -> String {
    match e {
        RememberError::Invalid(m) => format!("memory remember: {m}"),
        RememberError::Db(m) => format!("memory remember: db error: {m}"),
        RememberError::Semantic(m) => format!("memory remember: semantic error: {m}"),
    }
}

// ---------------------------------------------------------------------------
// list / show / search
// ---------------------------------------------------------------------------

fn list(args: &[String]) -> Result<Value, String> {
    let mut source: Option<String> = None;
    let mut limit: usize = 20;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" if i + 1 < args.len() => {
                source = Some(args[i + 1].clone());
                i += 2;
            }
            "--limit" if i + 1 < args.len() => {
                limit = args[i + 1]
                    .parse()
                    .map_err(|e| format!("memory list: --limit must be a positive integer: {e}"))?;
                i += 2;
            }
            other => return Err(format!("memory list: unexpected arg {other}")),
        }
    }
    let db = open_db()?;
    let rows = app_memory::list(&db, source.as_deref(), limit)
        .map_err(|e| format!("memory list: {e}"))?;
    Ok(json!({ "rows": rows_to_json(rows) }))
}

fn show(args: &[String]) -> Result<Value, String> {
    let id: i64 = args
        .first()
        .ok_or_else(|| "memory show: missing <id>".to_string())?
        .parse()
        .map_err(|e| format!("memory show: id must be an integer: {e}"))?;
    let db = open_db()?;
    let row = app_memory::show(&db, id).map_err(|e| format!("memory show: {e}"))?;
    Ok(match row {
        Some(r) => json!({ "row": row_to_json(r) }),
        None => json!({ "row": Value::Null }),
    })
}

fn search(args: &[String]) -> Result<Value, String> {
    if args.is_empty() {
        return Err("memory search: missing <query>".into());
    }
    let query = args[0].clone();
    let mut source: Option<String> = None;
    let mut limit: usize = 20;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--source" if i + 1 < args.len() => {
                source = Some(args[i + 1].clone());
                i += 2;
            }
            "--limit" if i + 1 < args.len() => {
                limit = args[i + 1].parse().map_err(|e| {
                    format!("memory search: --limit must be a positive integer: {e}")
                })?;
                i += 2;
            }
            other => return Err(format!("memory search: unexpected arg {other}")),
        }
    }
    let db = open_db()?;
    let rows = app_memory::search(&db, &query, source.as_deref(), limit)
        .map_err(|e| format!("memory search: {e}"))?;
    Ok(json!({ "rows": rows_to_json(rows) }))
}

// ---------------------------------------------------------------------------
// forget
// ---------------------------------------------------------------------------

fn forget(args: &[String]) -> Result<Value, String> {
    let mut source: Option<String> = None;
    let mut row: Option<i64> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" if i + 1 < args.len() => {
                source = Some(args[i + 1].clone());
                i += 2;
            }
            "--row" if i + 1 < args.len() => {
                row = Some(
                    args[i + 1]
                        .parse()
                        .map_err(|e| format!("memory forget: --row must be an integer: {e}"))?,
                );
                i += 2;
            }
            other => return Err(format!("memory forget: unexpected arg {other}")),
        }
    }
    if source.is_some() == row.is_some() {
        return Err("memory forget: pass exactly one of --source <id> or --row <id>".into());
    }
    let db = open_db()?;
    let store = app_memory::open_default_store();
    if let Some(s) = source {
        let n = app_memory::forget_source(&db, store.as_ref(), &s)
            .map_err(|e| format!("memory forget: {e}"))?;
        return Ok(json!({ "removed": n, "source": s }));
    }
    let id = row.unwrap();
    let removed = app_memory::forget_row(&db, store.as_ref(), id)
        .map_err(|e| format!("memory forget: {e}"))?;
    Ok(json!({ "removed": if removed { 1 } else { 0 }, "row_id": id }))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Lazily-initialised single-threaded tokio runtime for the
/// bridge process. The bridge is invoked once per `cos __memory`
/// call (one subprocess), so a current-thread runtime is enough and
/// avoids spinning up a worker pool we'd immediately tear down.
fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("memory bridge: build tokio runtime")
    })
}

fn open_db() -> Result<MemoryDb, String> {
    MemoryDb::open_default().map_err(|e| format!("memory bridge: open memory db: {e}"))
}

fn take_json_arg(args: &[String], cmd: &str) -> Result<String, String> {
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--json" && i + 1 < args.len() {
            return Ok(args[i + 1].clone());
        }
        i += 1;
    }
    Err(format!("memory {cmd}: missing --json <payload>"))
}

fn row_to_json(r: AppMemoryRow) -> Value {
    json!({
        "id": r.id,
        "source": r.source,
        "ts_ms": r.ts_ms,
        "text": r.text,
        "kind": r.kind,
        "entity_id": r.entity_id,
        "tags": r.tags,
        "link": r.link,
        "rank": r.rank,
    })
}

fn rows_to_json(rows: Vec<AppMemoryRow>) -> Vec<Value> {
    rows.into_iter().map(row_to_json).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_requires_json_arg() {
        let err = remember(&[]).unwrap_err();
        assert!(err.contains("missing --json"));
    }

    #[test]
    fn list_rejects_unknown_flag() {
        let err = list(&["--bogus".into()]).unwrap_err();
        assert!(err.contains("unexpected"));
    }

    #[test]
    fn search_requires_query() {
        let err = search(&[]).unwrap_err();
        assert!(err.contains("missing <query>"));
    }

    #[test]
    fn forget_requires_exactly_one_target() {
        let err = forget(&[]).unwrap_err();
        assert!(err.contains("exactly one"));
        let err = forget(&[
            "--source".into(),
            "expense-tracker".into(),
            "--row".into(),
            "1".into(),
        ])
        .unwrap_err();
        assert!(err.contains("exactly one"));
    }

    #[test]
    fn run_unknown_command() {
        let err = run("nope", &[]).unwrap_err();
        assert!(err.contains("unknown internal memory command"));
    }
}
