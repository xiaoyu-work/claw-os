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
use tokio::io::{AsyncBufReadExt, BufReader};
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
    /// Remote endpoint for an HTTP/SSE (Streamable HTTP) server. When
    /// `Some`, this server is reached over HTTP and `command`/`args`/
    /// `env`/`cwd` are ignored (no child process is spawned).
    pub url: Option<String>,
    /// Name of the environment variable holding a bearer token for an
    /// authenticated remote server. Kept as a var name (not the token)
    /// so secrets never sit in a manifest on disk.
    pub bearer_env: Option<String>,
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
    _proc_session: Option<crate::bridge::McpProcSession>,
}

impl McpServerHandle {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tool_count(&self) -> usize {
        self.tool_count
    }

    /// Borrow a clone of the underlying MCP client. Callers that want
    /// to issue arbitrary `tools/call` invocations against this server
    /// (e.g. the app-session bridge) hold this `Arc` instead of going
    /// through registered [`McpRemoteTool`]s. The reader task stays
    /// alive as long as any clone of the client survives, so this is
    /// safe even after the handle is dropped — though the next call
    /// will fail once the child is killed.
    pub fn client(&self) -> Arc<McpClient> {
        self.client.clone()
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        // Releasing this Arc lets the McpClient::Drop fire (if no
        // tool still holds a clone), which signals the reader task
        // to exit. Then we best-effort kill + reap the child.
        let _ = Arc::strong_count(&self.client);
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            // Reap in a detached task so a zombie doesn't linger.
            // This requires a tokio runtime to be present; if Drop
            // fires outside one (the cos binary doesn't construct
            // these handles outside an agent run), the spawn silently
            // fails and the OS reaps on parent exit, which is fine.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
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
            format!(
                "Remote MCP tool `{}` from server `{prefix}`.",
                descriptor.name
            )
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
            Ok(call_result) => render_call_result(self.name, call_result),
            Err(e) => ToolResult::err(format!(
                "MCP `{}` failed: {}",
                self.name,
                render_client_err(e)
            )),
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
fn render_call_result(tool_name: &str, res: super::protocol::CallToolResult) -> ToolResult {
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
    // MCP servers are third parties; their output is untrusted. Wrap it
    // so a hostile server can't inject instructions into a kernel-
    // resident agent via its tool result.
    let wrapped = crate::agent::safety::untrusted::wrap_untrusted(
        crate::agent::safety::untrusted::TOOL_RESULT_TAG,
        &body,
    );
    if res.is_error.unwrap_or(false) {
        ToolResult::err(wrapped)
    } else {
        ToolResult::ok(wrapped)
    }
}

/// Spawn one server, run the handshake, register every tool it
/// advertises, and return the live handle. Returns `Err` on any
/// hard failure (caller logs and skips).
///
/// Process security:
/// - **Environment** is `env_clear()`ed first, then a small,
///   well-known allowlist is copied across (PATH, HOME, USER, SHELL,
///   LANG, LC_*, TZ, COS_*), then the caller-supplied `spec.env`
///   overlays on top. Without `env_clear()` the child would inherit
///   every secret the parent has (`GITHUB_TOKEN`, `OPENAI_API_KEY`,
///   etc.) — MCP servers are third-party code and should see only
///   what the agent operator explicitly grants.
/// - **Stderr** is piped into a forwarder task that prefixes each
///   line with `[mcp:<name>] ` and emits it via `tracing::warn!`.
///   The previous `Stdio::inherit()` would scribble unprefixed bytes
///   onto the parent's stderr, which corrupts TUIs and makes it
///   impossible to attribute log lines to a specific server.
/// - **Child reaping**: on handle drop we `start_kill()` and then
///   spawn a background task to `wait()` for the process. Without
///   the wait, a long-lived agent process accumulates zombies — a
///   real problem when the agent re-attaches servers on config
///   reload.
pub async fn attach_server(
    spec: &McpServerSpec,
    registry: &mut ToolRegistry,
) -> Result<McpServerHandle, String> {
    // Remote (HTTP/SSE) servers take a separate, child-less path.
    if spec.url.is_some() {
        return attach_http_server(spec, registry).await;
    }
    let proc_session =
        crate::bridge::McpProcSession::for_current_parent(&spec.command)?;
    let mut command = if proc_session.is_some() {
        let mut command =
            tokio::process::Command::new(crate::bridge::app_runner_path());
        command.arg("--").arg(&spec.command).args(&spec.args);
        command
    } else {
        let mut command = tokio::process::Command::new(&spec.command);
        command.args(&spec.args);
        command
    };
    // Wipe inherited environment then re-add an explicit allowlist.
    // The order is: env_clear → allowlist from os::env → spec.env
    // overlay. Caller-provided values win on collision.
    command.env_clear();
    for (k, v) in safe_env_allowlist() {
        command.env(k, v);
    }
    for (k, v) in &spec.env {
        command.env(k, v);
    }
    if let Some(session) = proc_session.as_ref() {
        command
            .env("COS_SESSION", session.id())
            .env("COS_PROC_DATA_DIR", session.proc_data_dir());
    }
    if let Some(home) = crate::paths::current_home_override() {
        let canonical_home = home
            .canonicalize()
            .map_err(|e| format!("canonicalize MCP owner home {}: {e}", home.display()))?;
        let cwd = match &spec.cwd {
            Some(cwd) => {
                let canonical = std::path::Path::new(cwd)
                    .canonicalize()
                    .map_err(|e| format!("canonicalize MCP cwd {cwd}: {e}"))?;
                if !canonical.starts_with(&canonical_home) {
                    return Err(format!(
                        "MCP cwd {} escapes owner home {}",
                        canonical.display(),
                        canonical_home.display()
                    ));
                }
                canonical
            }
            None => canonical_home,
        };
        command
            .current_dir(cwd)
            .env("HOME", &home)
            .env("COS_HOME", &home)
            .env("COS_DATA_DIR", crate::paths::user_data_dir())
            .env("COS_PROC_DATA_DIR", crate::paths::proc_data_dir());
    } else if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::bridge::apply_routed_identity(command.as_std_mut())?;
    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn `{}`: {e}", spec.command))?;
    if let Some(session) = proc_session.as_ref() {
        let Some(pid) = child.id() else {
            kill_and_reap(child);
            return Err("spawned MCP server has no pid".to_string());
        };
        if let Err(error) = session.bind_process(pid) {
            kill_and_reap(child);
            return Err(format!("bind MCP child session: {error}"));
        }
    }
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "child stdin unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout unavailable".to_string())?;
    // Forward child stderr line-by-line under a `[mcp:<name>]`
    // prefix. The task ends when the child closes its stderr.
    if let Some(stderr) = child.stderr.take() {
        let prefix = spec.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        tracing::warn!(target: "mcp", "[mcp:{prefix}] {line}");
                    }
                    Ok(None) => break,
                    Err(e) => {
                        tracing::warn!(target: "mcp", "[mcp:{prefix}] stderr read error: {e}");
                        break;
                    }
                }
            }
        });
    }
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
            kill_and_reap(child);
            return Err(format!("initialize: {}", render_client_err(e)));
        }
        Err(_) => {
            kill_and_reap(child);
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
            kill_and_reap(child);
            return Err(format!("tools/list: {}", render_client_err(e)));
        }
        Err(_) => {
            kill_and_reap(child);
            return Err(format!(
                "tools/list timed out after {}s",
                timeout_dur.as_secs()
            ));
        }
    };

    let mut registered = 0usize;
    for descriptor in tools.tools {
        let tool = McpRemoteTool::new(&spec.name, descriptor, client.clone(), timeout_dur);
        registry.register(Arc::new(tool));
        registered += 1;
    }

    Ok(McpServerHandle {
        client,
        child: Some(child),
        name: spec.name.clone(),
        tool_count: registered,
        _proc_session: proc_session,
    })
}

/// Attach a **remote** MCP server over HTTP/SSE (Streamable HTTP).
/// Unlike the stdio path there is no child process — the transport
/// speaks JSON-RPC to `spec.url`. This is what lets the agent use
/// hosted MCP servers, not just local subprocesses. Optional bearer
/// auth is read from the env var named by `spec.bearer_env`, so tokens
/// never sit in an on-disk manifest.
pub async fn attach_http_server(
    spec: &McpServerSpec,
    registry: &mut ToolRegistry,
) -> Result<McpServerHandle, String> {
    let url_str = spec
        .url
        .as_deref()
        .ok_or_else(|| "attach_http_server called without a url".to_string())?;
    let url =
        reqwest::Url::parse(url_str).map_err(|e| format!("invalid mcp url `{url_str}`: {e}"))?;
    let bearer = spec
        .bearer_env
        .as_deref()
        .and_then(|var| std::env::var(var).ok())
        .filter(|t| !t.is_empty());

    let transport = super::transport::HttpTransport::new(url, bearer)
        .map_err(|e| format!("http transport: {e}"))?;
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
    match timeout(timeout_dur, init_fut).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("initialize: {}", render_client_err(e))),
        Err(_) => {
            return Err(format!(
                "initialize timed out after {}s",
                timeout_dur.as_secs()
            ))
        }
    }
    let _ = client.notify("notifications/initialized", None).await;

    let list_fut = client.list_tools();
    let tools = match timeout(timeout_dur, list_fut).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => return Err(format!("tools/list: {}", render_client_err(e))),
        Err(_) => {
            return Err(format!(
                "tools/list timed out after {}s",
                timeout_dur.as_secs()
            ))
        }
    };

    let mut registered = 0usize;
    for descriptor in tools.tools {
        let tool = McpRemoteTool::new(&spec.name, descriptor, client.clone(), timeout_dur);
        registry.register(Arc::new(tool));
        registered += 1;
    }

    Ok(McpServerHandle {
        client,
        child: None,
        name: spec.name.clone(),
        tool_count: registered,
        _proc_session: None,
    })
}

/// Environment variables passed unconditionally to MCP child
/// processes. These are the bare minimum a typical command-line tool
/// needs to function (locate its libraries, render Unicode, locate
/// its config home). Notably absent: any `*_TOKEN`, `*_KEY`,
/// `*_SECRET`, AWS / GCP / Azure credentials, the user's
/// `OPENAI_API_KEY`, GitHub tokens, etc.
///
/// `COS_*` variables are forwarded because they configure the
/// agent's own runtime; some MCP servers shipped with cos expect
/// e.g. `COS_DATA_DIR` to be set.
fn safe_env_allowlist() -> Vec<(String, String)> {
    const ALWAYS: &[&str] = &[
        "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES",
        "TZ", "TERM", "TMPDIR", "TEMP", "TMP",
    ];
    let mut out = Vec::with_capacity(ALWAYS.len() + 8);
    for k in ALWAYS {
        if let Ok(v) = std::env::var(k) {
            out.push(((*k).to_string(), v));
        }
    }
    const SAFE_COS: &[&str] = &[
        "COS_SESSION",
        "COS_TRACE_ID",
        "COS_SPAN_ID",
        "COS_BIN",
        "COS_VERSION",
        "COS_SDK_PYTHON_DIR",
        "COS_SNAPSHOT",
        "COS_PERMS_MODE",
    ];
    for key in SAFE_COS {
        if let Ok(value) = std::env::var(key) {
            out.push(((*key).to_string(), value));
        }
    }
    out
}

/// Kill the child and spawn a background reap so zombies don't
/// accumulate across many attach attempts. `start_kill()` posts the
/// signal synchronously; `wait()` collects the exit status. We must
/// take the child by-value because both methods need exclusive
/// access and the caller's error path can't await.
fn kill_and_reap(mut child: Child) {
    let _ = child.start_kill();
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
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
            url: None,
            bearer_env: None,
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
                ContentItem::Text {
                    text: "hello".into(),
                },
                ContentItem::Text {
                    text: "world".into(),
                },
            ],
            is_error: None,
        };
        let r = render_call_result("mcp_x_y", res);
        assert!(!r.is_error);
        // MCP results are wrapped in an untrusted-data boundary
        // (prompt-injection defense); the concatenated body lives inside.
        assert!(r.content.contains("hello\n\nworld"), "content: {}", r.content);
        assert!(
            r.content.contains("<untrusted_tool_result>"),
            "content: {}",
            r.content
        );
    }

    #[test]
    fn render_call_result_marks_error_when_is_error_true() {
        let res = CallToolResult {
            content: vec![ContentItem::Text {
                text: "boom".into(),
            }],
            is_error: Some(true),
        };
        let r = render_call_result("mcp_x_y", res);
        assert!(r.is_error);
        assert!(r.content.contains("boom"), "content: {}", r.content);
        assert!(
            r.content.contains("<untrusted_tool_result>"),
            "content: {}",
            r.content
        );
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
                server_t
                    .send(serde_json::to_string(&resp).unwrap())
                    .await
                    .unwrap();
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
        // The remote result is wrapped in the untrusted-tool-result
        // boundary before it reaches the agent loop.
        assert!(result.content.contains("pong"), "content: {}", result.content);
        assert!(
            result.content.contains("<untrusted_tool_result>"),
            "content: {}",
            result.content
        );

        drop(client);
        let _ = server_task.await;
    }
}
