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
//! - `read    {namespace, key, offset?, revision?}` → versioned source page
//! - `count   {session_id?}`                 → row count (default all)
//!
//! Orthogonal to `cos_recall`:
//! - `cos_recall`           — exact-word / FTS5 search (fast, exact)
//! - `cos_recall_semantic`  — meaning-based search (handles paraphrase)

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::memory::semantic::{SemanticHit, SemanticStore};
use crate::agent::tools::exposure::{MemoryExposure, ToolExposure};
use crate::agent::tools::{Tool, ToolResult};
use crate::agent::trust::{LabeledSegment, SourceKind, TrustClass};

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
    fn name(&self) -> &str {
        "cos_recall_semantic"
    }

    fn description(&self) -> &str {
        "Vector-similarity search over the agent's persistent memory. \
         Returns past messages and app-pushed facts whose MEANING is close \
         to the query, even when no exact keyword matches. With `session_id` \
         given, restricts to that conversation; with `session_id` omitted \
         (recommended for cross-app recall), scans every namespace including \
         `app/<source>` rows produced by calendar/email/search/gateway/etc. \
         Use when the user paraphrases something they said or did earlier. \
         For exact-word search prefer `cos_recall`; for source-filtered \
         structured app-fact queries prefer `cos_app_memory`; for durable \
         Markdown notes use `cos_memory`. Scores measure similarity, not factual \
         confidence. Results are indexed historical snapshots; indexed_at_ms is \
         not the original event time. If a result is insufficient or questionable, \
         follow its read/original_source handle, refine the query/namespace, or \
         check the owning App/system's current state. Follow next_offset with \
         revision; changed sources require restarting the read. Do not stop at \
         the first plausible match or interpret no matches as proof of no memory."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["search", "read", "count"],
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
                "namespace": {
                    "type": "string",
                    "description": "Exact index namespace, including app/<id> or session/<id>. Required for read; optional filter for search/count. Do not combine with session_id."
                },
                "key": {
                    "type": "string",
                    "description": "Stable key returned by search; required for read."
                },
                "offset": {
                    "type": "integer", "minimum": 0, "default": 0,
                    "description": "For read: character offset in the indexed source."
                },
                "max_chars": {
                    "type": "integer", "minimum": 1,
                    "maximum": crate::agent::memory::history::MAX_READ_CHARS,
                    "default": crate::agent::memory::history::DEFAULT_READ_CHARS,
                },
                "revision": {
                    "type": "string",
                    "description": "Revision returned by search/read. Required for offset > 0; restart if the source changes."
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

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_memory(
            [crate::caps::Verb::MEMORY_READ],
            MemoryExposure::SystemAgentOrSession,
        )
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let command = match input.get("command").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return ToolResult::err("missing 'command' (search|read|count)".to_string());
            }
        };
        if !matches!(command.as_str(), "search" | "read" | "count") {
            return ToolResult::err(format!(
                "unknown command '{command}'. valid: search|read|count"
            ));
        }
        let query = match super::memory::optional_string(&input, "query") {
            Ok(value) => value.unwrap_or("").to_string(),
            Err(error) => return ToolResult::err(error),
        };
        if command == "search" && query.trim().is_empty() {
            return ToolResult::err("'search' requires non-empty 'query'");
        }
        if input.get("namespace").is_some() && input.get("session_id").is_some() {
            return ToolResult::err("use namespace or session_id, not both");
        }
        let namespace = input
            .get("namespace")
            .and_then(Value::as_str)
            .map(str::to_string);
        if input.get("namespace").is_some()
            && namespace
                .as_ref()
                .is_none_or(|value| value.trim().is_empty())
        {
            return ToolResult::err("namespace must be a non-empty string");
        }
        let session = match super::memory::optional_string(&input, "session_id") {
            Ok(value)
                if value.is_none_or(|value| {
                    !value
                        .strip_prefix("session/")
                        .unwrap_or(value)
                        .trim()
                        .is_empty()
                }) =>
            {
                value
            }
            Ok(_) => return ToolResult::err("session_id must be non-empty when provided"),
            Err(error) => return ToolResult::err(error),
        };
        let namespace = namespace.or_else(|| session.map(normalise_namespace));
        if let Some(namespace) = namespace.as_deref() {
            if let Err(error) = crate::agent::tools::validate_memory_scope(namespace, "namespace") {
                return ToolResult::err(error);
            }
            if let Some(source) = namespace.strip_prefix("app/") {
                if let Err(error) = crate::agent::memory::app_memory::validate_source(source) {
                    return ToolResult::err(error);
                }
            }
        }
        let limit = match super::memory::result_limit(&input, DEFAULT_LIMIT, MAX_LIMIT) {
            Ok(limit) => limit,
            Err(error) => return ToolResult::err(error),
        };
        let key = if command == "read" {
            if namespace.is_none() {
                return ToolResult::err("read requires namespace and key from a search result");
            }
            let key = match super::memory::optional_string(&input, "key") {
                Ok(Some(key)) => key,
                Ok(None) => {
                    return ToolResult::err("read requires a non-empty key from a search result")
                }
                Err(error) => return ToolResult::err(error),
            };
            if let Err(error) = crate::agent::tools::validate_memory_scope(key, "key") {
                return ToolResult::err(error);
            }
            Some(key)
        } else {
            None
        };
        let window = if command == "read" {
            match super::memory::read_window(&input) {
                Ok(window) => window,
                Err(error) => return ToolResult::err(error),
            }
        } else {
            super::memory::ReadWindow::default()
        };
        let scope = match namespace
            .as_deref()
            .and_then(|value| value.strip_prefix("session/"))
        {
            Some(session_id) => {
                if let Err(error) =
                    crate::agent::tools::validate_memory_scope(session_id, "session_id")
                {
                    return ToolResult::err(error);
                }
                crate::agent::tools::MemoryScope::Session(session_id)
            }
            None => crate::agent::tools::MemoryScope::SystemAgent,
        };
        if let Err(denial) =
            crate::agent::tools::require_memory(crate::caps::Verb::MEMORY_READ, scope)
        {
            return ToolResult::err(denial.to_string());
        }

        match command.as_str() {
            "search" => {
                let ns = namespace.as_deref();
                match self.store.search(ns, &query, limit + 1).await {
                    Ok(mut hits) => {
                        let has_more = hits.len() > limit;
                        hits.truncate(limit);
                        let max_chars = (crate::agent::memory::history::DEFAULT_READ_CHARS
                            / hits.len().max(1))
                        .min(512);
                        let v = json!({
                            "query": query,
                            "namespace": ns,
                            "limit": limit,
                            "has_more": has_more,
                            "score_kind": "cosine_similarity_not_confidence",
                            "source_kind": "semantic_index_snapshot",
                            "hits": hits.iter().map(|hit| hit_to_json(hit, max_chars)).collect::<Vec<_>>(),
                        });
                        // The semantic index has no persisted provenance columns.
                        // Its snapshots cannot acquire trust merely by being retrieved.
                        super::memory::memory_result_with_class(
                            SourceKind::RecalledMemory,
                            TrustClass::LegacyUnknown,
                            &v,
                        )
                    }
                    Err(e) => ToolResult::err(format!("cos_recall_semantic search: {e}")),
                }
            }
            "read" => {
                let namespace = namespace.as_deref().expect("validated above");
                let key = key.expect("validated above");
                match self.store.get(namespace, key) {
                    Ok(Some(row)) => {
                        let page = match window.page(&row.text) {
                            Ok(page) => page,
                            Err(error) => return ToolResult::err(error),
                        };
                        let mut value = source_page(namespace, key, &page);
                        value["id"] = json!(row.id);
                        value["model"] = json!(row.model);
                        value["indexed_at_ms"] = json!(row.ts_ms);
                        value["provenance"] =
                            json!(LabeledSegment::of(SourceKind::LegacyStoredRow, &row.text)
                                .manifest_entry());
                        super::memory::memory_result_with_class(
                            SourceKind::RecalledMemory,
                            TrustClass::LegacyUnknown,
                            &value,
                        )
                    }
                    Ok(None) => ToolResult::err(
                        "indexed source no longer exists; search again or use its original source",
                    ),
                    Err(error) => ToolResult::err(format!("cos_recall_semantic read: {error}")),
                }
            }
            "count" => {
                let ns = namespace.as_deref();
                match self.store.count(ns) {
                    Ok(n) => {
                        let v = json!({
                            "namespace": ns,
                            "count": n,
                        });
                        super::memory::memory_result(SourceKind::RecalledMemory, &v)
                    }
                    Err(e) => ToolResult::err(format!("cos_recall_semantic count: {e}")),
                }
            }
            other => ToolResult::err(format!("unexpected validated command '{other}'")),
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

fn hit_to_json(hit: &SemanticHit, max_chars: usize) -> Value {
    let page = crate::agent::memory::history::text_page(&hit.text, 0, max_chars)
        .expect("a bounded first-page window is valid");
    let mut value = source_page(&hit.namespace, &hit.key, &page);
    value["id"] = json!(hit.id);
    value["score"] = json!(hit.score);
    value["ts_ms"] = json!(hit.ts_ms);
    value["indexed_at_ms"] = json!(hit.ts_ms);
    value["model"] = json!(hit.model);
    value["provenance"] =
        json!(LabeledSegment::of(SourceKind::LegacyStoredRow, &hit.text).manifest_entry());
    value
}

fn source_page(
    namespace: &str,
    key: &str,
    page: &crate::agent::memory::history::TextPage,
) -> Value {
    json!({
        "namespace": namespace, "key": key, "text": page.content,
        "revision": page.revision, "offset": page.offset,
        "next_offset": page.next_offset, "total_chars": page.total_chars,
        "source_complete": page.source_complete,
        "source_kind": "semantic_index_snapshot",
        "read": {
            "tool": "cos_recall_semantic", "command": "read",
            "namespace": namespace, "key": key,
            "offset": page.next_offset.unwrap_or(0), "revision": page.revision,
        },
        "original_source": original_source(namespace, key),
    })
}

fn original_source(namespace: &str, key: &str) -> Option<Value> {
    if let Some(session_id) = namespace.strip_prefix("session/") {
        let (role, id) = key.rsplit_once('-')?;
        if !matches!(role, "user" | "assistant" | "tool") {
            return None;
        }
        let id = id.parse::<i64>().ok().filter(|id| *id > 0)?;
        Some(json!({
            "tool": "cos_recall", "command": "show",
            "session_id": session_id, "message_id": id,
        }))
    } else if let Some(source) = namespace.strip_prefix("app/") {
        let id = key.parse::<i64>().ok().filter(|id| *id > 0)?;
        Some(json!({
            "tool": "cos_app_memory", "command": "show", "source": source, "id": id,
        }))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy/recall_semantic.rs"
    ));
}
