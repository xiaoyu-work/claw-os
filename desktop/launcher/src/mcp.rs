//! MCP tool surface for `cosmic-launcher`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of showing the on-screen launcher. Every tool is a
//! thin wrapper over the kernel `apps/launcher` so capability gating
//! (`desktop.launch name:<app_id>`) and the launch-history journal
//! apply uniformly whether the user picks an app from the panel or
//! the agent does it via tool call.
//!
//! Tools:
//!   - `launcher.find(query, limit?)` — fuzzy AppID search
//!   - `launcher.list()`              — every installed .desktop entry
//!   - `launcher.open(app_id, extras?)` — launch an app
//!   - `launcher.recent(limit?)`      — recent launches log

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{Server, Tool, ToolResult};
use serde_json::{Value, json};

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
    fn description(&self) -> &'static str {
        "Fuzzy search the installed GUI applications by name, comment, \
         categories or keywords. Returns a ranked list of AppIDs the \
         agent can hand to `launcher.open`."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "default": 10}
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let query = match input.get("query").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing query"),
        };
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 50)
            .to_string();
        match invoke_app("find", &["--limit", &limit, &query]).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::err(format!("launcher.find: {e}")),
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
    fn description(&self) -> &'static str {
        "List every installed .desktop entry (visible, non-hidden by \
         default). Useful for grounding 'what apps are installed?' \
         questions when fuzzy search would be too lossy."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "include_hidden": {"type": "boolean", "default": false}
            },
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let extra: Vec<&str> = if input
            .get("include_hidden")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            vec!["--include-hidden"]
        } else {
            vec![]
        };
        match invoke_app("list", &extra).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::err(format!("launcher.list: {e}")),
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
    fn description(&self) -> &'static str {
        "Launch an installed application by its `.desktop` AppID \
         (e.g. `com.clawos.Files`). The kernel mediates the launch \
         through `desktop.launch name:<app_id>` — granting that cap \
         is the user's choice. Optional `extras` are passed verbatim \
         to the entry's `Exec=` line (file/URI substitutions only)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app_id": {"type": "string", "minLength": 1},
                "extras": {
                    "type": "array",
                    "items": {"type": "string"},
                    "default": []
                }
            },
            "required": ["app_id"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let app_id = match input.get("app_id").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing app_id"),
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
        match invoke_app("open", &args_b).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::err(format!("launcher.open: {e}")),
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
    fn description(&self) -> &'static str {
        "Read the recent-launch journal so the agent can ground \
         'what was I just working on?' or 'open it again'."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "default": 20}
            },
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let limit = input
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 200)
            .to_string();
        match invoke_app("recent", &["--limit", &limit]).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::err(format!("launcher.recent: {e}")),
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
        Server::new("cosmic-launcher", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(FindTool))
            .tool(Arc::new(ListTool))
            .tool(Arc::new(OpenTool))
            .tool(Arc::new(RecentTool))
            .serve_stdio()
            .await
            .map_err(|e| anyhow::anyhow!("cosmic-launcher MCP server exited: {e}"))
    })
}
