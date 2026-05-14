//! MCP tool surface for `cosmic-store`.
//!
//! Like the other desktop binaries, we do not initialise libcosmic
//! when `COS_MCP_SERVER=1`. The store's full backend (appstream
//! cache, flatpak/packagekit clients, etc.) is heavyweight; for the
//! initial agent-facing surface we shell out to the **same package
//! tools the GUI uses underneath** (`dpkg-query`, `apt-cache`,
//! `flatpak`) so the MCP path adds minimal new code and matches
//! whatever the user already has installed.
//!
//! Caller convention: every tool prints its output as JSON in
//! `ToolResult::ok`; errors (non-zero exit, missing binary) come
//! back as `ToolResult::err` so the agent can react.

use std::sync::Arc;

use async_trait::async_trait;
use cos_mcp_serve::{Server, Tool, ToolResult};
use serde_json::{Value, json};
use tokio::process::Command;

async fn run_cmd(prog: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(prog)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("spawn `{prog}`: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`{prog}` exited {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("decode stdout: {e}"))
}

struct ListInstalledTool;

#[async_trait]
impl Tool for ListInstalledTool {
    fn name(&self) -> &'static str {
        "store.list_installed"
    }
    fn description(&self) -> &'static str {
        "List dpkg-installed packages. Returns an array of package names \
         (newline-separated values from `dpkg-query -W`)."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {}, "additionalProperties": false })
    }
    async fn exec(&self, _input: Value) -> ToolResult {
        match run_cmd("dpkg-query", &["-W", "-f=${binary:Package}\n"]).await {
            Ok(out) => {
                let names: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
                ToolResult::ok(json!({ "count": names.len(), "packages": names }).to_string())
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

struct SearchTool;

#[async_trait]
impl Tool for SearchTool {
    fn name(&self) -> &'static str {
        "store.search"
    }
    fn description(&self) -> &'static str {
        "Search the apt cache for packages matching `query`. Returns an array \
         of {name, summary} objects (parsed from `apt-cache search`)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let query = match input.get("query").and_then(|v| v.as_str()) {
            Some(q) if !q.is_empty() => q,
            _ => return ToolResult::err("missing required field: query"),
        };
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        match run_cmd("apt-cache", &["search", "--names-only", query]).await {
            Ok(out) => {
                let mut results = Vec::new();
                for line in out.lines().take(limit) {
                    if let Some((name, summary)) = line.split_once(" - ") {
                        results.push(json!({"name": name.trim(), "summary": summary.trim()}));
                    } else if !line.is_empty() {
                        results.push(json!({"name": line.trim(), "summary": ""}));
                    }
                }
                ToolResult::ok(json!({ "count": results.len(), "results": results }).to_string())
            }
            Err(e) => ToolResult::err(e),
        }
    }
}

struct ShowTool;

#[async_trait]
impl Tool for ShowTool {
    fn name(&self) -> &'static str {
        "store.show"
    }
    fn description(&self) -> &'static str {
        "Return apt-cache show output for `name` (description, version, dependencies)."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "name": { "type": "string", "minLength": 1 } },
            "required": ["name"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n,
            _ => return ToolResult::err("missing required field: name"),
        };
        match run_cmd("apt-cache", &["show", name]).await {
            Ok(out) => ToolResult::ok(json!({ "info": out }).to_string()),
            Err(e) => ToolResult::err(e),
        }
    }
}

pub(crate) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        Server::new("cosmic-store", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(ListInstalledTool))
            .tool(Arc::new(SearchTool))
            .tool(Arc::new(ShowTool))
            .serve_stdio()
            .await
    })?;
    Ok(())
}
