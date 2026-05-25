//! MCP tool surface for `cosmic-term`.
//!
//! When launched with `COS_MCP_SERVER=1`, the binary flips into a tiny
//! stdio MCP server instead of opening a terminal window. The agent
//! gets a controlled shell-execution path that goes through the same
//! capability gate as everything else (`cos_runtime::exec::*`), with
//! audit logging and timeout enforcement built in.
//!
//! Every command the agent runs through `term.run` is **traceable in
//! the kernel audit log** — there is no way to bypass it. That's the
//! whole point of routing terminal exec through this server instead
//! of letting the agent shell out directly.

use std::sync::Arc;

use async_trait::async_trait;
use cos_mcp_serve::{Server, Tool, ToolResult};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------
// term.run — capability-gated single-shot command execution
// ---------------------------------------------------------------------------

struct RunTool;

#[async_trait]
impl Tool for RunTool {
    fn name(&self) -> &'static str {
        "term.run"
    }

    fn description(&self) -> &'static str {
        "Run a single shell command, capture stdout/stderr, exit code \
         and timeout state. Goes through `cos app exec run` so the \
         kernel's capability gate and audit log apply. Use this instead \
         of asking the user to copy-paste a command into a terminal."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "argv": {
                    "type": "array",
                    "items": {"type": "string"},
                    "minItems": 1,
                    "description": "Program + arguments. The first element is the binary."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600,
                    "default": 30,
                    "description": "Kill the process after this many seconds."
                }
            },
            "required": ["argv"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let argv: Vec<String> = match input
            .get("argv")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            }) {
            Some(v) if !v.is_empty() => v,
            _ => return ToolResult::err("argv must be a non-empty array of strings"),
        };
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
            Ok(Ok(r)) => ToolResult::ok(
                json!({
                    "stdout": r.stdout,
                    "stderr": r.stderr,
                    "exit_code": r.exit_code,
                    "timed_out": r.timed_out,
                })
                .to_string(),
            ),
            Ok(Err(e)) => ToolResult::err(format!("term.run: {e}")),
            Err(e) => ToolResult::err(format!("term.run join: {e}")),
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

    fn description(&self) -> &'static str {
        "Resolve a program name to its absolute path on $PATH. \
         Use this before suggesting commands to confirm the binary \
         exists on the user's system."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "program": {"type": "string"}
            },
            "required": ["program"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let program = match input.get("program").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing program"),
        };
        let res = tokio::task::spawn_blocking(move || cos_runtime::exec::which(&program)).await;
        match res {
            Ok(Ok(r)) => {
                let found = r.path.is_some();
                ToolResult::ok(
                    json!({
                        "program": r.program,
                        "path": r.path,
                        "found": found,
                    })
                    .to_string(),
                )
            }
            Ok(Err(e)) => ToolResult::err(format!("term.which: {e}")),
            Err(e) => ToolResult::err(format!("term.which join: {e}")),
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

    fn description(&self) -> &'static str {
        "Open an interactive terminal window, optionally cd'd into a \
         working directory. Use this to hand control back to the user \
         after the agent has finished a long-running operation in the \
         background, e.g. 'I've finished cloning the repo — opening a \
         terminal in /home/cos/proj for you.'"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "cwd": {
                    "type": "string",
                    "description": "Initial working directory. Absolute path."
                }
            },
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
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
            Ok(Ok(_)) => ToolResult::ok(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::err(format!("term.open: {e}")),
            Err(e) => ToolResult::err(format!("term.open join: {e}")),
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
        Server::new("cosmic-term", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(RunTool))
            .tool(Arc::new(WhichTool))
            .tool(Arc::new(OpenTool))
            .serve_stdio()
            .await
            .map_err(|e| anyhow::anyhow!("cosmic-term MCP server exited: {e}"))
    })
}
