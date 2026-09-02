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
//! own app id. Apps cannot impersonate each other. The `list`,
//! `search`, and `show` subcommands likewise require
//! [`caps::require(Verb::MEMORY_READ, Scope::self_ref(<source>))`] and
//! `forget` re-uses `MEMORY_WRITE` against the owning source.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::agent::memory::app_memory::{
    self, AppMemoryEntry, AppMemoryRow, RememberError, RememberOutcome,
};
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::caps::{require, Cap, CapSet, Denial, Scope, Verb};

/// Who decides whether one memory call may proceed.
///
/// Outside a sandbox the calling process *is* the session, so the
/// kernel's registry-backed check applies directly. A sandboxed worker
/// has no session row to resolve and no route to the owner's store, so
/// the launcher runs the call on its behalf and the decision comes from
/// that launch's live authority instead. Both paths derive the same
/// verb and `self:<source>` scope from the same parsed arguments — only
/// the authority that answers differs.
pub(crate) trait MemoryAuthority {
    fn allow(&self, verb: Verb, scope: Scope) -> Result<(), String>;
}

/// The default: the kernel's own capability check for this session.
pub(crate) struct SessionAuthority;

impl MemoryAuthority for SessionAuthority {
    fn allow(&self, verb: Verb, scope: Scope) -> Result<(), String> {
        require(verb, scope).map_err(|denial| denial.to_json().to_string())
    }
}

/// A launch's live capability set, used by the worker broker when it
/// runs a sandboxed App's memory call against the owner's store.
pub(crate) struct LaunchAuthority {
    caps: CapSet,
}

impl LaunchAuthority {
    pub(crate) fn new(caps: CapSet) -> Self {
        Self { caps }
    }
}

impl MemoryAuthority for LaunchAuthority {
    fn allow(&self, verb: Verb, scope: Scope) -> Result<(), String> {
        if self.caps.covers(&Cap::new(verb, scope.clone())) {
            return Ok(());
        }
        Err(Denial::scope_out_of_range(verb, scope, &self.caps)
            .to_json()
            .to_string())
    }
}

/// Entry point for the hidden `cos __memory` bridge. The first
/// argument is the subcommand (`remember` | `list` | `show` |
/// `search` | `forget`); the rest are subcommand-specific.
///
/// Inside a worker sandbox the owner's memory database is not mounted —
/// `COS_DATA_DIR` is the App's own partition — so the call is forwarded
/// to the launch's broker endpoint, which re-parses it here and answers
/// from the launch's live authority. Apps keep writing to the same
/// cross-App agent memory they always have, and hostile code still
/// cannot open the database or reach another source's rows.
pub fn run(command: &str, args: &[String]) -> Result<Value, String> {
    if let Some(result) = crate::worker::broker::sandbox_memory_call(command, args) {
        return result;
    }
    run_with(&SessionAuthority, command, args)
}

/// Execute one memory call, taking every authorization decision from
/// `auth`.
pub(crate) fn run_with(
    auth: &dyn MemoryAuthority,
    command: &str,
    args: &[String],
) -> Result<Value, String> {
    match command {
        "remember" => remember(auth, args),
        "list" => list(auth, args),
        "show" => show(auth, args),
        "search" => search(auth, args),
        "forget" => forget(auth, args),
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

fn remember(auth: &dyn MemoryAuthority, args: &[String]) -> Result<Value, String> {
    let payload_str = take_json_arg(args, "remember")?;
    let payload: RememberPayload = serde_json::from_str(&payload_str)
        .map_err(|e| format!("memory remember: invalid --json payload: {e}"))?;

    // Capability check: app may only write to its own source. The
    // kernel resolves Scope::SelfRef against the session manifest,
    // so an app whose `memory.write` is bound to `self:expense-tracker`
    // cannot pass `source = "calendar"`.
    auth.allow(Verb::MEMORY_WRITE, Scope::self_ref(&payload.source))
        .map_err(|denial| format!("memory remember denied: {}", denial))?;

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
    }
}

// ---------------------------------------------------------------------------
// list / show / search
// ---------------------------------------------------------------------------

fn list(auth: &dyn MemoryAuthority, args: &[String]) -> Result<Value, String> {
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
    // Apps may only enumerate their own namespace. The agent runtime
    // reads in-process and never goes through this bridge.
    let source = source.ok_or_else(|| "memory list: --source is required".to_string())?;
    auth.allow(Verb::MEMORY_READ, Scope::self_ref(&source))
        .map_err(|denial| format!("memory list denied: {}", denial))?;
    let db = open_db()?;
    let rows = app_memory::list(&db, Some(source.as_str()), limit)
        .map_err(|e| format!("memory list: {e}"))?;
    Ok(json!({ "rows": rows_to_json(rows) }))
}

fn show(auth: &dyn MemoryAuthority, args: &[String]) -> Result<Value, String> {
    let id: i64 = args
        .first()
        .ok_or_else(|| "memory show: missing <id>".to_string())?
        .parse()
        .map_err(|e| format!("memory show: id must be an integer: {e}"))?;
    let db = open_db()?;
    let row = app_memory::show(&db, id).map_err(|e| format!("memory show: {e}"))?;
    // A row owned by another source has to be indistinguishable from
    // one that was never written. Answering "denied" for the first and
    // "null" for the second would let an App walk the id space and
    // learn which rows the owner's other Apps hold. The capability
    // check still runs, and is still audited; its outcome only decides
    // whether *this* caller sees the row.
    let visible = match row {
        Some(row)
            if auth
                .allow(Verb::MEMORY_READ, Scope::self_ref(&row.source))
                .is_ok() =>
        {
            Some(row)
        }
        _ => None,
    };
    Ok(match visible {
        Some(r) => json!({ "row": row_to_json(r) }),
        None => json!({ "row": Value::Null }),
    })
}

fn search(auth: &dyn MemoryAuthority, args: &[String]) -> Result<Value, String> {
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
    let source = source.ok_or_else(|| "memory search: --source is required".to_string())?;
    auth.allow(Verb::MEMORY_READ, Scope::self_ref(&source))
        .map_err(|denial| format!("memory search denied: {}", denial))?;
    let db = open_db()?;
    let rows = app_memory::search(&db, &query, Some(source.as_str()), limit)
        .map_err(|e| format!("memory search: {e}"))?;
    Ok(json!({ "rows": rows_to_json(rows) }))
}

// ---------------------------------------------------------------------------
// forget
// ---------------------------------------------------------------------------

fn forget(auth: &dyn MemoryAuthority, args: &[String]) -> Result<Value, String> {
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
        // Deleting every row for a source is a write — gate it.
        auth.allow(Verb::MEMORY_WRITE, Scope::self_ref(&s))
            .map_err(|denial| format!("memory forget denied: {}", denial))?;
        let n = app_memory::forget_source(&db, store.as_ref(), &s)
            .map_err(|e| format!("memory forget: {e}"))?;
        return Ok(json!({ "removed": n, "source": s }));
    }
    let id = row.unwrap();
    // For row-id deletion the owning source decides the scope, so it
    // has to be read first. A row belonging to another source answers
    // exactly like a row that is not there — `{"removed": 0}` — because
    // a distinct denial would turn `forget` into an existence oracle
    // over every other App's rows. The check is still made, and still
    // audited; it only decides whether the delete happens.
    let target = app_memory::show(&db, id).map_err(|e| format!("memory forget: {e}"))?;
    let permitted = match target.as_ref() {
        Some(r) => auth
            .allow(Verb::MEMORY_WRITE, Scope::self_ref(&r.source))
            .is_ok(),
        None => false,
    };
    let removed = if permitted {
        app_memory::forget_row(&db, store.as_ref(), id)
            .map_err(|e| format!("memory forget: {e}"))?
    } else {
        false
    };
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/mem_bridge.rs"
    ));
}
