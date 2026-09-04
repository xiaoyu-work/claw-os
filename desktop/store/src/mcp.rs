//! MCP tool surface for `cosmic-store`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of opening the store GUI. Tools route through the
//! kernel-level `apps/pkg` (capability-gated `apt`/`dpkg` wrapper)
//! plus a thin `cosmic-store` GUI launcher. The agent never shells
//! out to `apt` directly.
//!
//! The authoritative tool catalog and capability contract live in
//! `apps/cosmic-store/app.json`.
//!
//! Install/remove are intentionally **not** exposed: those are
//! impactful operations that should go through the kernel-level
//! approval flow (`cos app pkg need / install`) — the agent should
//! emit a "need" call, the user reviews it in the approval gate, and
//! the kernel performs the install. Wiring those into MCP would
//! short-circuit that flow.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::{json, Value};

fn cos_bin() -> String {
    std::env::var("CLAW_COS_BIN").unwrap_or_else(|_| "cos".into())
}

async fn invoke_app(app_id: &str, op: &str, extra: &[&str]) -> Result<Value, String> {
    let bin = cos_bin();
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(["app", app_id, op]);
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
// store.search
// ---------------------------------------------------------------------------

struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "store.search"
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
            .unwrap_or(25)
            .clamp(1, 100)
            .to_string();
        let result = invoke_app("pkg", "search", &["--query", &query, "--limit", &limit]).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match result {
            Ok(v) => ToolResult::text(v.to_string()),
            Err(e) => ToolResult::error(format!("store.search: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// store.installed
// ---------------------------------------------------------------------------

struct InstalledTool;

#[async_trait]
impl Tool for InstalledTool {
    fn name(&self) -> &'static str {
        "store.installed"
    }
    async fn handle(&self, _input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let result = invoke_app("pkg", "list", &[]).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match result {
            Ok(v) => ToolResult::text(v.to_string()),
            Err(e) => ToolResult::error(format!("store.installed: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// store.show
// ---------------------------------------------------------------------------

struct ShowTool;

#[async_trait]
impl Tool for ShowTool {
    fn name(&self) -> &'static str {
        "store.show"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("missing name"),
        };
        let result = invoke_app("pkg", "show", &["--name", &name]).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match result {
            Ok(v) => ToolResult::text(v.to_string()),
            Err(e) => ToolResult::error(format!("store.show: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// store.open — launch cosmic-store, optionally deep-linked
// ---------------------------------------------------------------------------

struct OpenTool;

#[async_trait]
impl Tool for OpenTool {
    fn name(&self) -> &'static str {
        "store.open"
    }
    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let argv: Vec<String> = match name {
            Some(n) => vec!["cosmic-store".into(), n],
            None => vec!["cosmic-store".into()],
        };
        let res = tokio::task::spawn_blocking(move || {
            let argv_b: Vec<&str> = argv.iter().map(String::as_str).collect();
            cos_runtime::exec::start(&argv_b)
        })
        .await;
        match res {
            Ok(Ok(_)) => ToolResult::text(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::error(format!("store.open: {e}")),
            Err(e) => ToolResult::error(format!("store.open join: {e}")),
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
        app.bind(Arc::new(SearchTool))?;
        app.bind(Arc::new(InstalledTool))?;
        app.bind(Arc::new(ShowTool))?;
        app.bind(Arc::new(OpenTool))?;
        app.serve_stdio().await
    })
    .map_err(|error| anyhow::anyhow!("cosmic-store MCP server exited: {error}"))
}
