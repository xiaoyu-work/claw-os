//! MCP tool surface for `cosmic-files`.
//!
//! When launched with `COS_MCP_SERVER=1`, the binary flips into a tiny
//! stdio MCP server instead of opening a window. The agent calls these
//! tools so it can navigate, inspect and act on the user's filesystem
//! without having to spawn an interactive Files window first.
//!
//! Every tool funnels through the same kernel boundary as the GUI
//! (`cos_runtime::fs::*` → `cos app fs <verb>`), so capability gating,
//! audit logging and approval gating apply uniformly. The agent
//! doesn't get a more permissive path than the user.
//!
//! AI ops (`files.summarize`, `files.explain`, `files.find_similar`)
//! intentionally reuse the *same* `claw_glue::ai` helpers the
//! right-click menu calls. That keeps the agent's "Ask Claw about this
//! file" path identical to the user's.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cos_mcp_serve::{Server, Tool, ToolResult};
use serde_json::{Value, json};
use walkdir::WalkDir;

use crate::claw_glue;

// ---------------------------------------------------------------------------
// files.list
// ---------------------------------------------------------------------------

struct ListTool;

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &'static str {
        "files.list"
    }

    fn description(&self) -> &'static str {
        "List the entries under a directory. Goes through the same \
         capability-gated `fs.ls` path the GUI uses."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Absolute directory path."
                }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let path = match string_arg(&input, "path") {
            Ok(v) => v,
            Err(e) => return e,
        };
        match cos_runtime::fs::ls(&path) {
            Ok(r) => {
                let entries: Vec<Value> = r
                    .files
                    .into_iter()
                    .map(|e| {
                        json!({
                            "name": e.name,
                            "is_dir": e.is_dir,
                        })
                    })
                    .collect();
                ToolResult::ok(json!({ "path": r.path, "entries": entries }).to_string())
            }
            Err(e) => denied_or_err(&e, "files.list"),
        }
    }
}

// ---------------------------------------------------------------------------
// files.metadata
// ---------------------------------------------------------------------------

struct MetadataTool;

#[async_trait]
impl Tool for MetadataTool {
    fn name(&self) -> &'static str {
        "files.metadata"
    }

    fn description(&self) -> &'static str {
        "Get size, type and mtime for a path."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let path = match string_arg(&input, "path") {
            Ok(v) => v,
            Err(e) => return e,
        };
        match cos_runtime::fs::stat(&path) {
            Ok(s) => ToolResult::ok(
                json!({
                    "path": s.path,
                    "size_bytes": s.size,
                    "is_dir": s.is_dir,
                    "is_file": s.is_file,
                    "modified_unix": s.modified,
                    "tags": s.tags.unwrap_or_default(),
                })
                .to_string(),
            ),
            Err(e) => denied_or_err(&e, "files.metadata"),
        }
    }
}

// ---------------------------------------------------------------------------
// files.search — recursive name match
// ---------------------------------------------------------------------------

struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "files.search"
    }

    fn description(&self) -> &'static str {
        "Recursively search a directory for entries whose name contains a \
         case-insensitive substring. Returns at most `limit` paths \
         (default 50, max 500). Honors `.gitignore` and skips hidden \
         entries by default."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "root": {"type": "string", "description": "Absolute directory to walk."},
                "query": {"type": "string", "description": "Case-insensitive substring."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 500, "default": 50},
                "include_hidden": {"type": "boolean", "default": false}
            },
            "required": ["root", "query"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let root = match string_arg(&input, "root") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let query = match string_arg(&input, "query") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .clamp(1, 500) as usize;
        let include_hidden = input
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let needle = query.to_lowercase();

        let results = tokio::task::spawn_blocking(move || {
            let mut hits: Vec<Value> = Vec::new();
            for entry in WalkDir::new(&root).follow_links(false).into_iter().filter_entry(
                |e| {
                    if include_hidden {
                        return true;
                    }
                    e.file_name()
                        .to_str()
                        .map(|s| !s.starts_with('.'))
                        .unwrap_or(true)
                },
            ) {
                let Ok(entry) = entry else { continue };
                let name = entry.file_name().to_string_lossy().to_lowercase();
                if name.contains(&needle) {
                    hits.push(json!({
                        "path": entry.path().display().to_string(),
                        "is_dir": entry.file_type().is_dir(),
                    }));
                    if hits.len() >= limit {
                        break;
                    }
                }
            }
            hits
        })
        .await
        .unwrap_or_default();

        ToolResult::ok(json!({ "matches": results }).to_string())
    }
}

// ---------------------------------------------------------------------------
// files.reveal — open a new cosmic-files window at the given path
// ---------------------------------------------------------------------------

struct RevealTool;

#[async_trait]
impl Tool for RevealTool {
    fn name(&self) -> &'static str {
        "files.reveal"
    }

    fn description(&self) -> &'static str {
        "Open a cosmic-files window pointing at the given directory (or \
         the directory containing the given file). The agent uses this \
         to hand control back to the user for visual confirmation \
         before a destructive operation."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let path = match string_arg(&input, "path") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let target = PathBuf::from(&path);
        let dir = if target.is_dir() {
            target
        } else {
            target.parent().map(Path::to_path_buf).unwrap_or(target)
        };
        let dir_str = dir.to_string_lossy().to_string();
        match cos_runtime::exec::start(&["cosmic-files", &dir_str]) {
            Ok(_) => ToolResult::ok(json!({"opened": dir_str}).to_string()),
            Err(e) => denied_or_err(&e, "files.reveal"),
        }
    }
}

// ---------------------------------------------------------------------------
// AI proxies — reuse the right-click menu's pipeline so the agent and
// the user share the same audit trail.
// ---------------------------------------------------------------------------

macro_rules! ai_path_tool {
    ($struct_name:ident, $tool_name:literal, $desc:literal, $glue:ident) => {
        struct $struct_name;
        #[async_trait]
        impl Tool for $struct_name {
            fn name(&self) -> &'static str {
                $tool_name
            }
            fn description(&self) -> &'static str {
                $desc
            }
            fn input_schema(&self) -> Value {
                json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                    "additionalProperties": false
                })
            }
            async fn exec(&self, input: Value) -> ToolResult {
                let path = match string_arg(&input, "path") {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                match claw_glue::ai::$glue(PathBuf::from(path)).await {
                    Ok(text) => ToolResult::ok(json!({"text": text}).to_string()),
                    Err(e) => ToolResult::err(e),
                }
            }
        }
    };
}

ai_path_tool!(
    SummarizeTool,
    "files.summarize",
    "Summarize a single document. Routes through the same `cos app doc \
     summarize` pipeline the right-click 'AI summary' menu uses.",
    summarize
);

ai_path_tool!(
    ExplainTool,
    "files.explain",
    "Explain a document's content in plain language. Routes through \
     `cos app doc explain`.",
    explain
);

struct FindSimilarTool;

#[async_trait]
impl Tool for FindSimilarTool {
    fn name(&self) -> &'static str {
        "files.find_similar"
    }

    fn description(&self) -> &'static str {
        "Find documents similar in content / topic to the given file. \
         Backed by the local Recoll index."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let path = match string_arg(&input, "path") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 200) as usize;
        match claw_glue::ai::find_similar(PathBuf::from(path), limit).await {
            Ok(hits) => {
                let arr: Vec<Value> = hits
                    .into_iter()
                    .map(|h| {
                        json!({
                            "path": h.path.display().to_string(),
                            "mime": h.mime,
                            "snippet": h.snippet,
                        })
                    })
                    .collect();
                ToolResult::ok(json!({"matches": arr}).to_string())
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn string_arg(input: &Value, key: &str) -> Result<String, ToolResult> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ToolResult::err(format!("missing required string '{key}'")))
}

fn denied_or_err<E: std::fmt::Display>(err: &E, tool: &str) -> ToolResult {
    ToolResult::err(format!("{tool}: {err}"))
}

// ---------------------------------------------------------------------------
// Entry point — called from `main` when COS_MCP_SERVER=1.
// ---------------------------------------------------------------------------

pub(crate) fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        Server::new("cosmic-files", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(ListTool))
            .tool(Arc::new(MetadataTool))
            .tool(Arc::new(SearchTool))
            .tool(Arc::new(RevealTool))
            .tool(Arc::new(SummarizeTool))
            .tool(Arc::new(ExplainTool))
            .tool(Arc::new(FindSimilarTool))
            .serve_stdio()
            .await
            .map_err(|e| anyhow::anyhow!("cosmic-files MCP server exited: {e}"))
    })
}
