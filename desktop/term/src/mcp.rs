//! MCP tool surface for `cosmic-term`.
//!
//! Headless command-execution. `COS_MCP_SERVER=1` skips libcosmic and
//! the alacritty PTY entirely. The kernel agent gets:
//!
//! - `term.run`  — run a single command, wait for exit, capture
//!                 stdout/stderr (bounded). For short-lived tasks.
//! - `term.shell` — run a string through `/bin/sh -c`.
//!
//! Caps gate on the kernel side: `proc.spawn` + `fs.exec` for the
//! resolved executable. Output captured up to `max_bytes` (default
//! 65 536, max 1 MiB) per stream.

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use cos_mcp_serve::{Server, Tool, ToolResult};
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time;

const DEFAULT_MAX_BYTES: usize = 64 * 1024;
const HARD_MAX_BYTES: usize = 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const HARD_TIMEOUT_SECS: u64 = 600;

async fn read_capped(mut r: impl tokio::io::AsyncRead + Unpin, max: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(max.min(8 * 1024));
    let mut chunk = [0u8; 4096];
    loop {
        if buf.len() >= max {
            // Drain remainder so child doesn't block on full pipe.
            let mut sink = [0u8; 4096];
            while r.read(&mut sink).await.unwrap_or(0) > 0 {}
            break;
        }
        match r.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => {
                let take = (max - buf.len()).min(n);
                buf.extend_from_slice(&chunk[..take]);
            }
            Err(_) => break,
        }
    }
    buf
}

async fn run_cmd(mut cmd: Command, max_bytes: usize, timeout_secs: u64) -> ToolResult {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("spawn: {e}")),
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_fut = async {
        match stdout {
            Some(s) => read_capped(s, max_bytes).await,
            None => Vec::new(),
        }
    };
    let stderr_fut = async {
        match stderr {
            Some(s) => read_capped(s, max_bytes).await,
            None => Vec::new(),
        }
    };
    let wait_fut = async {
        let status = child.wait().await;
        let (so, se) = tokio::join!(stdout_fut, stderr_fut);
        (status, so, se)
    };
    let timed = time::timeout(Duration::from_secs(timeout_secs), wait_fut).await;
    let (status, so, se) = match timed {
        Ok(t) => t,
        Err(_) => {
            return ToolResult::err(format!("timed out after {timeout_secs}s"));
        }
    };
    let status = match status {
        Ok(s) => s,
        Err(e) => return ToolResult::err(format!("wait: {e}")),
    };
    let stdout_str = String::from_utf8_lossy(&so).into_owned();
    let stderr_str = String::from_utf8_lossy(&se).into_owned();
    ToolResult::ok(
        json!({
            "exit_code": status.code(),
            "success": status.success(),
            "stdout": stdout_str,
            "stdout_bytes": so.len(),
            "stderr": stderr_str,
            "stderr_bytes": se.len(),
            "stdout_truncated": so.len() >= max_bytes,
            "stderr_truncated": se.len() >= max_bytes,
        })
        .to_string(),
    )
}

fn clamped_bytes(input: &Value) -> usize {
    let v = input
        .get("max_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_MAX_BYTES as u64) as usize;
    v.min(HARD_MAX_BYTES).max(256)
}

fn clamped_timeout(input: &Value) -> u64 {
    let v = input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_TIMEOUT_SECS);
    v.min(HARD_TIMEOUT_SECS).max(1)
}

struct RunTool;

#[async_trait]
impl Tool for RunTool {
    fn name(&self) -> &'static str {
        "term.run"
    }
    fn description(&self) -> &'static str {
        "Run `command` with `args` and return its exit code + captured \
         stdout/stderr. No shell expansion. Bounded by `timeout_secs` and \
         `max_bytes` per stream."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command":      { "type": "string", "minLength": 1 },
                "args":         { "type": "array", "items": { "type": "string" }, "default": [] },
                "cwd":          { "type": "string" },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": HARD_TIMEOUT_SECS, "default": DEFAULT_TIMEOUT_SECS },
                "max_bytes":    { "type": "integer", "minimum": 256, "maximum": HARD_MAX_BYTES, "default": DEFAULT_MAX_BYTES }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let command = match input.get("command").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing required field: command"),
        };
        let args: Vec<String> = input
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let mut cmd = Command::new(&command);
        cmd.args(&args);
        if let Some(cwd) = input.get("cwd").and_then(|v| v.as_str()) {
            cmd.current_dir(cwd);
        }
        run_cmd(cmd, clamped_bytes(&input), clamped_timeout(&input)).await
    }
}

struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "term.shell"
    }
    fn description(&self) -> &'static str {
        "Run `script` through `/bin/sh -c`. Useful for pipelines and \
         globs; subject to the same timeout / byte caps as term.run."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "script":       { "type": "string", "minLength": 1 },
                "cwd":          { "type": "string" },
                "timeout_secs": { "type": "integer", "minimum": 1, "maximum": HARD_TIMEOUT_SECS, "default": DEFAULT_TIMEOUT_SECS },
                "max_bytes":    { "type": "integer", "minimum": 256, "maximum": HARD_MAX_BYTES, "default": DEFAULT_MAX_BYTES }
            },
            "required": ["script"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let script = match input.get("script").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing required field: script"),
        };
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(&script);
        if let Some(cwd) = input.get("cwd").and_then(|v| v.as_str()) {
            cmd.current_dir(cwd);
        }
        run_cmd(cmd, clamped_bytes(&input), clamped_timeout(&input)).await
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        Server::new("cosmic-term", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(RunTool))
            .tool(Arc::new(ShellTool))
            .serve_stdio()
            .await
    })?;
    Ok(())
}
