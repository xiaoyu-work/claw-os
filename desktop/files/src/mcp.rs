//! MCP tool surface for `cosmic-files`.
//!
//! When launched with `COS_MCP_SERVER=1`, the binary flips into a tiny
//! stdio MCP server instead of opening a window. The agent calls these
//! tools so it can navigate, inspect and act on the user's filesystem
//! without having to spawn an interactive Files window first.
//!
//! The authoritative tool catalog, arguments, and capability needs live in
//! `apps/cosmic-files/app.json`. Implementations reuse the GUI's guarded
//! runtime and AI helpers where those paths exist; recursive search executes
//! inside the App Host sandbox under the manifest-derived filesystem grant.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::{json, Value};
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

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
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
                ToolResult::text(json!({ "path": r.path, "entries": entries }).to_string())
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

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match string_arg(&input, "path") {
            Ok(v) => v,
            Err(e) => return e,
        };
        match cos_runtime::fs::stat(&path) {
            Ok(s) => ToolResult::text(
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

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
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

        let search_context = context.clone();
        let results = tokio::task::spawn_blocking(move || -> Result<Vec<Value>, String> {
            let mut hits: Vec<Value> = Vec::new();
            for entry in WalkDir::new(&root)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    if include_hidden {
                        return true;
                    }
                    e.file_name()
                        .to_str()
                        .map(|s| !s.starts_with('.'))
                        .unwrap_or(true)
                })
            {
                search_context
                    .check_cancelled()
                    .map_err(|error| error.to_string())?;
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
            Ok(hits)
        })
        .await;

        let results = match results {
            Ok(Ok(results)) => results,
            Ok(Err(error)) => return ToolResult::error(error),
            Err(error) => return ToolResult::error(format!("files.search join: {error}")),
        };

        ToolResult::text(json!({ "matches": results }).to_string())
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

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
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
            Ok(_) => ToolResult::text(json!({"opened": dir_str}).to_string()),
            Err(e) => denied_or_err(&e, "files.reveal"),
        }
    }
}

// ---------------------------------------------------------------------------
// AI proxies — reuse the right-click menu's pipeline so the agent and
// the user share the same audit trail.
// ---------------------------------------------------------------------------

macro_rules! ai_path_tool {
    ($struct_name:ident, $tool_name:literal, $glue:ident) => {
        struct $struct_name;
        #[async_trait]
        impl Tool for $struct_name {
            fn name(&self) -> &'static str {
                $tool_name
            }
            async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
                if let Err(error) = context.check_cancelled() {
                    return ToolResult::error(error.to_string());
                }
                let path = match string_arg(&input, "path") {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                let result = claw_glue::ai::$glue(PathBuf::from(path)).await;
                match result {
                    Ok(text) => ToolResult::text(json!({"text": text}).to_string()),
                    Err(e) => ToolResult::error(e),
                }
            }
        }
    };
}

ai_path_tool!(SummarizeTool, "files.summarize", summarize);

ai_path_tool!(ExplainTool, "files.explain", explain);

struct FindSimilarTool;

#[async_trait]
impl Tool for FindSimilarTool {
    fn name(&self) -> &'static str {
        "files.find_similar"
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match string_arg(&input, "path") {
            Ok(v) => v,
            Err(e) => return e,
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 200) as usize;
        let result = claw_glue::ai::find_similar(PathBuf::from(path), limit).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match result {
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
                ToolResult::text(json!({"matches": arr}).to_string())
            }
            Err(e) => ToolResult::error(e),
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
        .ok_or_else(|| ToolResult::error(format!("missing required string '{key}'")))
}

fn denied_or_err<E: std::fmt::Display>(err: &E, tool: &str) -> ToolResult {
    ToolResult::error(format!("{tool}: {err}"))
}

// ---------------------------------------------------------------------------
// Entry point — called from `main` when COS_MCP_SERVER=1.
// ---------------------------------------------------------------------------

pub(crate) fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut app = App::from_environment()?;
        app.bind(Arc::new(ListTool))?;
        app.bind(Arc::new(MetadataTool))?;
        app.bind(Arc::new(SearchTool))?;
        app.bind(Arc::new(RevealTool))?;
        app.bind(Arc::new(SummarizeTool))?;
        app.bind(Arc::new(ExplainTool))?;
        app.bind(Arc::new(FindSimilarTool))?;
        app.serve_stdio().await
    })
    .map_err(|error| anyhow::anyhow!("cosmic-files MCP server exited: {error}"))
}
