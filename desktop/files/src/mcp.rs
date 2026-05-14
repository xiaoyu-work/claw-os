//! MCP tool surface for `cosmic-files`.
//!
//! Skips libcosmic when `COS_MCP_SERVER=1`. The MCP server exposes
//! headless file-system reads:
//!
//! - `files.list` — directory entries with type/size/mtime
//! - `files.search` — recursive name match (respects gitignore)
//! - `files.metadata` — single-entry stat
//!
//! Writes intentionally **omitted** from this surface: the agent
//! already has `fs.write` at the kernel level (capability-gated and
//! audited). The files App's role here is structured discovery.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use cos_mcp_serve::{Server, Tool, ToolResult};
use ignore::WalkBuilder;
use serde_json::{Value, json};

fn entry_metadata(path: &Path) -> Value {
    let md = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) => {
            return json!({
                "path": path.to_string_lossy(),
                "error": format!("stat: {e}")
            });
        }
    };
    let kind = if md.is_dir() {
        "dir"
    } else if md.is_symlink() {
        "symlink"
    } else if md.is_file() {
        "file"
    } else {
        "other"
    };
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    json!({
        "path": path.to_string_lossy(),
        "name": path.file_name().map(|s| s.to_string_lossy().to_string()),
        "kind": kind,
        "size": if md.is_file() { Some(md.len()) } else { None },
        "mtime_unix": mtime,
    })
}

struct ListTool;

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &'static str {
        "files.list"
    }
    fn description(&self) -> &'static str {
        "List entries directly under `path` (non-recursive). Returns array \
         of {path, name, kind, size, mtime_unix} objects."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "include_hidden": { "type": "boolean", "default": false }
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => return ToolResult::err("missing required field: path"),
        };
        let include_hidden = input
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let read = match std::fs::read_dir(&path) {
            Ok(r) => r,
            Err(e) => return ToolResult::err(format!("read_dir `{}`: {e}", path.display())),
        };
        let mut entries = Vec::new();
        for entry in read.flatten() {
            let p = entry.path();
            if !include_hidden {
                if let Some(name) = p.file_name() {
                    if name.to_string_lossy().starts_with('.') {
                        continue;
                    }
                }
            }
            entries.push(entry_metadata(&p));
        }
        ToolResult::ok(json!({ "count": entries.len(), "entries": entries }).to_string())
    }
}

struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "files.search"
    }
    fn description(&self) -> &'static str {
        "Recursively walk `root` and return entries whose filename matches \
         the case-insensitive substring `query`. Respects .gitignore by \
         default. Bounded by `limit` (max 5000)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "root":  { "type": "string", "minLength": 1 },
                "query": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 5000, "default": 200 },
                "respect_gitignore": { "type": "boolean", "default": true }
            },
            "required": ["root", "query"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let root = match input.get("root").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => PathBuf::from(s),
            _ => return ToolResult::err("missing required field: root"),
        };
        let query = match input.get("query").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_lowercase(),
            _ => return ToolResult::err("missing required field: query"),
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(200) as usize;
        let respect = input
            .get("respect_gitignore")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let mut wb = WalkBuilder::new(&root);
        wb.standard_filters(respect).hidden(respect);
        let mut hits = Vec::new();
        for result in wb.build() {
            if hits.len() >= limit {
                break;
            }
            let entry = match result {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name_lc = path
                .file_name()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if name_lc.contains(&query) {
                hits.push(entry_metadata(path));
            }
        }
        ToolResult::ok(json!({ "count": hits.len(), "matches": hits }).to_string())
    }
}

struct MetadataTool;

#[async_trait]
impl Tool for MetadataTool {
    fn name(&self) -> &'static str {
        "files.metadata"
    }
    fn description(&self) -> &'static str {
        "Return metadata for a single path: {path, name, kind, size, mtime_unix}."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string", "minLength": 1 } },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => return ToolResult::err("missing required field: path"),
        };
        ToolResult::ok(entry_metadata(&path).to_string())
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        Server::new("cosmic-files", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(ListTool))
            .tool(Arc::new(SearchTool))
            .tool(Arc::new(MetadataTool))
            .serve_stdio()
            .await
    })?;
    Ok(())
}
