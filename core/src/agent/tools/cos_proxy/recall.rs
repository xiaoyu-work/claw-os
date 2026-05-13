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
    fn name(&self) -> &'static str {
        "cos_recall"
    }

    fn description(&self) -> &'static str {
        "Search the agent's full conversation history (every prior turn, every \
         session) using SQLite FTS5. Use this to recall what the user told you \
         earlier, what tools you ran, or what you concluded in a past session. \
         Distinct from cos_memory, which is for durable notes you write \
         deliberately."
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
                    let hits = match &session_id {
                        Some(sid) => db
                            .search_session(sid, &query, limit)
                            .map_err(|e| e.to_string())?,
                        None => db.search(&query, limit).map_err(|e| e.to_string())?,
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
                ToolResult::ok(serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()))
            }
            Ok(Err(msg)) => ToolResult::err(msg),
            Err(e) => ToolResult::err(format!("cos_recall panicked: {e}")),
        }
    }
}

fn row_to_json(row: &MessageRow) -> Value {
    json!({
        "id": row.id,
        "session_id": row.session_id,
        "role": row.role,
        "content": row.content,
        "ts_ms": row.ts_ms,
    })
}

fn hit_to_json(hit: &SearchHit) -> Value {
    json!({
        "id": hit.row.id,
        "session_id": hit.row.session_id,
        "role": hit.row.role,
        "content": hit.row.content,
        "ts_ms": hit.row.ts_ms,
        "rank": hit.rank,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool() -> CosRecallTool {
        CosRecallTool::new(MemoryDb::open_in_memory().unwrap())
    }

    #[tokio::test]
    async fn missing_command_is_tool_error() {
        let r = tool().exec(json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("missing 'command'"));
    }

    #[tokio::test]
    async fn search_finds_inserted_message() {
        let t = tool();
        t.db.record_message("s", "user", "the secret password is rosebud")
            .unwrap();
        let r = t
            .exec(json!({ "command": "search", "query": "rosebud" }))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("rosebud"));
    }

    #[tokio::test]
    async fn search_without_query_errors() {
        let t = tool();
        let r = t.exec(json!({ "command": "search" })).await;
        assert!(r.is_error);
        assert!(r.content.contains("non-empty 'query'"));
    }

    #[tokio::test]
    async fn recent_requires_session_id() {
        let t = tool();
        let r = t.exec(json!({ "command": "recent" })).await;
        assert!(r.is_error);
        assert!(r.content.contains("requires 'session_id'"));
    }

    #[tokio::test]
    async fn recent_returns_session_messages() {
        let t = tool();
        t.db.record_message("alpha", "user", "first").unwrap();
        t.db.record_message("alpha", "assistant", "ok").unwrap();
        t.db.record_message("bravo", "user", "elsewhere").unwrap();
        let r = t
            .exec(json!({ "command": "recent", "session_id": "alpha" }))
            .await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("first"));
        assert!(r.content.contains("ok"));
        assert!(!r.content.contains("elsewhere"));
    }

    #[tokio::test]
    async fn sessions_lists_distinct_session_ids() {
        let t = tool();
        t.db.record_message("a", "user", "x").unwrap();
        t.db.record_message("b", "user", "y").unwrap();
        let r = t.exec(json!({ "command": "sessions" })).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("\"a\""));
        assert!(r.content.contains("\"b\""));
    }

    #[tokio::test]
    async fn stats_returns_total_count() {
        let t = tool();
        t.db.record_message("s", "user", "one").unwrap();
        t.db.record_message("s", "user", "two").unwrap();
        let r = t.exec(json!({ "command": "stats" })).await;
        assert!(!r.is_error, "{}", r.content);
        // total_messages: 2
        assert!(r.content.contains("\"total_messages\":2"));
    }

    #[tokio::test]
    async fn limit_is_clamped() {
        let t = tool();
        for i in 0..5 {
            t.db.record_message("s", "user", &format!("m{i}")).unwrap();
        }
        // limit > MAX_LIMIT must be silently clamped, not rejected.
        let r = t
            .exec(json!({
                "command": "recent",
                "session_id": "s",
                "limit": MAX_LIMIT as i64 + 100,
            }))
            .await;
        assert!(!r.is_error, "{}", r.content);
    }
}
