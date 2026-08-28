//! `cos_app_memory` — recall facts that apps have stored about the user.
//!
//! Apps with the `memory.write` capability emit structured one-line
//! summaries every time they do something on the user's behalf — a
//! calendar event created, an email sent, a document summarised, a web
//! search run, a chat dispatched through a gateway, etc. Those rows
//! live in the same FTS5 store as the agent's own conversation
//! history (see [`crate::agent::memory::app_memory`]), but they're
//! tagged with `source = "<app-id>"` and structured fields
//! (`kind`, `entity_id`, `tags`, `link`).
//!
//! This tool exposes the dedicated query surface so the LLM can ask
//! questions like "what hotel did I expense in March?" or "list every
//! calendar event I created last week" and get back the app-pushed
//! facts without sifting through whole conversation transcripts.
//!
//! Subcommands:
//! - `list   {source?, kind?, limit?}`        → recent rows, newest first
//! - `search {query, source?, kind?, limit?}` → FTS5 search, bm25-ranked
//! - `show   {id}`                            → fetch one row by id
//!
//! Orthogonal to the existing tools:
//! - `cos_recall`           — also FTS5, but over CONVERSATION history.
//!   It does include app rows incidentally because they share the
//!   table, but it has no source/kind filter and no structured fields.
//! - `cos_recall_semantic`  — vector similarity; covers `app/<source>`
//!   namespaces when called with no `session_id`.
//! - `cos_memory`           — Markdown notes (MEMORY.md / USER.md), a
//!   completely different storage.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::memory::app_memory::{self, AppMemoryRow};
use crate::agent::memory::sqlite_fts::MemoryDb;
use crate::agent::tools::exposure::{MemoryExposure, ToolExposure};
use crate::agent::tools::{Tool, ToolResult};

const DEFAULT_LIST_LIMIT: usize = 20;
const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_LIMIT: usize = 100;

/// LLM tool surface for app-pushed memory rows.
pub struct CosAppMemoryTool {
    db: MemoryDb,
}

impl CosAppMemoryTool {
    pub fn new(db: MemoryDb) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Tool for CosAppMemoryTool {
    fn name(&self) -> &str {
        "cos_app_memory"
    }

    fn description(&self) -> &str {
        "Recall structured facts that apps have stored about the user's activity. \
         Calendar app stores events the user created or updated; the email app \
         stores messages the user sent; mail-ai stores triage decisions and \
         summaries; the doc and web apps store summaries the user asked for; \
         the search app stores search queries the user ran; browser-attached \
         stores pages the user navigated to; gateway apps (slack, telegram, \
         whatsapp, sms, teams, discord, signal, matrix, webex, googlechat, \
         mattermost, rocketchat, zulip, larksuite, dingtalk, homeassistant, \
         pushover, ntfy, webhook, gateway-email) store outbound messages. \
         Use this whenever the user asks 'when did I...', 'how much was the...', \
         'what did I send to...', 'show me my recent...', or any factual recall \
         about past activity. Each row has a `source` (the app id), optional \
         `kind` (event/fact/note), `entity_id`, `tags`, and a `link` shell \
         command the user can run to re-open the underlying record."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["list", "search", "show"],
                    "description": "list = recent rows; search = FTS5 keyword search; show = fetch one row by id.",
                },
                "query": {
                    "type": "string",
                    "description": "Required for 'search'. Free-text query — wrapped as an FTS5 phrase internally so punctuation is safe.",
                },
                "source": {
                    "type": "string",
                    "description": "Optional filter by app id (e.g. 'calendar', 'email', 'gateway-slack'). Omit to span every app.",
                },
                "kind": {
                    "type": "string",
                    "description": "Optional post-filter on the row's `kind` field (e.g. 'event', 'fact', 'summary').",
                },
                "id": {
                    "type": "integer",
                    "description": "Row id. Required for 'show'.",
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT as i64,
                    "description": "Max rows to return. Defaults: 20 for list, 10 for search.",
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_memory(
            [crate::caps::Verb::MEMORY_READ],
            MemoryExposure::SystemAgentOrApp,
        )
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let command = match input.get("command").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return ToolResult::err(
                    "missing 'command' (list|search|show)".to_string(),
                );
            }
        };
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let source = input
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string);
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .map(|s| s.to_lowercase());
        let id = input.get("id").and_then(Value::as_i64);
        let limit_raw = input.get("limit").and_then(Value::as_u64).map(|n| n as usize);

        match command.as_str() {
            "list" => {
                if let Err(error) = authorize_source(source.as_deref()) {
                    return ToolResult::err(error);
                }
            }
            "search" => {
                if query.is_empty() {
                    return ToolResult::err("'search' requires non-empty 'query'");
                }
                if let Err(error) = authorize_source(source.as_deref()) {
                    return ToolResult::err(error);
                }
            }
            "show" => {
                let Some(id) = id else {
                    return ToolResult::err("'show' requires 'id'");
                };
                let db = self.db.clone();
                let row = match tokio::task::spawn_blocking(move || app_memory::show(&db, id)).await
                {
                    Ok(Ok(row)) => row,
                    Ok(Err(error)) => return ToolResult::err(error.to_string()),
                    Err(error) => {
                        return ToolResult::err(format!("cos_app_memory panicked: {error}"))
                    }
                };
                let authorization = match row.as_ref() {
                    Some(row) => authorize_source(Some(&row.source)),
                    None => authorize_source(None),
                };
                if let Err(error) = authorization {
                    return ToolResult::err(error);
                }
                return render_result(json!({
                    "row": row.as_ref().map(row_to_json).unwrap_or(Value::Null),
                }));
            }
            other => {
                return ToolResult::err(format!(
                    "unknown command '{other}'. valid: list|search|show"
                ))
            }
        }

        let db = self.db.clone();
        let join = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            match command.as_str() {
                "list" => {
                    let limit = limit_raw
                        .unwrap_or(DEFAULT_LIST_LIMIT)
                        .clamp(1, MAX_LIMIT);
                    let rows = app_memory::list(&db, source.as_deref(), limit)
                        .map_err(|e| e.to_string())?;
                    let filtered = filter_by_kind(rows, kind.as_deref());
                    Ok(json!({
                        "source": source,
                        "kind": kind,
                        "rows": filtered.iter().map(row_to_json).collect::<Vec<_>>(),
                        "n": filtered.len(),
                    }))
                }
                "search" => {
                    let limit = limit_raw
                        .unwrap_or(DEFAULT_SEARCH_LIMIT)
                        .clamp(1, MAX_LIMIT);
                    // Wrap as a single FTS5 phrase so punctuation and
                    // reserved keywords in the user query don't blow
                    // up the parser. See cos_recall::escape_fts5_query
                    // for the same trick.
                    let fts_query = escape_fts5_query(&query);
                    // Over-fetch to compensate for kind post-filter
                    // (if any), then trim.
                    let fetch = if kind.is_some() {
                        limit.saturating_mul(3).min(MAX_LIMIT.saturating_mul(3))
                    } else {
                        limit
                    };
                    let rows = app_memory::search(&db, &fts_query, source.as_deref(), fetch)
                        .map_err(|e| e.to_string())?;
                    let filtered: Vec<AppMemoryRow> = filter_by_kind(rows, kind.as_deref())
                        .into_iter()
                        .take(limit)
                        .collect();
                    Ok(json!({
                        "query": query,
                        "source": source,
                        "kind": kind,
                        "rows": filtered.iter().map(row_to_json).collect::<Vec<_>>(),
                        "n": filtered.len(),
                    }))
                }
                "show" => unreachable!("show returns before the worker closure"),
                other => Err(format!("unexpected validated command '{other}'")),
            }
        })
        .await;

        match join {
            Ok(Ok(v)) => render_result(v),
            Ok(Err(msg)) => ToolResult::err(msg),
            Err(e) => ToolResult::err(format!("cos_app_memory panicked: {e}")),
        }
    }
}

fn authorize_source(source: Option<&str>) -> Result<(), String> {
    let scope = match source {
        Some(source) => {
            app_memory::validate_source(source)?;
            crate::agent::tools::MemoryScope::App(source)
        }
        None => crate::agent::tools::MemoryScope::SystemAgent,
    };
    crate::agent::tools::require_memory(crate::caps::Verb::MEMORY_READ, scope)
        .map_err(|denial| denial.to_string())
}

fn render_result(value: Value) -> ToolResult {
    // App memory holds content apps recorded from external sources; wrap as
    // untrusted prior-session data.
    let body = serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
    ToolResult::ok(crate::agent::safety::untrusted::wrap_untrusted(
        crate::agent::safety::untrusted::MEMORY_TAG,
        &body,
    ))
}

fn row_to_json(r: &AppMemoryRow) -> Value {
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

fn filter_by_kind(rows: Vec<AppMemoryRow>, kind: Option<&str>) -> Vec<AppMemoryRow> {
    let Some(k) = kind else {
        return rows;
    };
    rows.into_iter()
        .filter(|r| r.kind.as_deref().map(|x| x.eq_ignore_ascii_case(k)).unwrap_or(false))
        .collect()
}

/// Same trick as `cos_recall`: wrap the query as a single
/// double-quoted FTS5 phrase, escaping interior `"` to `""`. Keeps
/// punctuation, `AND`/`OR`/`NEAR`, and column-filter syntax from
/// leaking into the parser.
fn escape_fts5_query(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('"');
    for ch in q.chars() {
        if ch == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(ch);
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy/app_memory.rs"
    ));
}
