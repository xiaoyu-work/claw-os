//! MCP tool surface for `cosmic-edit`.
//!
//! Launched with `COS_MCP_SERVER=1`, the binary becomes a stdio MCP
//! server instead of opening an editor window. The agent gets the
//! same file-editing primitives the user has, going through the same
//! capability gate (`cos_runtime::fs::*`) and AI plumbing
//! (`cos app doc *`) — no shortcuts.
//!
//! Tools:
//!   - `edit.read`            — read a file's UTF-8 contents
//!   - `edit.write`           — write a file's UTF-8 contents (full overwrite)
//!   - `edit.replace_range`   — substring find-and-replace
//!   - `edit.open`            — spawn an interactive cosmic-edit window
//!   - `edit.summarize`       — `cos app doc summarize --file <p>`
//!   - `edit.explain`         — `cos app doc explain --file <p>`
//!   - `edit.rewrite`         — `cos app doc rewrite --file <p> --instruction <i>`

use std::sync::Arc;

use async_trait::async_trait;
use claw_os_sdk::mcp::{Server, Tool, ToolResult};
use serde_json::{Value, json};

use crate::claw_glue;

fn req_str(input: &Value, field: &str) -> Result<String, ToolResult> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .ok_or_else(|| ToolResult::err(format!("missing string field '{field}'")))
}

// ---------------------------------------------------------------------------
// edit.read
// ---------------------------------------------------------------------------

struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "edit.read"
    }
    fn description(&self) -> &'static str {
        "Read a file as UTF-8 text through the kernel capability gate. \
         Use this to inspect a file's contents before proposing an edit."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": {"type": "string"} },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let res = tokio::task::spawn_blocking(move || {
            cos_runtime::fs::read(&path)
        })
        .await;
        match res {
            Ok(Ok(r)) => ToolResult::ok(
                json!({ "path": r.path, "content": r.content }).to_string(),
            ),
            Ok(Err(e)) => ToolResult::err(format!("edit.read: {e}")),
            Err(e) => ToolResult::err(format!("edit.read join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.write
// ---------------------------------------------------------------------------

struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "edit.write"
    }
    fn description(&self) -> &'static str {
        "Overwrite a file's contents with new UTF-8 text. Goes through \
         the kernel capability gate, so a write to a protected path \
         will require approval. There is no in-process undo — call \
         `edit.read` first if you need to keep the prior contents."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["path", "content"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let content = match req_str(&input, "content") {
            Ok(c) => c,
            Err(e) => return e,
        };
        let res = tokio::task::spawn_blocking(move || {
            cos_runtime::fs::write(&path, &content)
        })
        .await;
        match res {
            Ok(Ok(_)) => ToolResult::ok(json!({"written": true}).to_string()),
            Ok(Err(e)) => ToolResult::err(format!("edit.write: {e}")),
            Err(e) => ToolResult::err(format!("edit.write join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.replace_range — substring find-and-replace
// ---------------------------------------------------------------------------

struct ReplaceRangeTool;

#[async_trait]
impl Tool for ReplaceRangeTool {
    fn name(&self) -> &'static str {
        "edit.replace_range"
    }
    fn description(&self) -> &'static str {
        "Replace exactly one occurrence of `find` with `replace` in \
         the target file. Fails if `find` appears zero times or more \
         than once — include enough surrounding context to make the \
         match unique. Preserves the rest of the file byte-for-byte."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path":    {"type": "string"},
                "find":    {"type": "string", "minLength": 1},
                "replace": {"type": "string"}
            },
            "required": ["path", "find", "replace"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let find = match req_str(&input, "find") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let replace = match req_str(&input, "replace") {
            Ok(p) => p,
            Err(e) => return e,
        };
        if find.is_empty() {
            return ToolResult::err("'find' must not be empty");
        }
        let res = tokio::task::spawn_blocking(move || -> Result<usize, String> {
            let r = cos_runtime::fs::read(&path)
                .map_err(|e| format!("read: {e}"))?;
            let body = r.content;
            let count = body.matches(&find).count();
            if count == 0 {
                return Err(format!("'find' not present in {path}"));
            }
            if count > 1 {
                return Err(format!(
                    "'find' is not unique in {path} ({count} matches); \
                     widen the context"
                ));
            }
            let new = body.replacen(&find, &replace, 1);
            cos_runtime::fs::write(&path, &new)
                .map_err(|e| format!("write: {e}"))?;
            Ok(1)
        })
        .await;
        match res {
            Ok(Ok(n)) => ToolResult::ok(json!({"replacements": n}).to_string()),
            Ok(Err(e)) => ToolResult::err(format!("edit.replace_range: {e}")),
            Err(e) => ToolResult::err(format!("edit.replace_range join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.open — spawn an interactive editor window
// ---------------------------------------------------------------------------

struct OpenTool;

#[async_trait]
impl Tool for OpenTool {
    fn name(&self) -> &'static str {
        "edit.open"
    }
    fn description(&self) -> &'static str {
        "Open a file in a new cosmic-edit window. Use this to hand \
         control back to the user after the agent has prepared a draft \
         or staged changes, e.g. 'I've drafted the new README — opening \
         it for you to review.'"
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": {"type": "string"} },
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let argv: Vec<String> = match path {
            Some(p) => vec!["cosmic-edit".into(), p],
            None => vec!["cosmic-edit".into()],
        };
        let res = tokio::task::spawn_blocking(move || {
            let argv_b: Vec<&str> = argv.iter().map(String::as_str).collect();
            cos_runtime::exec::start(&argv_b)
        })
        .await;
        match res {
            Ok(Ok(_)) => ToolResult::ok(json!({"opened": true}).to_string()),
            Ok(Err(e)) => ToolResult::err(format!("edit.open: {e}")),
            Err(e) => ToolResult::err(format!("edit.open join: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// edit.summarize / edit.explain / edit.rewrite
// ---------------------------------------------------------------------------

struct SummarizeTool;

#[async_trait]
impl Tool for SummarizeTool {
    fn name(&self) -> &'static str {
        "edit.summarize"
    }
    fn description(&self) -> &'static str {
        "Produce a short summary of the file's contents via the kernel \
         `apps/doc` route. Useful when about to propose an edit on a \
         file you haven't seen recently."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": {"type": "string"} },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        match claw_glue::ai::summarize(path.into()).await {
            Ok(s) => ToolResult::ok(json!({"summary": s}).to_string()),
            Err(e) => ToolResult::err(format!("edit.summarize: {e}")),
        }
    }
}

struct ExplainTool;

#[async_trait]
impl Tool for ExplainTool {
    fn name(&self) -> &'static str {
        "edit.explain"
    }
    fn description(&self) -> &'static str {
        "Generate a plain-language explanation of the file's contents."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "path": {"type": "string"} },
            "required": ["path"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        match claw_glue::ai::explain(path.into()).await {
            Ok(s) => ToolResult::ok(json!({"text": s}).to_string()),
            Err(e) => ToolResult::err(format!("edit.explain: {e}")),
        }
    }
}

struct RewriteTool;

#[async_trait]
impl Tool for RewriteTool {
    fn name(&self) -> &'static str {
        "edit.rewrite"
    }
    fn description(&self) -> &'static str {
        "Rewrite a file's contents per a natural-language instruction \
         (e.g. 'translate to Chinese', 'tighten the prose'). Returns \
         the proposed new body — does NOT write it back. Call \
         `edit.write` after the user confirms."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path":        {"type": "string"},
                "instruction": {"type": "string", "minLength": 1}
            },
            "required": ["path", "instruction"],
            "additionalProperties": false
        })
    }
    async fn exec(&self, input: Value) -> ToolResult {
        let path = match req_str(&input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let instruction = match req_str(&input, "instruction") {
            Ok(i) => i,
            Err(e) => return e,
        };
        match claw_glue::ai::rewrite(path.into(), instruction).await {
            Ok(s) => ToolResult::ok(json!({"text": s}).to_string()),
            Err(e) => ToolResult::err(format!("edit.rewrite: {e}")),
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
        Server::new("cosmic-edit", env!("CARGO_PKG_VERSION"))
            .tool(Arc::new(ReadTool))
            .tool(Arc::new(WriteTool))
            .tool(Arc::new(ReplaceRangeTool))
            .tool(Arc::new(OpenTool))
            .tool(Arc::new(SummarizeTool))
            .tool(Arc::new(ExplainTool))
            .tool(Arc::new(RewriteTool))
            .serve_stdio()
            .await
            .map_err(|e| anyhow::anyhow!("cosmic-edit MCP server exited: {e}"))
    })
}
