//! MCP tool surface for `cosmic-edit`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of opening an editor window. The agent gets the
//! same file-editing primitives the user has, going through the same
//! capability gate (`cos_runtime::fs::*`) and AI plumbing
//! (`cos app doc *`) — no shortcuts.
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
        let res = tokio::task::spawn_blocking(move || cos_runtime::fs::read(&path)).await;
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
        let res =
            tokio::task::spawn_blocking(move || cos_runtime::fs::write(&path, &content)).await;
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
        let res = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            let r = cos_runtime::fs::read(&path).map_err(|e| format!("read: {e}"))?;
            let body = r.content;
            let count = body.matches(&find).count();
            if count == 0 {
                return Err(format!("'find' not present in {path}"));
            }
            if count > 1 {
                return Err(format!(
                    "'find' is not unique in {path} ({count} matches); \
                     widen the context"
                ));
            }
            let new = body.replacen(&find, &replace, 1);
            cos_runtime::fs::write(&path, &new).map_err(|e| format!("write: {e}"))?;
            Ok(1)
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
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let argv: Vec<String> = match path {
            Some(p) => vec!["cosmic-edit".into(), p],
            None => vec!["cosmic-edit".into()],
        };
        let res = tokio::task::spawn_blocking(move || {
            let argv_b: Vec<&str> = argv.iter().map(String::as_str).collect();
            cos_runtime::exec::start(&argv_b)
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
        let result = claw_glue::ai::summarize(path.into()).await;
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
        let result = claw_glue::ai::explain(path.into()).await;
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
        let result = claw_glue::ai::rewrite(path.into(), instruction).await;
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
