//! `cos_memory` — exposes [`crate::agent::memory::notes`] to the model.
//!
//! Subcommands:
//! - `read    {name}`            → return file contents (empty string if missing)
//! - `write   {name, content}`   → atomically replace file
//! - `append  {name, line}`      → append a line, creating the file if missing
//! - `list`                      → list all `.md` notes in the store
//! - `delete  {name}`            → delete a note (idempotent)
//!
//! Files live under `data_dir/agent/notes/` (see `crate::paths`). New sessions
//! snapshot MEMORY.md and USER.md into their canonical system prompt.

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agent::memory::notes::NotesStore;
use crate::agent::tools::exposure::{MemoryExposure, ToolExposure};
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
        "Read/write the agent's persistent notes (MEMORY.md, USER.md, and any \
         user-named .md note). MEMORY.md is your own working memory across \
         conversations; USER.md captures persistent preferences about the user. \
         New sessions snapshot both into their system prompt. Write durable \
         facts for future sessions; read a note explicitly if this session \
         needs a newly written value immediately."
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

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always().requiring_memory(
            [
                crate::caps::Verb::MEMORY_READ,
                crate::caps::Verb::MEMORY_WRITE,
            ],
            MemoryExposure::SystemAgent,
        )
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
        let required_verb = match command.as_str() {
            "read" | "list" => crate::caps::Verb::MEMORY_READ,
            "write" | "append" | "delete" => crate::caps::Verb::MEMORY_WRITE,
            other => {
                return ToolResult::err(format!(
                    "unknown command '{other}'. valid: read|write|append|list|delete"
                ))
            }
        };
        if command != "list" {
            if let Err(error) = crate::agent::memory::notes::validate_name(&name) {
                return ToolResult::err(error);
            }
        }
        if let Err(denial) = crate::agent::tools::require_memory(
            required_verb,
            crate::agent::tools::MemoryScope::SystemAgent,
        ) {
            return ToolResult::err(denial.to_string());
        }

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
                other => Err(format!("unexpected validated command '{other}'")),
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_proxy/memory.rs"
    ));
}
