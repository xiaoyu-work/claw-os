//! `cos_recall_semantic` — vector-similarity search over the
//! agent's conversation history.
//!
//! Backed by [`crate::agent::memory::semantic::SemanticStore`]. The
//! runtime auto-indexes every recorded message into this store
//! (see [`crate::agent::runtime::semantic_indexer`]) so the model
//! can find "things meaning roughly X" even when keyword search
//! (`cos_recall search`) misses paraphrases.
//!
//! Subcommands:
//! - `search  {query, limit?, session_id?}`  → top-K by cosine
//! - `count   {session_id?}`                 → row count (default all)
//!
//! Orthogonal to `cos_recall`:
//! - `cos_recall`           — exact-word / FTS5 search (fast, exact)
//! - `cos_recall_semantic`  — meaning-based search (handles paraphrase)

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::memory::semantic::{SemanticHit, SemanticStore};
use crate::agent::tools::{Tool, ToolResult};

const DEFAULT_LIMIT: usize = 10;
const MAX_LIMIT: usize = 50;

pub struct CosRecallSemanticTool {
    store: Arc<SemanticStore>,
}

impl CosRecallSemanticTool {
    pub fn new(store: Arc<SemanticStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for CosRecallSemanticTool {
    fn name(&self) -> &'static str {
        "cos_recall_semantic"
    }

    fn description(&self) -> &'static str {
        "Vector-similarity search over the agent's persistent memory. \
         Returns past messages and app-pushed facts whose MEANING is close \
         to the query, even when no exact keyword matches. With `session_id` \
         given, restricts to that conversation; with `session_id` omitted \
         (recommended for cross-app recall), scans every namespace including \
         `app/<source>` rows produced by calendar/email/search/gateway/etc. \
         Use when the user paraphrases something they said or did earlier. \
         For exact-word search prefer `cos_recall`; for source-filtered \
         structured app-fact queries prefer `cos_app_memory`; for durable \
         Markdown notes use `cos_memory`."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["search", "count"],
                },
                "query": {
                    "type": "string",
                    "description": "Free-text query. Required for 'search'.",
                },
                "session_id": {
                    "type": "string",
                    "description": "Constrain to a specific session id. \
                                    Format: 'session/<sid>' or just '<sid>'.",
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
                return ToolResult::err("missing 'command' (search|count)".to_string());
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
            .map(normalise_namespace);
        let limit = input
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);

        match command.as_str() {
            "search" => {
                if query.is_empty() {
                    return ToolResult::err("'search' requires non-empty 'query'".to_string());
                }
                let ns = session_id.as_deref();
                match self.store.search(ns, &query, limit).await {
                    Ok(hits) => {
                        let v = json!({
                            "query": query,
                            "namespace": ns,
                            "hits": hits.iter().map(hit_to_json).collect::<Vec<_>>(),
                        });
                        let body = serde_json::to_string(&v).unwrap_or_else(|_| v.to_string());
                        ToolResult::ok(crate::agent::safety::untrusted::wrap_untrusted(
                            crate::agent::safety::untrusted::MEMORY_TAG,
                            &body,
                        ))
                    }
                    Err(e) => ToolResult::err(format!("cos_recall_semantic search: {e}")),
                }
            }
            "count" => {
                let ns = session_id.as_deref();
                match self.store.count(ns) {
                    Ok(n) => {
                        let v = json!({
                            "namespace": ns,
                            "count": n,
                        });
                        ToolResult::ok(serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()))
                    }
                    Err(e) => ToolResult::err(format!("cos_recall_semantic count: {e}")),
                }
            }
            other => ToolResult::err(format!(
                "unknown command '{other}'. valid: search|count"
            )),
        }
    }
}

fn normalise_namespace(s: &str) -> String {
    if s.starts_with("session/") {
        s.to_string()
    } else {
        format!("session/{s}")
    }
}

fn hit_to_json(h: &SemanticHit) -> Value {
    json!({
        "id": h.id,
        "namespace": h.namespace,
        "key": h.key,
        "text": h.text,
        "score": h.score,
        "ts_ms": h.ts_ms,
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy/recall_semantic.rs"
    ));
}
