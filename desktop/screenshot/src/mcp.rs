//! MCP tool surface for the kernel agent.
//!
//! When spawned with `COS_MCP_SERVER=1`, `cosmic-screenshot` flips
//! into a tiny MCP stdio server that exposes one tool —
//! `screenshot.capture` — backed by the same `capture()` flow the
//! CLI uses. The interactive portal still runs on a Wayland session,
//! so the agent normally calls this with `interactive: false` to
//! take an immediate full-screen capture and save it under
//! `~/Pictures` (or the supplied `save_dir`).

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{App, AppError, CallContext, Tool, ToolResult};
use serde_json::{json, Value};

use crate::{capture, CaptureOptions};

pub(crate) struct CaptureTool;

#[async_trait]
impl Tool for CaptureTool {
    fn name(&self) -> &'static str {
        "screenshot.capture"
    }

    async fn handle(&self, input: Value, context: CallContext) -> ToolResult {
        if let Err(error) = context.check_cancelled() {
            return ToolResult::error(error.to_string());
        }
        let interactive = input
            .get("interactive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let modal = input.get("modal").and_then(|v| v.as_bool()).unwrap_or(true);
        let save_dir = input
            .get("save_dir")
            .and_then(|v| v.as_str())
            .map(PathBuf::from);

        let opts = CaptureOptions {
            interactive,
            modal,
            save_dir,
        };

        let result = capture(opts).await;
        match result {
            Ok(outcome) if outcome.cancelled => ToolResult::text(
                json!({"cancelled": true, "path": null}).to_string(),
            ),
            Ok(outcome) => ToolResult::text(
                json!({
                    "cancelled": false,
                    "path": if outcome.path.is_empty() { Value::Null } else { Value::String(outcome.path) }
                })
                .to_string(),
            ),
            Err(err) => ToolResult::error(format!("screenshot capture failed: {err}")),
        }
    }
}

pub(crate) async fn run() -> Result<(), AppError> {
    let mut app = App::from_environment()?;
    app.bind(Arc::new(CaptureTool))?;
    app.serve_stdio().await
}
