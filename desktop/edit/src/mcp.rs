//! MCP tool surface for `cosmic-edit`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of opening an editor window. The agent gets the
//! same file-editing primitives the user has, going through the same
//! controlled filesystem/desktop services and SDK AI gate — no App calls.
//!
//! `apps/cosmic-edit/app.json` is the sole authority for tool descriptions,
//! arguments, defaults, and capability needs.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::{json, Value};

use crate::claw_glue;

fn req_str(input: &Value, field: &str) -> Result<String, ToolResult> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ToolResult::error(format!("missing string field '{field}'")))
}

// ---------------------------------------------------------------------------
// edit.read
// ---------------------------------------------------------------------------

struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "edit.read"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let worker_context = context.clone();
        let res = tokio::task::spawn_blocking(move || {
            worker_context
                .check_cancelled()
                .map_err(|e| e.to_string())?;
            cos_runtime::filesystem::read(&path).map_err(|e| e.to_string())
        })
        .await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match res {
            Ok(Ok(r)) => {
                ToolResult::text(json!({ "path": r.path, "content": r.content }).to_string())
            }
            Ok(Err(e)) => ToolResult::error(format!("edit.read: {e}")),
            Err(e) => ToolResult::error(format!("edit.read join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.write
// ---------------------------------------------------------------------------

struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "edit.write"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let content = match req_str(&input, "content") {
            Ok(c) => c,
            Err(e) => return e,
        };
        let res = tokio::task::spawn_blocking(move || {
            context.check_cancelled().map_err(|e| e.to_string())?;
            cos_runtime::filesystem::write(&path, &content).map_err(|e| e.to_string())
        })
        .await;
        match res {
            Ok(Ok(_)) => ToolResult::text(json!({"written": true}).to_string()),
            Ok(Err(e)) => ToolResult::error(format!("edit.write: {e}")),
            Err(e) => ToolResult::error(format!("edit.write join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.replace_range — substring find-and-replace
// ---------------------------------------------------------------------------

struct ReplaceRangeTool;

#[async_trait]
impl Tool for ReplaceRangeTool {
    fn name(&self) -> &'static str {
        "edit.replace_range"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let find = match req_str(&input, "find") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let replace = match req_str(&input, "replace") {
            Ok(p) => p,
            Err(e) => return e,
        };
        if find.is_empty() {
            return ToolResult::error("'find' must not be empty");
        }
        let res = tokio::task::spawn_blocking(move || {
            context.check_cancelled().map_err(|e| e.to_string())?;
            cos_runtime::filesystem::replace(&path, &find, &replace).map_err(|e| e.to_string())
        })
        .await;
        match res {
            Ok(Ok(n)) => ToolResult::text(json!({"replacements": n}).to_string()),
            Ok(Err(e)) => ToolResult::error(format!("edit.replace_range: {e}")),
            Err(e) => ToolResult::error(format!("edit.replace_range join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.open — spawn an interactive editor window
// ---------------------------------------------------------------------------

struct OpenTool;

#[async_trait]
impl Tool for OpenTool {
    fn name(&self) -> &'static str {
        "edit.open"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match input.get("path") {
            None => None,
            Some(Value::String(path)) => Some(path.clone()),
            Some(_) => return ToolResult::error("path must be a string when supplied"),
        };
        let res = tokio::task::spawn_blocking(move || {
            context.check_cancelled().map_err(|e| e.to_string())?;
            cos_runtime::desktop::open_editor(path.as_deref()).map_err(|e| e.to_string())
        })
        .await;
        match res {
            Ok(Ok(_)) => ToolResult::text(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::error(format!("edit.open: {e}")),
            Err(e) => ToolResult::error(format!("edit.open join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.summarize / edit.explain / edit.rewrite
// ---------------------------------------------------------------------------

struct SummarizeTool;

#[async_trait]
impl Tool for SummarizeTool {
    fn name(&self) -> &'static str {
        "edit.summarize"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let result = claw_glue::ai::transform(path, "summarize", None, context).await;
        match result {
            Ok(s) => ToolResult::text(json!({"summary": s}).to_string()),
            Err(e) => ToolResult::error(format!("edit.summarize: {e}")),
        }
    }
}

struct ExplainTool;

#[async_trait]
impl Tool for ExplainTool {
    fn name(&self) -> &'static str {
        "edit.explain"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let result = claw_glue::ai::transform(path, "explain", None, context).await;
        match result {
            Ok(s) => ToolResult::text(json!({"text": s}).to_string()),
            Err(e) => ToolResult::error(format!("edit.explain: {e}")),
        }
    }
}

struct RewriteTool;

#[async_trait]
impl Tool for RewriteTool {
    fn name(&self) -> &'static str {
        "edit.rewrite"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let instruction = match req_str(&input, "instruction") {
            Ok(i) => i,
            Err(e) => return e,
        };
        let result = claw_glue::ai::transform(path, "rewrite", Some(instruction), context).await;
        match result {
            Ok(s) => ToolResult::text(json!({"text": s}).to_string()),
            Err(e) => ToolResult::error(format!("edit.rewrite: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub(crate) fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut app = App::from_environment()?;
        app.bind(Arc::new(ReadTool))?;
        app.bind(Arc::new(WriteTool))?;
        app.bind(Arc::new(ReplaceRangeTool))?;
        app.bind(Arc::new(OpenTool))?;
        app.bind(Arc::new(SummarizeTool))?;
        app.bind(Arc::new(ExplainTool))?;
        app.bind(Arc::new(RewriteTool))?;
        app.serve_stdio().await
    })
    .map_err(|error| anyhow::anyhow!("cosmic-edit MCP server exited: {error}"))
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/mcp.rs"));
}
