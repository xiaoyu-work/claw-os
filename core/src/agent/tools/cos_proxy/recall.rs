//! `cos_recall` — full-text search over the agent's conversation history.
//!
//! Backed by [`crate::agent::memory::sqlite_fts::MemoryDb`]. The model uses
//! this to recall what was said in earlier turns or in earlier sessions —
//! orthogonal to `cos_memory` (which is for durable notes).
//!
//! Subcommands:
//! - `search  {query, limit?, session_id?}`  → FTS5 search ranked by bm25
//! - `recent  {session_id, limit?}`          → most-recent N messages of session
//! - `show    {message_id, session_id?}`     → one complete source message
//! - `sessions {limit?}`                     → list distinct sessions
//! - `stats   {session_id?}`                 → row counts

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::memory::sqlite_fts::{MemoryDb, MessageRow, SearchHit};
use crate::agent::tools::{Tool, ToolResult};

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 200;

pub struct CosRecallTool {
    db: MemoryDb,
}

impl CosRecallTool {
    pub fn new(db: MemoryDb) -> Self {
        Self { db }
    }
}

#[async_trait]
impl Tool for CosRecallTool {
    fn name(&self) -> &str {
        "cos_recall"
    }

    fn description(&self) -> &str {
        "Search the agent's full conversation history (every prior turn, every \
         session) using SQLite FTS5. Use this to recall what the user told you \
         earlier, what tools you ran, or what you concluded in a past session. \
         Incidentally also returns structured facts that apps have pushed \
         (calendar events, sent emails, etc.) because they share the same \
         FTS index — for source-filtered queries over app facts specifically, \
         prefer `cos_app_memory`. Distinct from `cos_memory`, which is for \
         durable Markdown notes you write deliberately. Use 'show' with a \
         returned message_id to expand one source without replaying a whole session. \
         Search/recent return bounded excerpts, not complete evidence. Follow \
         next_offset with show offset/max_chars and the returned revision. \
         If results are insufficient, off-topic, stale or conflicting, refine the \
         query/scope, use semantic recall for paraphrases, or inspect original/current \
         sources. No matches is only a result for this query/scope. BM25 rank measures \
         retrieval relevance, not truth; historical success does not prove current state."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["search", "recent", "show", "sessions", "stats"],
                },
                "query": {
                    "type": "string",
                    "description": "Free-text search query. Used by 'search'."
                },
                "session_id": {
                    "type": "string",
                    "description": "Constrain to a specific session id."
                },
                "message_id": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Source message id returned by recall. Required for 'show'."
                },
                "offset": {
                    "type": "integer", "minimum": 0, "default": 0,
                    "description": "For show: character offset in the disclosed source."
                },
                "max_chars": {
                    "type": "integer", "minimum": 1,
                    "maximum": crate::agent::memory::history::MAX_READ_CHARS,
                    "default": crate::agent::memory::history::DEFAULT_READ_CHARS,
                    "description": "For show: maximum characters returned."
                },
                "revision": {
                    "type": "string",
                    "description": "Source revision from search/show. Required for offset > 0; restart the read if it changed."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT as i64,
                    "default": DEFAULT_LIMIT as i64,
                },
            },
            "required": ["command"],
            "additionalProperties": false,
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let command = match input.get("command").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return ToolResult::err(
                    "missing 'command' (search|recent|show|sessions|stats)".to_string(),
                );
            }
        };
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let session_id = match super::memory::optional_string(&input, "session_id") {
            Ok(value) if value.is_none_or(|value| !value.trim().is_empty()) => {
                value.map(str::to_string)
            }
            Ok(_) => return ToolResult::err("session_id must be non-empty when provided"),
            Err(error) => return ToolResult::err(error),
        };
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);
        let message_id = input
            .get("message_id")
            .and_then(Value::as_i64)
            .filter(|id| *id > 0);
        let window = if command == "show" {
            match super::memory::read_window(&input) {
                Ok(window) => window,
                Err(error) => return ToolResult::err(error),
            }
        } else {
            super::memory::ReadWindow::default()
        };

        let db = self.db.clone();
        let join = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            match command.as_str() {
                "show" => {
                    let id = message_id
                        .ok_or_else(|| "'show' requires a positive 'message_id'".to_string())?;
                    let row = db
                        .message(id)
                        .map_err(|error| error.to_string())?
                        .filter(|row| {
                            row.role != crate::agent::memory::sqlite_fts::INJECTED_ROLE
                                && session_id.as_ref().is_none_or(|sid| *sid == row.session_id)
                        })
                        .ok_or_else(|| {
                            "source message not found in the requested scope".to_string()
                        })?;
                    let text = crate::agent::memory::history::sanitize_stored_content(
                        &row.role,
                        &row.content,
                    );
                    let page = window.page(&text)?;
                    Ok(json!({ "message": row_page_to_json(&row, &page) }))
                }
                "search" => {
                    if query.trim().is_empty() {
                        return Err("'search' requires non-empty 'query'".to_string());
                    }
                    let mut hits = db
                        .search_history(&query, session_id.as_deref(), limit + 1)
                        .map_err(|error| error.to_string())?;
                    let has_more = hits.len() > limit;
                    hits.truncate(limit);
                    let preview_chars = preview_chars(hits.len());
                    Ok(json!({
                        "query": query,
                        "session_id": session_id,
                        "limit": limit,
                        "has_more": has_more,
                        "match_kind": "fts5",
                        "score_kind": "bm25_not_confidence",
                        "hits": hits.iter().map(|hit| hit_to_json(hit, preview_chars)).collect::<Vec<_>>(),
                    }))
                }
                "recent" => {
                    let sid = session_id
                        .clone()
                        .ok_or_else(|| "'recent' requires 'session_id'".to_string())?;
                    let mut rows = db
                        .recent_replayable(&sid, limit + 1)
                        .map_err(|e| e.to_string())?;
                    let has_more = rows.len() > limit;
                    if has_more {
                        rows.remove(0);
                    }
                    let preview_chars = preview_chars(rows.len());
                    Ok(json!({
                        "session_id": sid,
                        "limit": limit,
                        "has_more": has_more,
                        "messages": rows.iter().map(|row| row_to_json(row, preview_chars)).collect::<Vec<_>>(),
                    }))
                }
                "sessions" => {
                    let summaries = db.sessions(limit).map_err(|e| e.to_string())?;
                    Ok(json!({
                        "sessions": summaries
                            .iter()
                            .map(|s| json!({
                                "session_id": s.session_id,
                                "last_ts_ms": s.last_ts_ms,
                                "message_count": s.message_count,
                            }))
                            .collect::<Vec<_>>(),
                    }))
                }
                "stats" => {
                    let total = db.count_total().map_err(|e| e.to_string())?;
                    let session_count = match &session_id {
                        Some(sid) => Some(db.count_session(sid).map_err(|e| e.to_string())?),
                        None => None,
                    };
                    Ok(json!({
                        "total_messages": total,
                        "session_id": session_id,
                        "session_messages": session_count,
                    }))
                }
                other => Err(format!(
                    "unknown command '{other}'. valid: search|recent|show|sessions|stats"
                )),
            }
        })
        .await;

        match join {
            Ok(Ok(v)) => {
                // Recalled history is prior-session content (it can quote
                // web pages, emails, or app output the agent ingested).
                // Wrap it so an injected instruction can't be read as a
                // command to this agent.
                let body = serde_json::to_string(&v).unwrap_or_else(|_| v.to_string());
                ToolResult::ok(crate::agent::safety::untrusted::wrap_untrusted(
                    crate::agent::safety::untrusted::MEMORY_TAG,
                    &body,
                ))
            }
            Ok(Err(msg)) => ToolResult::err(msg),
            Err(e) => ToolResult::err(format!("cos_recall panicked: {e}")),
        }
    }
}

fn preview_chars(count: usize) -> usize {
    (crate::agent::memory::history::DEFAULT_READ_CHARS / count.max(1)).min(512)
}

fn row_to_json(row: &MessageRow, max_chars: usize) -> Value {
    let content = crate::agent::memory::history::sanitize_stored_content(&row.role, &row.content);
    let page = crate::agent::memory::history::text_page(&content, 0, max_chars)
        .expect("a zero offset and fixed positive excerpt length are valid");
    row_page_to_json(row, &page)
}

fn row_page_to_json(row: &MessageRow, page: &crate::agent::memory::history::TextPage) -> Value {
    json!({
        "id": row.id,
        "session_id": row.session_id,
        "role": row.role,
        "content": page.content,
        "revision": page.revision,
        "ts_ms": row.ts_ms,
        "offset": page.offset,
        "next_offset": page.next_offset,
        "total_chars": page.total_chars,
        "source_complete": page.source_complete,
        "read": {
            "tool": "cos_recall",
            "command": "show",
            "message_id": row.id,
            "session_id": row.session_id,
            "offset": page.next_offset.unwrap_or(0),
            "revision": page.revision,
        },
    })
}

fn hit_to_json(hit: &SearchHit, max_chars: usize) -> Value {
    let mut value = row_to_json(&hit.row, max_chars);
    value["rank"] = json!(hit.rank);
    value
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy/recall.rs"
    ));
}
