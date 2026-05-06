//! `cos_todo` — agent task list tool.
//!
//! Mirrors the well-trodden Claude / Cursor TodoWrite pattern: the
//! model maintains an explicit, structured list of subtasks for the
//! current session, marking items as it works through them. The list
//! lives in `<data_dir>/agent/todos/<session_id>.json` so it survives
//! `cos agent ask` invocation boundaries within the same session.
//!
//! Why an explicit todo tool instead of just letting the model
//! free-form plan in chat:
//!   * Forces decomposition into discrete checkable items, which
//!     reduces "I forgot a step" failure modes on long tasks.
//!   * Gives downstream UIs a structured handle to render progress
//!     without parsing prose.
//!   * Persists across context-window compression — even if the
//!     compressor summarises away the planning chat, the list itself
//!     remains canonical.
//!
//! ## Operations
//!
//! - `read`  — return the current list (or empty if none).
//! - `write` — replace the entire list with the supplied items
//!   (whole-list semantics, race-free).
//! - `set_status` — narrow update of a single item by id.
//! - `clear` — wipe the list.
//!
//! Whole-list `write` is the canonical mutation; `set_status` is a
//! convenience that avoids re-sending the full list on a single status
//! flip. Both go through the same on-disk file with an atomic rename
//! so a crashed write can't leave a half-written JSON.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::agent::tools::{Tool, ToolResult};

/// One todo entry. `id` is caller-supplied (must be unique within the
/// list) so the model can reference items between writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TodoItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub status: TodoStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TodoStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

impl TodoStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TodoStatus::Pending => "pending",
            TodoStatus::InProgress => "in_progress",
            TodoStatus::Completed => "completed",
            TodoStatus::Cancelled => "cancelled",
        }
    }
}

/// Whole-list document persisted to disk.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

impl TodoList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return Err if the list contains duplicate ids — would otherwise
    /// silently shadow earlier entries on `set_status`.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen: HashSet<&str> = HashSet::new();
        for item in &self.items {
            if item.id.trim().is_empty() {
                return Err("todo id must be non-empty".into());
            }
            if !seen.insert(item.id.as_str()) {
                return Err(format!("duplicate todo id: {}", item.id));
            }
            if item.title.trim().is_empty() {
                return Err(format!("todo {} has empty title", item.id));
            }
        }
        Ok(())
    }
}

/// On-disk store of per-session todo lists. Files live under
/// `<data_dir>/agent/todos/<session_id>.json`. `session_id` is treated
/// as an opaque token; we sanitise it to forbid path traversal.
pub struct TodoStore {
    root: PathBuf,
}

impl TodoStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// System-default store rooted at `crate::paths::agent_todos_dir()`.
    pub fn default_store() -> Self {
        Self::new(crate::paths::agent_todos_dir())
    }

    fn session_file(&self, session_id: &str) -> Result<PathBuf, String> {
        let trimmed = session_id.trim();
        if trimmed.is_empty() {
            return Err("session_id must be non-empty".into());
        }
        if trimmed.contains('/')
            || trimmed.contains('\\')
            || trimmed.contains("..")
            || trimmed.contains('\0')
        {
            return Err(format!("invalid session_id: {trimmed:?}"));
        }
        Ok(self.root.join(format!("{trimmed}.json")))
    }

    pub fn read(&self, session_id: &str) -> Result<TodoList, String> {
        let path = self.session_file(session_id)?;
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| format!("corrupt todo list at {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TodoList::default()),
            Err(e) => Err(format!("failed to read {}: {e}", path.display())),
        }
    }

    /// Atomic write: serialise → write to `<file>.tmp` → rename. Avoids
    /// half-written JSON on crash.
    pub fn write(&self, session_id: &str, list: &TodoList) -> Result<(), String> {
        list.validate()?;
        let path = self.session_file(session_id)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(list)
            .map_err(|e| format!("serialize todos: {e}"))?;
        fs::write(&tmp, &bytes).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &path).map_err(|e| {
            // Best-effort cleanup of the tmp file on rename failure.
            let _ = fs::remove_file(&tmp);
            format!("rename {} -> {}: {e}", tmp.display(), path.display())
        })?;
        Ok(())
    }

    pub fn set_status(
        &self,
        session_id: &str,
        id: &str,
        status: TodoStatus,
    ) -> Result<TodoList, String> {
        let mut list = self.read(session_id)?;
        let mut found = false;
        for item in list.items.iter_mut() {
            if item.id == id {
                item.status = status;
                found = true;
                break;
            }
        }
        if !found {
            return Err(format!("todo id not found: {id}"));
        }
        self.write(session_id, &list)?;
        Ok(list)
    }

    pub fn clear(&self, session_id: &str) -> Result<(), String> {
        let path = self.session_file(session_id)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("clear {}: {e}", path.display())),
        }
    }
}

/// `cos_todo` tool exposed to the model.
pub struct Todo {
    store: TodoStore,
}

impl Todo {
    pub fn new(store: TodoStore) -> Self {
        Self { store }
    }

    pub fn default_tool() -> Self {
        Self::new(TodoStore::default_store())
    }
}

#[derive(Debug, Deserialize)]
struct TodoInput {
    /// One of `read | write | set_status | clear`.
    command: String,
    /// Caller-supplied session id. Defaults to `"default"` if absent —
    /// useful for ad-hoc tests but the runtime should always pass the
    /// real session id.
    #[serde(default = "default_session")]
    session_id: String,
    /// For `write`: the complete replacement list.
    #[serde(default)]
    items: Option<Vec<TodoItem>>,
    /// For `set_status`: the id to update.
    #[serde(default)]
    id: Option<String>,
    /// For `set_status`: the new status (`pending|in_progress|completed|cancelled`).
    #[serde(default)]
    status: Option<TodoStatus>,
}

fn default_session() -> String {
    "default".to_string()
}

#[async_trait]
impl Tool for Todo {
    fn name(&self) -> &'static str {
        "cos_todo"
    }

    fn description(&self) -> &'static str {
        "Per-session task list. Use to plan and track multi-step work. \
         Commands: read | write (replace entire list) | set_status (update one item by id) | clear."
    }

    fn input_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "enum": ["read", "write", "set_status", "clear"]
                },
                "session_id": {
                    "type": "string",
                    "description": "Session identifier. Pass the current ask session id."
                },
                "items": {
                    "type": "array",
                    "description": "For 'write': the complete replacement list.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id":     { "type": "string" },
                            "title":  { "type": "string" },
                            "status": { "type": "string", "enum": ["pending","in_progress","completed","cancelled"] },
                            "note":   { "type": "string" }
                        },
                        "required": ["id","title"]
                    }
                },
                "id":     { "type": "string", "description": "For 'set_status': item id to update." },
                "status": { "type": "string", "enum": ["pending","in_progress","completed","cancelled"], "description": "For 'set_status'." }
            },
            "required": ["command"]
        })
    }

    async fn exec(&self, input: serde_json::Value) -> ToolResult {
        let parsed: TodoInput = match serde_json::from_value(input) {
            Ok(p) => p,
            Err(e) => return ToolResult::err(format!("invalid todo input: {e}")),
        };
        match parsed.command.as_str() {
            "read" => match self.store.read(&parsed.session_id) {
                Ok(list) => ToolResult::ok(render_list(&list)),
                Err(e) => ToolResult::err(e),
            },
            "write" => {
                let items = parsed.items.unwrap_or_default();
                let list = TodoList { items };
                match self.store.write(&parsed.session_id, &list) {
                    Ok(()) => ToolResult::ok(format!(
                        "wrote {} item(s) to session {}",
                        list.items.len(),
                        parsed.session_id
                    )),
                    Err(e) => ToolResult::err(e),
                }
            }
            "set_status" => {
                let id = match parsed.id.as_deref() {
                    Some(s) if !s.trim().is_empty() => s,
                    _ => return ToolResult::err("set_status requires 'id'"),
                };
                let status = match parsed.status {
                    Some(s) => s,
                    None => return ToolResult::err("set_status requires 'status'"),
                };
                match self.store.set_status(&parsed.session_id, id, status) {
                    Ok(list) => ToolResult::ok(format!(
                        "updated {} -> {}\n{}",
                        id,
                        status.as_str(),
                        render_list(&list)
                    )),
                    Err(e) => ToolResult::err(e),
                }
            }
            "clear" => match self.store.clear(&parsed.session_id) {
                Ok(()) => ToolResult::ok(format!("cleared session {}", parsed.session_id)),
                Err(e) => ToolResult::err(e),
            },
            other => ToolResult::err(format!("unknown command: {other}")),
        }
    }
}

fn render_list(list: &TodoList) -> String {
    if list.items.is_empty() {
        return "(no todos)".to_string();
    }
    let mut out = String::new();
    for item in &list.items {
        let marker = match item.status {
            TodoStatus::Pending => "[ ]",
            TodoStatus::InProgress => "[~]",
            TodoStatus::Completed => "[x]",
            TodoStatus::Cancelled => "[-]",
        };
        out.push_str(&format!("{} {} {}", marker, item.id, item.title));
        if let Some(note) = &item.note {
            out.push_str(&format!("  ({note})"));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (TodoStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = TodoStore::new(dir.path().to_path_buf());
        (store, dir)
    }

    #[test]
    fn read_missing_session_returns_empty() {
        let (store, _g) = tmp_store();
        let list = store.read("nope").unwrap();
        assert!(list.items.is_empty());
    }

    #[test]
    fn write_then_read_round_trips() {
        let (store, _g) = tmp_store();
        let list = TodoList {
            items: vec![
                TodoItem {
                    id: "a".into(),
                    title: "do A".into(),
                    status: TodoStatus::Pending,
                    note: None,
                },
                TodoItem {
                    id: "b".into(),
                    title: "do B".into(),
                    status: TodoStatus::InProgress,
                    note: Some("reason".into()),
                },
            ],
        };
        store.write("s1", &list).unwrap();
        let back = store.read("s1").unwrap();
        assert_eq!(back.items, list.items);
    }

    #[test]
    fn write_rejects_duplicate_ids() {
        let (store, _g) = tmp_store();
        let list = TodoList {
            items: vec![
                TodoItem {
                    id: "a".into(),
                    title: "x".into(),
                    status: TodoStatus::Pending,
                    note: None,
                },
                TodoItem {
                    id: "a".into(),
                    title: "y".into(),
                    status: TodoStatus::Pending,
                    note: None,
                },
            ],
        };
        let err = store.write("s1", &list).unwrap_err();
        assert!(err.contains("duplicate"));
    }

    #[test]
    fn write_rejects_empty_id_and_title() {
        let (store, _g) = tmp_store();
        let list = TodoList {
            items: vec![TodoItem {
                id: " ".into(),
                title: "x".into(),
                status: TodoStatus::Pending,
                note: None,
            }],
        };
        assert!(store.write("s1", &list).is_err());
        let list = TodoList {
            items: vec![TodoItem {
                id: "a".into(),
                title: "  ".into(),
                status: TodoStatus::Pending,
                note: None,
            }],
        };
        assert!(store.write("s1", &list).is_err());
    }

    #[test]
    fn set_status_updates_one_item() {
        let (store, _g) = tmp_store();
        store
            .write(
                "s1",
                &TodoList {
                    items: vec![TodoItem {
                        id: "a".into(),
                        title: "x".into(),
                        status: TodoStatus::Pending,
                        note: None,
                    }],
                },
            )
            .unwrap();
        let updated = store
            .set_status("s1", "a", TodoStatus::Completed)
            .unwrap();
        assert_eq!(updated.items[0].status, TodoStatus::Completed);
    }

    #[test]
    fn set_status_unknown_id_errors() {
        let (store, _g) = tmp_store();
        store.write("s1", &TodoList::default()).unwrap();
        let err = store
            .set_status("s1", "ghost", TodoStatus::Completed)
            .unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn clear_removes_session_file() {
        let (store, _g) = tmp_store();
        store
            .write(
                "s1",
                &TodoList {
                    items: vec![TodoItem {
                        id: "a".into(),
                        title: "x".into(),
                        status: TodoStatus::Pending,
                        note: None,
                    }],
                },
            )
            .unwrap();
        store.clear("s1").unwrap();
        let after = store.read("s1").unwrap();
        assert!(after.items.is_empty());
    }

    #[test]
    fn clear_missing_session_is_noop() {
        let (store, _g) = tmp_store();
        store.clear("never-existed").unwrap();
    }

    #[test]
    fn session_id_path_traversal_rejected() {
        let (store, _g) = tmp_store();
        assert!(store.read("../escape").is_err());
        assert!(store.read("a/b").is_err());
        assert!(store.read("a\\b").is_err());
        assert!(store.read("").is_err());
        assert!(store.read("  ").is_err());
        assert!(store.read("with\0null").is_err());
    }

    #[test]
    fn render_list_marks_status_correctly() {
        let list = TodoList {
            items: vec![
                TodoItem {
                    id: "a".into(),
                    title: "one".into(),
                    status: TodoStatus::Pending,
                    note: None,
                },
                TodoItem {
                    id: "b".into(),
                    title: "two".into(),
                    status: TodoStatus::InProgress,
                    note: None,
                },
                TodoItem {
                    id: "c".into(),
                    title: "three".into(),
                    status: TodoStatus::Completed,
                    note: Some("notes".into()),
                },
                TodoItem {
                    id: "d".into(),
                    title: "four".into(),
                    status: TodoStatus::Cancelled,
                    note: None,
                },
            ],
        };
        let rendered = render_list(&list);
        assert!(rendered.contains("[ ] a one"));
        assert!(rendered.contains("[~] b two"));
        assert!(rendered.contains("[x] c three"));
        assert!(rendered.contains("(notes)"));
        assert!(rendered.contains("[-] d four"));
    }

    #[test]
    fn render_list_empty_message() {
        assert_eq!(render_list(&TodoList::default()), "(no todos)");
    }

    #[tokio::test]
    async fn tool_exec_read_then_write_then_read_via_json() {
        let (store, _g) = tmp_store();
        let tool = Todo::new(store);
        let r = tool
            .exec(json!({"command":"read","session_id":"s1"}))
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("(no todos)"));

        let r = tool
            .exec(json!({
                "command":"write",
                "session_id":"s1",
                "items":[{"id":"a","title":"plan","status":"in_progress"}]
            }))
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("wrote 1"));

        let r = tool
            .exec(json!({"command":"read","session_id":"s1"}))
            .await;
        assert!(r.content.contains("[~] a plan"));
    }

    #[tokio::test]
    async fn tool_exec_set_status_via_json() {
        let (store, _g) = tmp_store();
        let tool = Todo::new(store);
        tool.exec(json!({
            "command":"write",
            "session_id":"s1",
            "items":[{"id":"a","title":"task","status":"pending"}]
        }))
        .await;
        let r = tool
            .exec(json!({
                "command":"set_status",
                "session_id":"s1",
                "id":"a",
                "status":"completed"
            }))
            .await;
        assert!(!r.is_error);
        assert!(r.content.contains("[x] a task"));
    }

    #[tokio::test]
    async fn tool_exec_invalid_command() {
        let (store, _g) = tmp_store();
        let tool = Todo::new(store);
        let r = tool.exec(json!({"command":"frob","session_id":"s1"})).await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn tool_exec_set_status_missing_args() {
        let (store, _g) = tmp_store();
        let tool = Todo::new(store);
        let r = tool
            .exec(json!({"command":"set_status","session_id":"s1"}))
            .await;
        assert!(r.is_error);
    }

    #[tokio::test]
    async fn tool_exec_clear_via_json() {
        let (store, _g) = tmp_store();
        let tool = Todo::new(store);
        tool.exec(json!({
            "command":"write","session_id":"s1",
            "items":[{"id":"a","title":"task"}]
        }))
        .await;
        let r = tool
            .exec(json!({"command":"clear","session_id":"s1"}))
            .await;
        assert!(!r.is_error);
        let r = tool
            .exec(json!({"command":"read","session_id":"s1"}))
            .await;
        assert!(r.content.contains("(no todos)"));
    }

    #[test]
    fn tool_metadata() {
        let tool = Todo::new(TodoStore::new(std::env::temp_dir()));
        assert_eq!(tool.name(), "cos_todo");
        assert!(tool.description().contains("task list"));
        let schema = tool.input_schema();
        assert_eq!(schema["required"][0], "command");
    }
}
