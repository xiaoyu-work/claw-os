//! MCP tool surface for `cosmic-launcher`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of showing the on-screen launcher. Every tool is a
//! thin wrapper over the kernel `apps/launcher` so capability gating
//! (`desktop.launch name:<app_id>`) and the launch-history journal
//! apply uniformly whether the user picks an app from the panel or
//! the agent does it via tool call.
//!
//! `apps/cosmic-launcher/app.json` owns the native tool descriptions,
//! arguments, defaults, and per-call capability needs.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::Value;

fn cos_bin() -> String {
    std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into())
}

async fn invoke_app(op: &str, extra: &[&str]) -> Result<Value, String> {
    let bin = cos_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(["app", "launcher", op]);
    cmd.args(extra);
    let output = cmd
        .output()
        .await
        .map_err(|e| format!("failed to invoke {bin}: {e}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cos produced no output ({})\n{}",
            output.status, stderr
        ));
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("bad JSON from cos: {e}\n---\n{trimmed}"))?;
    if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
        return Err(err.to_string());
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// launcher.find
// ---------------------------------------------------------------------------

struct FindTool;

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &'static str {
        "launcher.find"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let query = match input.get("query").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("missing query"),
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 50)
            .to_string();
        let result = invoke_app("find", &["--limit", &limit, &query]).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match result {
            Ok(v) => ToolResult::text(v.to_string()),
            Err(e) => ToolResult::error(format!("launcher.find: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// launcher.list
// ---------------------------------------------------------------------------

struct ListTool;

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &'static str {
        "launcher.list"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let extra: Vec<&str> = if input
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            vec!["--include-hidden"]
        } else {
            vec![]
        };
        let result = invoke_app("list", &extra).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match result {
            Ok(v) => ToolResult::text(v.to_string()),
            Err(e) => ToolResult::error(format!("launcher.list: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// launcher.open
// ---------------------------------------------------------------------------

struct OpenTool;

#[async_trait]
impl Tool for OpenTool {
    fn name(&self) -> &'static str {
        "launcher.open"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let app_id = match input.get("app_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("missing app_id"),
        };
        let extras: Vec<String> = input
            .get("extras")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let mut args: Vec<String> = vec![app_id];
        args.extend(extras);
        let args_b: Vec<&str> = args.iter().map(String::as_str).collect();
        let result = invoke_app("open", &args_b).await;
        match result {
            Ok(v) => ToolResult::text(v.to_string()),
            Err(e) => ToolResult::error(format!("launcher.open: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// launcher.recent
// ---------------------------------------------------------------------------

struct RecentTool;

#[async_trait]
impl Tool for RecentTool {
    fn name(&self) -> &'static str {
        "launcher.recent"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 200)
            .to_string();
        let result = invoke_app("recent", &["--limit", &limit]).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match result {
            Ok(v) => ToolResult::text(v.to_string()),
            Err(e) => ToolResult::error(format!("launcher.recent: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn run() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        let mut app = App::from_environment()?;
        app.bind(Arc::new(FindTool))?;
        app.bind(Arc::new(ListTool))?;
        app.bind(Arc::new(OpenTool))?;
        app.bind(Arc::new(RecentTool))?;
        app.serve_stdio().await
    })
    .map_err(|error| anyhow::anyhow!("cosmic-launcher MCP server exited: {error}"))
}
