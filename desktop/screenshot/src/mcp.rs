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

use async_trait::async_trait;
use claw_os_sdk::mcp::{Tool, ToolResult};
use serde_json::{Value, json};

use crate::{CaptureOptions, capture};

pub(crate) struct CaptureTool;

#[async_trait]
impl Tool for CaptureTool {
    fn name(&self) -> &'static str {
        "screenshot.capture"
    }

    fn description(&self) -> &'static str {
        "Capture the screen via xdg-desktop-portal. \
         Saves a PNG to `save_dir` (or the default Pictures directory) \
         and returns the absolute path. If the portal puts the image \
         on the clipboard instead, the returned path is empty."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "interactive": {
                    "type": "boolean",
                    "default": false,
                    "description": "Let the user pick a region via the portal UI."
                },
                "modal": {
                    "type": "boolean",
                    "default": true,
                    "description": "Render the portal as modal."
                },
                "save_dir": {
                    "type": "string",
                    "description": "Absolute path of the directory to save the PNG to. \
                                    Must already exist."
                }
            },
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
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

        match capture(opts).await {
            Ok(outcome) if outcome.cancelled => ToolResult::ok(
                json!({"cancelled": true, "path": null}).to_string(),
            ),
            Ok(outcome) => ToolResult::ok(
                json!({
                    "cancelled": false,
                    "path": if outcome.path.is_empty() { Value::Null } else { Value::String(outcome.path) }
                })
                .to_string(),
            ),
            Err(err) => ToolResult::err(format!("screenshot capture failed: {err}")),
        }
    }
}
