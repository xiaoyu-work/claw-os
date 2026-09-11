//! `cos_memory` — exposes [`crate::agent::memory::notes`] to the model.
//!
//! Subcommands:
//! - `search  {query, name?, limit?}` → matching excerpts and versioned read handles
//! - `read    {name, offset?, max_chars?}` → return a source page
//! - `write   {name, content}`   → atomically replace file
//! - `append  {name, line}`      → append a line, creating the file if missing
//! - `list`                      → list all `.md` notes in the store
//! - `delete  {name}`            → delete a note (idempotent)
//!
//! Files live under `data_dir/agent/notes/` (see `crate::paths`). Each request
//! selects USER.md and explicitly [always]-pinned entries as bounded profile data.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::memory::notes::NotesStore;
use crate::agent::tools::{Tool, ToolResult};

/// `cos_memory` LLM tool. Holds its own `NotesStore` so tests can inject a
/// temp directory without touching global env vars.
pub struct CosMemoryTool {
    store: NotesStore,
}

impl CosMemoryTool {
    /// Use the system-default notes store (data_dir/agent/notes/).
    pub fn new() -> Self {
        Self {
            store: NotesStore::system_default(),
        }
    }

    /// Use a caller-supplied store (tests / overrides).
    pub fn with_store(store: NotesStore) -> Self {
        Self { store }
    }
}

impl Default for CosMemoryTool {
    fn default() -> Self {
        Self::new()
    }
}

const DEFAULT_NOTE: &str = "MEMORY.md";

#[async_trait]
impl Tool for CosMemoryTool {
    fn name(&self) -> &str {
        "cos_memory"
    }

    fn description(&self) -> &str {
        "Search/read/write the agent's persistent notes (MEMORY.md, USER.md, and any \
         user-named .md note). MEMORY.md is your own working memory across \
         conversations; USER.md captures persistent preferences about the user. \
         The initial request profile includes bounded USER.md and explicitly \
         [always]-pinned entries. Choose which other notes to read based on \
         the user's request; they are not automatically selected by keywords. \
         Write durable facts for future requests; read explicitly when this \
         execution needs a newly written value. Search uses your literal keyword \
         or phrase, case-insensitively; omitted name searches all notes. No matches \
         only describes that query/scope: rephrase or use conversation/semantic recall \
         if needed. Read the returned source, not just its excerpt. Follow next_offset \
         with the returned revision; if the source changed, restart the read. \
         If memory is incomplete, stale or conflicting, continue with a better query \
         or an authoritative current source. Notes are remembered claims, not proof."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["search", "read", "write", "append", "list", "delete"],
                },
                "name": {
                    "type": "string",
                    "description": "Single .md file name. Search: omit to search all notes. Other commands: defaults to MEMORY.md.",
                },
                "query": {
                    "type": "string", "maxLength": 256,
                    "description": "For search: a short literal keyword or phrase chosen by you. Try alternate wording if results are insufficient."
                },
                "limit": {
                    "type": "integer", "minimum": 1, "maximum": 20, "default": 5,
                    "description": "For search: maximum matching excerpts."
                },
                "content": {
                    "type": "string",
                    "description": "For 'write': full new contents. For 'append': line to append.",
                },
                "offset": {
                    "type": "integer", "minimum": 0, "default": 0,
                    "description": "For read: character offset in the note."
                },
                "max_chars": {
                    "type": "integer", "minimum": 1,
                    "maximum": crate::agent::memory::history::MAX_READ_CHARS,
                    "default": crate::agent::memory::history::DEFAULT_READ_CHARS,
                    "description": "For read: maximum characters returned."
                },
                "revision": {
                    "type": "string",
                    "description": "Source revision returned by search/read. Required when offset > 0; prevents combining pages from different versions."
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
                    "missing 'command' (search|read|write|append|list|delete)".to_string(),
                );
            }
        };
        let search_name = match optional_string(&input, "name") {
            Ok(name) => name.map(str::to_string),
            Err(error) => return ToolResult::err(error),
        };
        let name = search_name.as_deref().unwrap_or(DEFAULT_NOTE).to_string();
        let content = if matches!(command.as_str(), "write" | "append") {
            match input.get("content").and_then(Value::as_str) {
                Some(content) => content.to_string(),
                None => {
                    return ToolResult::err(
                        "write/append requires a string content; no note was changed",
                    )
                }
            }
        } else {
            String::new()
        };
        let window = if command == "read" {
            match read_window(&input) {
                Ok(window) => window,
                Err(error) => return ToolResult::err(error),
            }
        } else {
            ReadWindow::default()
        };
        let query = match optional_string(&input, "query") {
            Ok(query) => query.unwrap_or("").to_string(),
            Err(error) => return ToolResult::err(error),
        };
        let search_limit = match input.get("limit") {
            None => 5,
            Some(value) => match value.as_u64().filter(|value| (1..=20).contains(value)) {
                Some(value) => value as usize,
                None => return ToolResult::err("search limit must be within 1..=20"),
            },
        };
        let untrusted_result = matches!(command.as_str(), "search" | "read" | "list");

        // Notes I/O is sync + filesystem — push to blocking pool. Clone the
        // store so the closure can be 'static.
        let store = self.store.clone();
        let join = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            match command.as_str() {
                "search" => {
                    let found = store.search(&query, search_name.as_deref(), search_limit)?;
                    let hits: Vec<_> = found
                        .hits
                        .iter()
                        .map(|hit| {
                            let mut value = serde_json::to_value(hit)
                                .expect("note hits contain only serializable text and integers");
                            value["read"] = json!({
                                "tool": "cos_memory", "command": "read", "name": hit.name,
                                "offset": hit.page.offset, "revision": hit.page.revision,
                            });
                            value
                        })
                        .collect();
                    Ok(json!({
                        "query": found.query, "names": found.names,
                        "searched_names": found.searched_names,
                        "scan_complete": !found.has_more,
                        "match_kind": "literal_text", "limit": search_limit,
                        "has_more": found.has_more, "hits": hits,
                    }))
                }
                "read" => {
                    let text = store.read(&name)?.ok_or_else(|| {
                        format!("note {name} not found; list/search available notes")
                    })?;
                    let page = window.page(&text)?;
                    Ok(json!({
                        "name": name,
                        "content": page.content,
                        "revision": page.revision,
                        "offset": page.offset,
                        "next_offset": page.next_offset,
                        "total_chars": page.total_chars,
                        "source_complete": page.source_complete,
                    }))
                }
                "write" => {
                    store.write(&name, &content)?;
                    Ok(json!({ "name": name, "bytes": content.len() }))
                }
                "append" => {
                    store.append(&name, &content)?;
                    Ok(json!({ "name": name, "appended_bytes": content.len() }))
                }
                "list" => Ok(json!({
                    "dir": store.dir().display().to_string(),
                    "notes": store.list()?,
                })),
                "delete" => {
                    store.delete(&name)?;
                    Ok(json!({ "name": name, "deleted": true }))
                }
                other => Err(format!(
                    "unknown command '{other}'. valid: search|read|write|append|list|delete"
                )),
            }
        })
        .await;

        match join {
            Ok(Ok(v)) => {
                let body = v.to_string();
                ToolResult::ok(if untrusted_result {
                    crate::agent::safety::untrusted::wrap_untrusted(
                        crate::agent::safety::untrusted::MEMORY_TAG,
                        &body,
                    )
                } else {
                    body
                })
            }
            Ok(Err(msg)) => ToolResult::err(msg),
            Err(e) => ToolResult::err(format!("cos_memory panicked: {e}")),
        }
    }
}

pub(super) struct ReadWindow {
    pub offset: usize,
    pub max_chars: usize,
    pub revision: Option<String>,
}

impl Default for ReadWindow {
    fn default() -> Self {
        Self {
            offset: 0,
            max_chars: crate::agent::memory::history::DEFAULT_READ_CHARS,
            revision: None,
        }
    }
}

impl ReadWindow {
    pub fn page(&self, text: &str) -> Result<crate::agent::memory::history::TextPage, String> {
        let actual = crate::agent::memory::history::text_revision(text);
        if self
            .revision
            .as_ref()
            .is_some_and(|expected| !actual.eq_ignore_ascii_case(expected))
        {
            return Err(
                "memory source changed; discard earlier pages and search/read again from offset 0"
                    .into(),
            );
        }
        crate::agent::memory::history::text_page(text, self.offset, self.max_chars)
    }
}

pub(super) fn optional_string<'a>(input: &'a Value, key: &str) -> Result<Option<&'a str>, String> {
    match input.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| format!("{key} must be a string when provided")),
    }
}

pub(super) fn read_window(input: &Value) -> Result<ReadWindow, String> {
    fn number(input: &Value, key: &str, default: usize) -> Result<usize, String> {
        match input.get(key) {
            None => Ok(default),
            Some(value) => value
                .as_u64()
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| format!("{key} must be a non-negative integer")),
        }
    }
    let offset = number(input, "offset", 0)?;
    let max_chars = number(
        input,
        "max_chars",
        crate::agent::memory::history::DEFAULT_READ_CHARS,
    )?;
    if !(1..=crate::agent::memory::history::MAX_READ_CHARS).contains(&max_chars) {
        return Err(format!(
            "max_chars must be within 1..={}",
            crate::agent::memory::history::MAX_READ_CHARS,
        ));
    }
    let revision = match input.get("revision") {
        None => None,
        Some(value) => {
            let value = value
                .as_str()
                .filter(|value| {
                    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .ok_or_else(|| {
                    "revision must be the 64-character source hash returned by search/read"
                        .to_string()
                })?;
            Some(value.to_string())
        }
    };
    if offset > 0 && revision.is_none() {
        return Err(
            "paging requires the source revision returned by the previous search/read".into(),
        );
    }
    Ok(ReadWindow {
        offset,
        max_chars,
        revision,
    })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy/memory.rs"
    ));
}
