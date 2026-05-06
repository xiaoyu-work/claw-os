//! Glue between configured MCP servers and the agent tool registry.
//!
//! The plain `mcp::client` + `mcp::transport` halves give us a wire-
//! protocol-compliant MCP client. This module ties them to the agent
//! lifecycle:
//!
//! * spawn each configured server as a child process,
//! * run `initialize` + `tools/list`,
//! * register every advertised tool as an [`McpRemoteTool`] in the
//!   agent's [`ToolRegistry`] under a deterministic prefix
//!   (`mcp_<server>_<remote>`),
//! * return a [`Vec<McpServerHandle>`] the caller must keep alive
//!   for the duration of any agent loop that wants to call those
//!   tools. Dropping a handle aborts the client reader task and
//!   kills the child.
//!
//! Failure model: every error is best-effort. A missing executable, a
//! handshake that hangs, a server that exposes zero tools — none of
//! these prevent the agent from running. They emit `tracing::warn!`
//! and the affected server is skipped. The agent should never refuse
//! to answer because an optional MCP server is misconfigured.
//!
//! Lifecycle ordering matters: `_client` must drop *before* `_child`
//! so the reader task releases its `Arc<Transport>` before we kill
//! the process holding the other end of the stdio pipes. Rust drops
//! struct fields in declaration order, so the field ordering in
//! [`McpServerHandle`] is load-bearing.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Child;
use tokio::time::timeout;

use super::client::{ClientError, McpClient};
use super::protocol::{ClientCapabilities, Implementation, ToolDescriptor, PROTOCOL_VERSION};
use super::transport::StdioTransport;
use crate::agent::tools::registry::ToolRegistry;
use crate::agent::tools::{Tool, ToolResult};

/// Configuration for one MCP server the agent should attach to.
///
/// The lifetime of this struct is the agent's config lifetime — it is
/// read once at startup. Per-call data (handles, clients) lives on
/// [`McpServerHandle`].
#[derive(Debug, Clone)]
pub struct McpServerSpec {
    /// Stable, snake_case identifier. Becomes the prefix in registered
    /// tool names: `mcp_<name>_<remote_tool_name>`. Must be unique
    /// across all MCP servers in this agent instance.
    pub name: String,
    /// Executable to invoke. Resolved against `PATH`.
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub cwd: Option<String>,
    /// Per-RPC timeout (initialize, tools/list, tools/call). 0 means
    /// "no timeout" — interpreted as `u64::MAX` seconds, effectively
    /// unbounded.
    pub timeout_secs: u64,
}

impl McpServerSpec {
    fn timeout_duration(&self) -> Duration {
        if self.timeout_secs == 0 {
            Duration::from_secs(u64::MAX)
        } else {
            Duration::from_secs(self.timeout_secs)
        }
    }
}

/// Owned handle to one running MCP server. Drop to terminate.
///
/// Field order is significant: `client` is dropped before `child`
/// so the reader task's `Arc<Transport>` is released before the
/// child's stdio fds are closed by `kill`/`wait`.
pub struct McpServerHandle {
    /// Shared with every `McpRemoteTool` registered for this server;
    /// kept here so even if the registry is dropped first, the
    /// client (and thus the reader task) lives until handle drop.
    client: Arc<McpClient>,
    child: Option<Child>,
    /// For diagnostics; not used past construction.
    name: String,
    tool_count: usize,
}

impl McpServerHandle {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tool_count(&self) -> usize {
        self.tool_count
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        // Releasing this Arc lets the McpClient::Drop fire (if no
        // tool still holds a clone), which aborts the reader task.
        // Then we best-effort kill + reap the child.
        let _ = Arc::strong_count(&self.client);
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            // We don't await wait() here — we're not async — but
            // start_kill posts SIGKILL/TerminateProcess; the OS will
            // reap once stdio fds close.
        }
    }
}

/// Tool wrapper that proxies `exec` to a remote MCP `tools/call`.
///
/// Holds a leaked `&'static str` for `name` / `description` because
/// the [`Tool`] trait's signatures require that lifetime. Leaks are
/// bounded by the number of MCP tools the user configures (a few
/// hundred bytes per tool, lasting until process exit) and never
/// happen on hot paths.
pub struct McpRemoteTool {
    /// `mcp_<server>_<remote>`, leaked.
    name: &'static str,
    /// Description from the server, leaked. Falls back to a generated
    /// string when the server omits one.
    description: &'static str,
    /// Cached on construction; cloned per `input_schema()` call (the
    /// trait returns by value).
    schema: Value,
    /// Untransformed remote tool name to send back over the wire.
    remote_name: String,
    client: Arc<McpClient>,
    timeout: Duration,
}

impl McpRemoteTool {
    fn new(
        prefix: &str,
        descriptor: ToolDescriptor,
        client: Arc<McpClient>,
        timeout: Duration,
    ) -> Self {
        let prefixed = format!("mcp_{prefix}_{}", descriptor.name);
        let name: &'static str = Box::leak(prefixed.into_boxed_str());
        let raw_desc = descriptor.description.unwrap_or_else(|| {
            format!("Remote MCP tool `{}` from server `{prefix}`.", descriptor.name)
        });
        let description: &'static str = Box::leak(raw_desc.into_boxed_str());
        // Some MCP servers report Null / missing `inputSchema`. The
        // LLM trait expects an object schema; coerce minimally so the
        // model doesn't see a `null` schema.
        let schema = if descriptor.input_schema.is_object() {
            descriptor.input_schema
        } else {
            json!({"type": "object", "properties": {}, "additionalProperties": true})
        };
        Self {
            name,
            description,
            schema,
            remote_name: descriptor.name,
            client,
            timeout,
        }
    }
}

#[async_trait]
impl Tool for McpRemoteTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &'static str {
        self.description
    }

    fn input_schema(&self) -> Value {
        self.schema.clone()
    }

    async fn exec(&self, input: Value) -> ToolResult {
        // MCP's `arguments` is `Option<Value>`. Treat empty/null
        // input as None so servers that pattern-match on absence
        // (vs. empty object) work correctly.
        let arguments = match input {
            Value::Null => None,
            Value::Object(ref m) if m.is_empty() => None,
            other => Some(other),
        };
        let call = self.client.call_tool(self.remote_name.clone(), arguments);
        let res = match timeout(self.timeout, call).await {
            Ok(r) => r,
            Err(_) => {
                return ToolResult::err(format!(
                    "MCP `{}` timed out after {}s",
                    self.name,
                    self.timeout.as_secs()
                ));
            }
        };
        match res {
            Ok(call_result) => render_call_result(&self.name, call_result),
            Err(e) => ToolResult::err(format!("MCP `{}` failed: {}", self.name, render_client_err(e))),
        }
    }
}

fn render_client_err(e: ClientError) -> String {
    match e {
        ClientError::Server { code, message, .. } => {
            format!("server error {code}: {message}")
        }
        other => other.to_string(),
    }
}

/// Convert MCP `CallToolResult` to a [`ToolResult`]. Concatenates
/// `text` content items with blank-line separators; `image` items are
/// rendered as a placeholder line so the model knows non-text content
/// was returned without us double-encoding base64. `isError: true`
/// from the server flips us to [`ToolResult::err`].
fn render_call_result(
    tool_name: &str,
    res: super::protocol::CallToolResult,
) -> ToolResult {
    use super::protocol::ContentItem;
    let mut chunks: Vec<String> = Vec::new();
    for item in res.content {
        match item {
            ContentItem::Text { text } => chunks.push(text),
            ContentItem::Image { mime_type, .. } => {
                chunks.push(format!("[image content omitted ({mime_type})]"));
            }
        }
    }
    let body = if chunks.is_empty() {
        format!("(MCP tool `{tool_name}` returned no content)")
    } else {
        chunks.join("\n\n")
    };
    if res.is_error.unwrap_or(false) {
        ToolResult::err(body)
    } else {
        ToolResult::ok(body)
    }
}

/// Spawn one server, run the handshake, register every tool it
/// advertises, and return the live handle. Returns `Err` on any
/// hard failure (caller logs and skips).
pub async fn attach_server(
    spec: &McpServerSpec,
    registry: &mut ToolRegistry,
) -> Result<McpServerHandle, String> {
    let mut command = tokio::process::Command::new(&spec.command);
    command.args(&spec.args);
    for (k, v) in &spec.env {
        command.env(k, v);
    }
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn `{}`: {e}", spec.command))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "child stdin unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout unavailable".to_string())?;
    let transport = StdioTransport::from_pair(Box::new(stdout), Box::new(stdin));
    let client = McpClient::new(transport);
    client.start().await;

    let timeout_dur = spec.timeout_duration();
    let init_fut = client.initialize(
        Implementation {
            name: "cos-agent".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        ClientCapabilities::default(),
    );
    let init = match timeout(timeout_dur, init_fut).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let _ = child.start_kill();
            return Err(format!("initialize: {}", render_client_err(e)));
        }
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "initialize timed out after {}s",
                timeout_dur.as_secs()
            ));
        }
    };
    // Most servers don't gate on this notification, but spec-correct
    // clients are required to send it before issuing any post-
    // initialize requests. Failures here aren't fatal.
    let _ = client.notify("notifications/initialized", None).await;

    if init.protocol_version != PROTOCOL_VERSION {
        // Servers may speak an older or newer version; we proceed
        // anyway because every method we use (tools/list, tools/call)
        // has been stable since 2024-11. Worth surfacing in the log.
        tracing::info!(
            "mcp `{}`: server protocol version `{}` differs from client `{PROTOCOL_VERSION}`",
            spec.name,
            init.protocol_version
        );
    }

    let list_fut = client.list_tools();
    let tools = match timeout(timeout_dur, list_fut).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let _ = child.start_kill();
            return Err(format!("tools/list: {}", render_client_err(e)));
        }
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "tools/list timed out after {}s",
                timeout_dur.as_secs()
            ));
        }
    };

    let mut registered = 0usize;
    for descriptor in tools.tools {
        let tool = McpRemoteTool::new(
            &spec.name,
            descriptor,
            client.clone(),
            timeout_dur,
        );
        registry.register(Arc::new(tool));
        registered += 1;
    }

    Ok(McpServerHandle {
        client,
        child: Some(child),
        name: spec.name.clone(),
        tool_count: registered,
    })
}

/// Convenience wrapper: try to attach every spec, log and skip
/// anything that fails. Returned handles must be kept alive until
/// the agent loop completes; dropping them tears down the children.
pub async fn attach_all(
    specs: &[McpServerSpec],
    registry: &mut ToolRegistry,
) -> Vec<McpServerHandle> {
    let mut handles = Vec::with_capacity(specs.len());
    for spec in specs {
        match attach_server(spec, registry).await {
            Ok(handle) => {
                tracing::info!(
                    "mcp `{}`: attached, registered {} tool(s)",
                    handle.name(),
                    handle.tool_count()
                );
                handles.push(handle);
            }
            Err(e) => {
                tracing::warn!("mcp `{}`: attach failed: {e}", spec.name);
            }
        }
    }
    handles
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::mcp::protocol::{
        CallToolResult, ContentItem, ListToolsResult, ToolDescriptor,
    };
    use crate::agent::tools::mcp::transport::{in_memory_pair, Transport};
    use crate::agent::tools::registry::ToolRegistry;
    use serde_json::json;

    fn make_spec(name: &str) -> McpServerSpec {
        McpServerSpec {
            name: name.to_string(),
            command: "true".to_string(),
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            timeout_secs: 5,
        }
    }

    #[test]
    fn timeout_duration_zero_means_unbounded() {
        let mut spec = make_spec("s");
        spec.timeout_secs = 0;
        assert_eq!(spec.timeout_duration(), Duration::from_secs(u64::MAX));
    }

    #[test]
    fn timeout_duration_nonzero_is_passthrough() {
        let mut spec = make_spec("s");
        spec.timeout_secs = 17;
        assert_eq!(spec.timeout_duration(), Duration::from_secs(17));
    }

    #[test]
    fn render_call_result_concatenates_text() {
        let res = CallToolResult {
            content: vec![
                ContentItem::Text { text: "hello".into() },
                ContentItem::Text { text: "world".into() },
            ],
            is_error: None,
        };
        let r = render_call_result("mcp_x_y", res);
        assert!(!r.is_error);
        assert_eq!(r.content, "hello\n\nworld");
    }

    #[test]
    fn render_call_result_marks_error_when_is_error_true() {
        let res = CallToolResult {
            content: vec![ContentItem::Text { text: "boom".into() }],
            is_error: Some(true),
        };
        let r = render_call_result("mcp_x_y", res);
        assert!(r.is_error);
        assert_eq!(r.content, "boom");
    }

    #[test]
    fn render_call_result_handles_empty_content() {
        let res = CallToolResult {
            content: Vec::new(),
            is_error: None,
        };
        let r = render_call_result("mcp_x_y", res);
        assert!(!r.is_error);
        assert!(r.content.contains("returned no content"));
    }

    #[test]
    fn render_call_result_image_placeholder_mentions_mime() {
        let res = CallToolResult {
            content: vec![ContentItem::Image {
                data: "QUJD".into(),
                mime_type: "image/png".into(),
            }],
            is_error: None,
        };
        let r = render_call_result("mcp_x_y", res);
        assert!(r.content.contains("image/png"));
        assert!(r.content.contains("omitted"));
    }

    #[test]
    fn mcp_remote_tool_uses_prefix_and_remote_name_round_trip() {
        let (client_t, _server_t) = in_memory_pair();
        let client = McpClient::new(client_t);
        let descriptor = ToolDescriptor {
            name: "query".to_string(),
            description: Some("run a query".to_string()),
            input_schema: json!({"type": "object", "properties": {"sql": {"type": "string"}}}),
        };
        let tool = McpRemoteTool::new("postgres", descriptor, client, Duration::from_secs(5));
        assert_eq!(tool.name(), "mcp_postgres_query");
        assert_eq!(tool.description(), "run a query");
        assert_eq!(tool.remote_name, "query");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["sql"].is_object());
    }

    #[test]
    fn mcp_remote_tool_falls_back_for_missing_description() {
        let (client_t, _server_t) = in_memory_pair();
        let client = McpClient::new(client_t);
        let descriptor = ToolDescriptor {
            name: "ping".to_string(),
            description: None,
            input_schema: json!({"type": "object"}),
        };
        let tool = McpRemoteTool::new("svc", descriptor, client, Duration::from_secs(5));
        assert!(tool.description().contains("ping"));
        assert!(tool.description().contains("svc"));
    }

    #[test]
    fn mcp_remote_tool_coerces_non_object_schema() {
        let (client_t, _server_t) = in_memory_pair();
        let client = McpClient::new(client_t);
        let descriptor = ToolDescriptor {
            name: "no_args".to_string(),
            description: Some("trigger".into()),
            input_schema: Value::Null,
        };
        let tool = McpRemoteTool::new("svc", descriptor, client, Duration::from_secs(5));
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        // additionalProperties on permissive fallback
        assert_eq!(schema["additionalProperties"], true);
    }

    /// End-to-end: a fake "MCP server" running in the same task pair
    /// answers `tools/list` with one descriptor and `tools/call` with
    /// a text payload. Verifies attach_server-equivalent flow against
    /// the in-memory transport (we can't spawn a real subprocess in
    /// unit tests portably).
    #[tokio::test]
    async fn end_to_end_in_memory_attach_flow_routes_call_through_prefixed_tool() {
        use crate::agent::tools::mcp::protocol::{
            InitializeResult, JsonRpcRequest, JsonRpcResponse, ServerCapabilities,
        };
        let (client_t, server_t) = in_memory_pair();
        let client = McpClient::new(client_t);
        client.start().await;

        let server_task = tokio::spawn(async move {
            for _ in 0..3 {
                let frame = match server_t.recv().await {
                    Ok(Some(f)) => f,
                    _ => break,
                };
                let req: JsonRpcRequest = serde_json::from_str(&frame).unwrap();
                let result = match req.method.as_str() {
                    "initialize" => serde_json::to_value(InitializeResult {
                        protocol_version: PROTOCOL_VERSION.to_string(),
                        capabilities: ServerCapabilities::default(),
                        server_info: Implementation {
                            name: "fake".into(),
                            version: "0.0.1".into(),
                        },
                        instructions: None,
                    })
                    .unwrap(),
                    "tools/list" => serde_json::to_value(ListToolsResult {
                        tools: vec![ToolDescriptor {
                            name: "say".into(),
                            description: Some("echo back".into()),
                            input_schema: json!({"type": "object"}),
                        }],
                        next_cursor: None,
                    })
                    .unwrap(),
                    "tools/call" => serde_json::to_value(CallToolResult {
                        content: vec![ContentItem::Text {
                            text: "pong".into(),
                        }],
                        is_error: None,
                    })
                    .unwrap(),
                    _ => json!({}),
                };
                let resp = JsonRpcResponse::ok(req.id, result);
                server_t.send(serde_json::to_string(&resp).unwrap()).await.unwrap();
            }
        });

        // Drive the same handshake `attach_server` performs, but
        // against the in-memory pair so we can avoid spawning.
        let init = client
            .initialize(
                Implementation {
                    name: "test".into(),
                    version: "0.0.0".into(),
                },
                ClientCapabilities::default(),
            )
            .await
            .unwrap();
        assert_eq!(init.server_info.name, "fake");
        let list = client.list_tools().await.unwrap();
        assert_eq!(list.tools.len(), 1);

        let mut registry = ToolRegistry::new();
        let descriptor = list.tools.into_iter().next().unwrap();
        let tool = McpRemoteTool::new("svc", descriptor, client.clone(), Duration::from_secs(5));
        let registered_name = tool.name();
        assert_eq!(registered_name, "mcp_svc_say");
        registry.register(Arc::new(tool));

        // Pull it back out of the registry and call it as the agent
        // loop would — `get` honours guardrails (we set none, so
        // permissive default permits everything).
        let dyn_tool = registry.get(registered_name).expect("tool registered");
        let result = dyn_tool.exec(json!({})).await;
        assert!(!result.is_error, "tool call should succeed: {:?}", result);
        assert_eq!(result.content, "pong");

        drop(client);
        let _ = server_task.await;
    }
}
