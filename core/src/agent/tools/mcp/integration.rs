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
    /// Broker endpoint, egress broker, cgroup and launch directory for
    /// this server's sandbox. Dropped with the handle, which is what
    /// stops a torn-down server from keeping any authority alive.
    _sandbox: Option<crate::worker::LaunchResources>,
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
pub struct McpRemoteTool {
    /// `mcp_<server>_<remote>`.
    name: String,
    /// Description from the server. Falls back to a generated
    /// string when the server omits one.
    description: String,
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
        let name = format!("mcp_{prefix}_{}", descriptor.name);
        let description = descriptor.description.unwrap_or_else(|| {
            format!(
                "Remote MCP tool `{}` from server `{prefix}`.",
                descriptor.name
            )
        });
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
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
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
    let proc_session = crate::bridge::McpProcSession::for_current_parent(&spec.command)?;
    // MCP servers and adapters are third-party code with no manifest of
    // their own, so they get the same hostile-worker sandbox an App
    // operation gets — minus any host path beyond the read-only system
    // image, and with no network at all. There is no separate, weaker
    // MCP launch path: a host that cannot enforce this refuses to
    // attach the server.
    let mut extra_env: std::collections::BTreeMap<String, String> = spec
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if let Some(session) = proc_session.as_ref() {
        extra_env.insert("COS_SESSION".to_string(), session.id().to_string());
    }
    let cwd = match (&spec.cwd, crate::paths::current_home_override()) {
        (Some(cwd), Some(home)) => {
            let canonical_home = home
                .canonicalize()
                .map_err(|e| format!("canonicalize MCP owner home {}: {e}", home.display()))?;
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
            Some(canonical)
        }
        (Some(cwd), None) => Some(
            std::path::Path::new(cwd)
                .canonicalize()
                .map_err(|e| format!("canonicalize MCP cwd {cwd}: {e}"))?,
        ),
        (None, _) => None,
    };
    let policy = crate::worker::derive::mcp_server(crate::worker::derive::McpServerInput {
        name: &spec.name,
        program: resolve_mcp_program(&spec.command)?,
        argv: spec.args.clone(),
        cwd,
        extra_env,
        session_id: proc_session
            .as_ref()
            .map(|session| session.id().to_string()),
    })
    .inspect_err(|error| {
        crate::worker::audit::refused(
            &format!("mcp:{}", spec.name),
            crate::worker::TrustTier::McpServer.as_str(),
            error,
        );
    })?;
    let mut launch = crate::worker::WorkerLaunch::new(policy);
    if let Some(session) = proc_session.as_ref() {
        // An MCP server holds no standing capabilities. Its authority
        // is whatever the kernel has installed on the session at the
        // instant of the call — nothing at rest, and a session tool
        // call's transient set only while that call is in flight. The
        // endpoint reads it live and relays under the launcher's grant,
        // so clearing the transient set removes it immediately.
        launch = launch.with_authority(session.broker_authority());
    }
    let prepared = crate::worker::prepare(&launch).inspect_err(|error| {
        crate::worker::audit::refused(
            &format!("mcp:{}", spec.name),
            crate::worker::TrustTier::McpServer.as_str(),
            error,
        );
    })?;
    crate::worker::audit::launched(
        &prepared.facts,
        proc_session.as_ref().map(|session| session.id()),
    );
    let crate::worker::PreparedLaunch {
        command, resources, ..
    } = prepared;
    let mut command = tokio::process::Command::from(command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
        _sandbox: Some(resources),
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
        _sandbox: None,
    })
}

/// Environment variables passed unconditionally to MCP child
/// processes.
///
/// Kept for the HTTP/SSE attach path, which starts no child of its own
/// but still resolves configured values. The stdio path no longer uses
/// it: a sandboxed server's environment is built from the launch
/// policy, so nothing from the launcher's own environment reaches it —
/// not `PATH`, not `HOME`, not a single `COS_*` value the policy did
/// not name.
#[allow(dead_code)]
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

/// Resolve an MCP server command to the canonical absolute path the
/// sandbox will execute.
///
/// Resolution happens in the launcher, against the launcher's `PATH`,
/// because inside the sandbox `PATH` is a fixed policy value and the
/// server has no say in which binary runs.
fn resolve_mcp_program(command: &str) -> Result<std::path::PathBuf, String> {
    let candidate = std::path::Path::new(command);
    if candidate.components().count() > 1 || candidate.is_absolute() {
        return candidate
            .canonicalize()
            .map_err(|error| format!("resolve MCP command `{command}`: {error}"));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let path = dir.join(command);
            if path.is_file() {
                return path
                    .canonicalize()
                    .map_err(|error| format!("resolve MCP command `{command}`: {error}"));
            }
        }
    }
    Err(format!("MCP command `{command}` was not found on PATH"))
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/mcp/integration.rs"
    ));
}
