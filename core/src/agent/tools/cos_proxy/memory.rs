//! `cos_memory` — exposes [`crate::agent::memory::notes`] to the model.
//!
//! Subcommands:
//! - `read    {name}`            → return file contents (empty string if missing)
//! - `write   {name, content}`   → atomically replace file
//! - `append  {name, line}`      → append a line, creating the file if missing
//! - `list`                      → list all `.md` notes in the store
//! - `delete  {name}`            → delete a note (idempotent)
//!
//! Files live under `data_dir/agent/notes/` (see `crate::paths`). MEMORY.md
//! and USER.md are also injected into the system prompt automatically by
//! `agent::prompt::build_system_prompt`.

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
    fn name(&self) -> &'static str {
        "cos_memory"
    }

    fn description(&self) -> &'static str {
        "Read/write the agent's persistent notes (MEMORY.md, USER.md, and any \
         user-named .md note). MEMORY.md is your own working memory across \
         conversations; USER.md captures persistent preferences about the user. \
         Both are auto-injected into your system prompt every turn — write to \
         them when you learn something durable."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["read", "write", "append", "list", "delete"],
                },
                "name": {
                    "type": "string",
                    "description": "Note file name (must end with .md). Defaults to MEMORY.md.",
                    "default": DEFAULT_NOTE,
                },
                "content": {
                    "type": "string",
                    "description": "For 'write': full new contents. For 'append': line to append.",
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
                    "missing 'command' (read|write|append|list|delete)".to_string(),
                );
            }
        };
        let name = input
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_NOTE)
            .to_string();
        let content = input
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        // Notes I/O is sync + filesystem — push to blocking pool. Clone the
        // store so the closure can be 'static.
        let store = self.store.clone();
        let join = tokio::task::spawn_blocking(move || -> Result<Value, String> {
            match command.as_str() {
                "read" => Ok(json!({
                    "name": name,
                    "content": store.read(&name)?.unwrap_or_default(),
                })),
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
                    "unknown command '{other}'. valid: read|write|append|list|delete"
                )),
            }
        })
        .await;

        match join {
            Ok(Ok(v)) => {
                ToolResult::ok(serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()))
            }
            Ok(Err(msg)) => ToolResult::err(msg),
            Err(e) => ToolResult::err(format!("cos_memory panicked: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_store(label: &str) -> NotesStore {
        let dir: PathBuf = std::env::temp_dir().join(format!(
            "cos-tool-mem-{}-{}-{}",
            label,
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        NotesStore::at(dir)
    }

    #[tokio::test]
    async fn write_then_read_roundtrips_via_tool() {
        let t = CosMemoryTool::with_store(tmp_store("rw"));
        let r = t
            .exec(json!({
                "command": "write",
                "name": "MEMORY.md",
                "content": "tool wrote this",
            }))
            .await;
        assert!(!r.is_error, "write failed: {}", r.content);

        let r = t
            .exec(json!({ "command": "read", "name": "MEMORY.md" }))
            .await;
        assert!(!r.is_error, "read failed: {}", r.content);
        assert!(
            r.content.contains("tool wrote this"),
            "content was: {}",
            r.content
        );
    }

    #[tokio::test]
    async fn list_returns_known_files() {
        let t = CosMemoryTool::with_store(tmp_store("list"));
        t.exec(json!({"command":"write","name":"MEMORY.md","content":"x"}))
            .await;
        t.exec(json!({"command":"write","name":"USER.md","content":"y"}))
            .await;
        let r = t.exec(json!({"command":"list"})).await;
        assert!(!r.is_error, "{}", r.content);
        assert!(r.content.contains("MEMORY.md"), "content: {}", r.content);
        assert!(r.content.contains("USER.md"), "content: {}", r.content);
    }

    #[tokio::test]
    async fn invalid_name_is_returned_as_tool_error() {
        let t = CosMemoryTool::with_store(tmp_store("bad-name"));
        let r = t
            .exec(json!({"command":"write","name":"../escape.md","content":"x"}))
            .await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn append_extends_existing_note() {
        let t = CosMemoryTool::with_store(tmp_store("append"));
        t.exec(json!({"command":"append","name":"MEMORY.md","content":"line1"}))
            .await;
        t.exec(json!({"command":"append","name":"MEMORY.md","content":"line2"}))
            .await;
        let r = t.exec(json!({"command":"read","name":"MEMORY.md"})).await;
        assert!(r.content.contains("line1") && r.content.contains("line2"));
    }

    #[tokio::test]
    async fn missing_command_is_tool_error() {
        let t = CosMemoryTool::with_store(tmp_store("no-cmd"));
        let r = t.exec(json!({})).await;
        assert!(r.is_error);
        assert!(r.content.contains("missing 'command'"));
    }
}
