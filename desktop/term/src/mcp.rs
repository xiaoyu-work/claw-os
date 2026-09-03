//! MCP tool surface for `cosmic-term`.
//!
//! When launched with `COS_MCP_SERVER=1`, the binary flips into a tiny
//! stdio MCP server instead of opening a terminal window. The agent
//! gets a controlled shell-execution path that goes through the same
//! capability gate as everything else (`cos_runtime::exec::*`), with
//! audit logging and timeout enforcement built in.
//!
//! `apps/cosmic-term/app.json` is authoritative for the tool catalog and
//! manifest-derived process and filesystem grants.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, CallContext, Tool, ToolResult};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// term.run — capability-gated single-shot command execution
// ---------------------------------------------------------------------------

struct RunTool;

#[async_trait]
impl Tool for RunTool {
    fn name(&self) -> &'static str {
        "term.run"
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let command = match input.get("command").and_then(|v| v.as_str()) {
            Some(command) => command.to_string(),
            None => return ToolResult::error("missing command"),
        };
        let arguments: Vec<String> = input
            .get("arguments")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut argv = Vec::with_capacity(arguments.len() + 1);
        argv.push(command);
        argv.extend(arguments);
        let timeout = input
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .clamp(1, 600) as u32;

        let res = tokio::task::spawn_blocking(move || {
            let argv_b: Vec<&str> = argv.iter().map(String::as_str).collect();
            cos_runtime::exec::run(&argv_b, Some(timeout))
        })
        .await;

        match res {
            Ok(Ok(r)) => ToolResult::text(
                json!({
                    "stdout": r.stdout,
                    "stderr": r.stderr,
                    "exit_code": r.exit_code,
                    "timed_out": r.timed_out,
                })
                .to_string(),
            ),
            Ok(Err(e)) => ToolResult::error(format!("term.run: {e}")),
            Err(e) => ToolResult::error(format!("term.run join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// term.which — capability-gated PATH lookup
// ---------------------------------------------------------------------------

struct WhichTool;

#[async_trait]
impl Tool for WhichTool {
    fn name(&self) -> &'static str {
        "term.which"
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let program = match input.get("program").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("missing program"),
        };
        let res = tokio::task::spawn_blocking(move || cos_runtime::exec::which(&program)).await;
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        match res {
            Ok(Ok(r)) => {
                let found = r.path.is_some();
                ToolResult::text(
                    json!({
                        "program": r.program,
                        "path": r.path,
                        "found": found,
                    })
                    .to_string(),
                )
            }
            Ok(Err(e)) => ToolResult::error(format!("term.which: {e}")),
            Err(e) => ToolResult::error(format!("term.which join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// term.open — spawn a real interactive cosmic-term window at `cwd`
// ---------------------------------------------------------------------------

struct OpenTool;

#[async_trait]
impl Tool for OpenTool {
    fn name(&self) -> &'static str {
        "term.open"
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let cwd = input
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let argv: Vec<String> = match cwd {
            Some(d) => vec!["cosmic-term".into(), "--working-directory".into(), d],
            None => vec!["cosmic-term".into()],
        };
        let res = tokio::task::spawn_blocking(move || {
            let argv_b: Vec<&str> = argv.iter().map(String::as_str).collect();
            cos_runtime::exec::start(&argv_b)
        })
        .await;
        match res {
            Ok(Ok(_)) => ToolResult::text(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::error(format!("term.open: {e}")),
            Err(e) => ToolResult::error(format!("term.open join: {e}")),
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
        app.bind(Arc::new(RunTool))?;
        app.bind(Arc::new(WhichTool))?;
        app.bind(Arc::new(OpenTool))?;
        app.serve_stdio().await
    })
    .map_err(|error| anyhow::anyhow!("cosmic-term MCP server exited: {error}"))
}
