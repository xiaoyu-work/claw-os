//! `cos_recall` — full-text search over the agent's conversation history.
//!
//! Backed by [`crate::agent::memory::sqlite_fts::MemoryDb`]. The model uses
//! this to recall what was said in earlier turns or in earlier sessions —
//! orthogonal to `cos_memory` (which is for durable notes).
//!
//! Subcommands:
//! - `search  {query, limit?, session_id?}`  → FTS5 search ranked by bm25
//! - `recent  {session_id, limit?}`          → most-recent N messages of session
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
         durable Markdown notes you write deliberately."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["search", "recent", "sessions", "stats"],
                },
                "query": {
                    "type": "string",
                    "description": "Free-text search query. Used by 'search'."
                },
                "session_id": {
                    "type": "string",
                    "description": "Constrain to a specific session id."
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
                    "missing 'command' (search|recent|sessions|stats)".to_string(),
                );
            }
        };
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let session_id = input
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);

        let db = self.db.clone();
        let join = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            match command.as_str() {
                "search" => {
                    if query.is_empty() {
                        return Err("'search' requires non-empty 'query'".to_string());
                    }
                    // FTS5 query strings have their own grammar (`AND`,
                    // `OR`, `NEAR`, column filters like `body:foo`). A
                    // raw user query containing `:` or `"` can either
                    // mis-parse (HTTP 500) or escape into a different
                    // column. Wrap the whole user blob as a single
                    // double-quoted phrase and double-escape interior
                    // `"`s — FTS5 treats `""` as a literal `"` inside
                    // a quoted phrase. This preserves "give me back what
                    // I typed" semantics; advanced users wanting raw
                    // operators can use the `cos_memory` direct path.
                    let fts_query = escape_fts5_query(&query);
                    let hits = match &session_id {
                        Some(sid) => db
                            .search_session(sid, &fts_query, limit)
                            .map_err(|e| e.to_string())?,
                        None => db.search(&fts_query, limit).map_err(|e| e.to_string())?,
                    };
                    Ok(json!({
                        "query": query,
                        "session_id": session_id,
                        "hits": hits.iter().map(hit_to_json).collect::<Vec<_>>(),
                    }))
                }
                "recent" => {
                    let sid = session_id
                        .clone()
                        .ok_or_else(|| "'recent' requires 'session_id'".to_string())?;
                    let rows = db.recent(&sid, limit).map_err(|e| e.to_string())?;
                    Ok(json!({
                        "session_id": sid,
                        "messages": rows.iter().map(row_to_json).collect::<Vec<_>>(),
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
                    "unknown command '{other}'. valid: search|recent|sessions|stats"
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

fn row_to_json(row: &MessageRow) -> Value {
    let content =
        crate::agent::memory::history::sanitize_stored_content(&row.role, &row.content);
    json!({
        "id": row.id,
        "session_id": row.session_id,
        "role": row.role,
        "content": content,
        "ts_ms": row.ts_ms,
    })
}

fn hit_to_json(hit: &SearchHit) -> Value {
    let content = crate::agent::memory::history::sanitize_stored_content(
        &hit.row.role,
        &hit.row.content,
    );
    json!({
        "id": hit.row.id,
        "session_id": hit.row.session_id,
        "role": hit.row.role,
        "content": content,
        "ts_ms": hit.row.ts_ms,
        "rank": hit.rank,
    })
}

/// Escape a free-text user query for FTS5's MATCH grammar.
///
/// FTS5 reserves `:` for column filters (`body:foo`), `"` for phrase
/// delimiters, `-` for negation, `*` for prefix, parens for grouping,
/// and the bare keywords `AND` / `OR` / `NOT` / `NEAR`. The model is
/// not in control of the raw FTS dialect — it asks for "search for X"
/// and expects literal matching. Wrap the entire query as a single
/// double-quoted phrase and double-escape interior `"` to a pair (the
/// SQLite-documented escape rule for FTS5 quoted phrases). Whitespace
/// inside the phrase is still tokenised by FTS5 as a multi-word
/// phrase-with-stopwords-allowed search.
///
/// The empty string would build `""` which is a valid-but-empty FTS5
/// match (returns no rows); the caller already rejects empty queries.
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
        "/test/unit/agent/tools/cos_proxy/recall.rs"
    ));
}
