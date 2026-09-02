//! MCP tool surface for `cosmic-store`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of opening the store GUI. Tools route through the
//! kernel-level `apps/pkg` (capability-gated `apt`/`dpkg` wrapper)
//! plus a thin `cosmic-store` GUI launcher. The agent never shells
//! out to `apt` directly.
//!
//! Tools:
//!   - `store.search(query, limit)` — `cos app pkg search`
//!   - `store.installed()`          — `cos app pkg list`
//!   - `store.show(name)`           — `cos app pkg show`
//!   - `store.open(name?)`          — spawn the store GUI, optionally
//!     deep-linked to a package
//!
//! Install/remove are intentionally **not** exposed: those are
//! impactful operations that should go through the kernel-level
//! approval flow (`cos app pkg need / install`) — the agent should
//! emit a "need" call, the user reviews it in the approval gate, and
//! the kernel performs the install. Wiring those into MCP would
//! short-circuit that flow.

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{Server, Tool, ToolResult};
use serde_json::{Value, json};

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
    fn description(&self) -> &'static str {
        "Search the system package catalogue (apt) for installable \
         packages matching a query. Routes through the kernel's \
         apps/pkg so it's capability-gated and audited. Returns \
         package name + short description for each hit."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 25}
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
            .unwrap_or(25)
            .clamp(1, 100)
            .to_string();
        match invoke_app("pkg", "search", &["--query", &query, "--limit", &limit]).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::err(format!("store.search: {e}")),
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
    fn description(&self) -> &'static str {
        "List packages dpkg considers installed on this system."
    }
    fn input_schema(&self) -> Value {
        json!({ "type": "object", "additionalProperties": false })
    }
    async fn exec(&self, _input: Value) -> ToolResult {
        match invoke_app("pkg", "list", &[]).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::err(format!("store.installed: {e}")),
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
    fn description(&self) -> &'static str {
        "Show full metadata for one package (version, description, \
         dependencies). Use this after store.search to confirm the \
         agent has the right hit before recommending it to the user."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "name": {"type": "string", "minLength": 1} },
            "required": ["name"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing name"),
        };
        match invoke_app("pkg", "show", &["--name", &name]).await {
            Ok(v) => ToolResult::ok(v.to_string()),
            Err(e) => ToolResult::err(format!("store.show: {e}")),
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
    fn description(&self) -> &'static str {
        "Open the App Store GUI. Pass `name` to land the user on \
         a specific package's detail page. Use this to hand control \
         back when the user wants to install / remove themselves."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "Optional package name to focus on."}
            },
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
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
            Ok(Ok(_)) => ToolResult::ok(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::err(format!("store.open: {e}")),
            Err(e) => ToolResult::err(format!("store.open join: {e}")),
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
        Server::new("cosmic-store", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(SearchTool))
            .tool(Arc::new(InstalledTool))
            .tool(Arc::new(ShowTool))
            .tool(Arc::new(OpenTool))
            .serve_stdio()
            .await
            .map_err(|e| anyhow::anyhow!("cosmic-store MCP server exited: {e}"))
    })
}
