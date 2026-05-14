//! MCP tool surface for `cosmic-edit`.
//!
//! Headless text-editor tools. `COS_MCP_SERVER=1` bypasses fork +
//! libcosmic entirely so the kernel agent can call:
//!
//! - `edit.read_file`  — return UTF-8 contents of a file
//! - `edit.write_file` — replace file contents atomically
//! - `edit.append`     — append a string to a file (creates if absent)
//!
//! Note: this is the *headless* surface. The interactive editor still
//! launches the libcosmic GUI when `COS_MCP_SERVER` is unset.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use cos_mcp_serve::{Server, Tool, ToolResult};
use serde_json::{Value, json};

const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

fn read_text(path: &Path) -> Result<String, String> {
    let md = std::fs::metadata(path).map_err(|e| format!("stat `{}`: {e}", path.display()))?;
    if !md.is_file() {
        return Err(format!("`{}` is not a regular file", path.display()));
    }
    if md.len() > MAX_READ_BYTES {
        return Err(format!(
            "`{}` is {} bytes; limit is {} bytes",
            path.display(),
            md.len(),
            MAX_READ_BYTES
        ));
    }
    std::fs::read_to_string(path).map_err(|e| format!("read `{}`: {e}", path.display()))
}

fn write_atomic(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("`{}` has no parent directory", path.display()))?;
    if !parent.as_os_str().is_empty() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("mkdir `{}`: {e}", parent.display()))?;
    }
    let tmp = path.with_extension(format!(
        "{}.cos-mcp.tmp",
        path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    std::fs::write(&tmp, content).map_err(|e| format!("write tmp `{}`: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| format!("rename `{}` -> `{}`: {e}", tmp.display(), path.display()))?;
    Ok(())
}

struct ReadFileTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &'static str {
        "edit.read_file"
    }
    fn description(&self) -> &'static str {
        "Read a UTF-8 text file (≤ 4 MiB). Returns {path, bytes, content}."
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
        match read_text(&path) {
            Ok(content) => ToolResult::ok(
                json!({
                    "path": path.to_string_lossy(),
                    "bytes": content.len(),
                    "content": content,
                })
                .to_string(),
            ),
            Err(e) => ToolResult::err(e),
        }
    }
}

struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &'static str {
        "edit.write_file"
    }
    fn description(&self) -> &'static str {
        "Atomically replace the contents of `path` with `content` (creates \
         parents as needed). UTF-8 only."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path":    { "type": "string", "minLength": 1 },
                "content": { "type": "string" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => return ToolResult::err("missing required field: path"),
        };
        let content = match input.get("content").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing required field: content"),
        };
        match write_atomic(&path, &content) {
            Ok(()) => ToolResult::ok(
                json!({
                    "path": path.to_string_lossy(),
                    "bytes": content.len(),
                })
                .to_string(),
            ),
            Err(e) => ToolResult::err(e),
        }
    }
}

struct AppendTool;

#[async_trait]
impl Tool for AppendTool {
    fn name(&self) -> &'static str {
        "edit.append"
    }
    fn description(&self) -> &'static str {
        "Append `content` to `path`. Creates the file (and parents) if absent."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path":    { "type": "string", "minLength": 1 },
                "content": { "type": "string" }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        use std::io::Write;
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => return ToolResult::err("missing required field: path"),
        };
        let content = match input.get("content").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing required field: content"),
        };
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return ToolResult::err(format!("mkdir `{}`: {e}", parent.display()));
                }
            }
        }
        let mut file = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => return ToolResult::err(format!("open `{}`: {e}", path.display())),
        };
        if let Err(e) = file.write_all(content.as_bytes()) {
            return ToolResult::err(format!("append `{}`: {e}", path.display()));
        }
        ToolResult::ok(
            json!({
                "path": path.to_string_lossy(),
                "appended_bytes": content.len(),
            })
            .to_string(),
        )
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        Server::new("cosmic-edit", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(ReadFileTool))
            .tool(Arc::new(WriteFileTool))
            .tool(Arc::new(AppendTool))
            .serve_stdio()
            .await
    })?;
    Ok(())
}
