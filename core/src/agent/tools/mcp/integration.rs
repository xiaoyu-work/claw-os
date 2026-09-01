//! Glue between configured MCP servers and the agent tool registry.
//!
//! The plain `mcp::client` + `mcp::transport` halves give us a wire-
//! protocol-compliant MCP client. This module ties them to the agent
//! lifecycle:
//!
//! * spawn each configured server locally for direct runtimes, or attach it
//!   through the task-owned extension host for `claw-agentd`,
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

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::time::timeout;

use super::client::{ClientError, McpClient};
use super::descriptor::{model_tool_name, sanitize_descriptor_set, NEUTRAL_DESCRIPTION};
use super::protocol::{ClientCapabilities, Implementation, ToolDescriptor, PROTOCOL_VERSION};
use super::transport::StdioTransport;
use crate::agent::tools::exposure::{ToolExposure, ToolExposureContext, ToolTransport};
use crate::agent::tools::registry::ToolRegistry;
use crate::agent::tools::{Tool, ToolResult};

/// Configuration for one MCP server the agent should attach to.
///
/// The lifetime of this struct is the agent's config lifetime — it is
/// read once at startup. Per-call data (handles, clients) lives on
/// [`McpServerHandle`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    client: Option<Arc<McpClient>>,
    child: Option<Child>,
    /// For diagnostics; not used past construction.
    name: String,
    tool_count: usize,
    descriptors: Vec<ToolDescriptor>,
    descriptor_digest: String,
    timeout: Duration,
    hosted: bool,
    _proc_session: Option<crate::bridge::McpProcSession>,
}

impl McpServerHandle {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tool_count(&self) -> usize {
        self.tool_count
    }

    pub(crate) fn descriptors(&self) -> &[ToolDescriptor] {
        &self.descriptors
    }

    pub(crate) fn descriptor_digest(&self) -> &str {
        &self.descriptor_digest
    }

    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Borrow a clone of the underlying MCP client. Callers that want
    /// to issue arbitrary `tools/call` invocations against this server
    /// (e.g. the app-session bridge) hold this `Arc` instead of going
    /// through registered [`McpRemoteTool`]s. The reader task stays
    /// alive as long as any clone of the client survives, so this is
    /// safe even after the handle is dropped — though the next call
    /// will fail once the child is killed.
    pub fn client(&self) -> Arc<McpClient> {
        self.client
            .as_ref()
            .expect("a hosted MCP handle has no in-process client")
            .clone()
    }
}

impl Drop for McpServerHandle {
    fn drop(&mut self) {
        if self.hosted {
            if let Some(client) = crate::extension_host::client::current() {
                let name = self.name.clone();
                if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                    runtime.spawn(async move {
                        let _ = client.detach_mcp(name).await;
                    });
                }
            }
            return;
        }
        // Releasing this Arc lets the McpClient::Drop fire (if no
        // tool still holds a clone), which signals the reader task
        // to exit. Then we best-effort kill + reap the child.
        if let Some(client) = self.client.as_ref() {
            let _ = Arc::strong_count(client);
        }
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
    /// Neutral local description. Remote prose never enters model context.
    description: String,
    /// Cached on construction; cloned per `input_schema()` call (the
    /// trait returns by value).
    schema: Value,
    /// Untransformed remote tool name to send back over the wire.
    remote_name: String,
    backend: McpToolBackend,
    timeout: Duration,
    exposure: ToolExposure,
}

enum McpToolBackend {
    Local {
        client: Arc<McpClient>,
        server: String,
        descriptor_digest: String,
    },
    Hosted {
        server: String,
        descriptor_digest: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DisclosureBinding {
    owner_uid: u32,
    authority_session_id: String,
    task_id: Option<String>,
    capability_generation: String,
}

impl DisclosureBinding {
    fn from_context(context: &ToolExposureContext) -> Self {
        Self {
            owner_uid: context.owner_uid(),
            authority_session_id: context.authority_session_id().to_string(),
            task_id: context.task_id().map(str::to_string),
            capability_generation: context.capability_generation().to_string(),
        }
    }

    fn matches(&self, context: &ToolExposureContext) -> bool {
        self.owner_uid == context.owner_uid()
            && self.authority_session_id == context.authority_session_id()
            && self.task_id.as_deref() == context.task_id()
            && self.capability_generation == context.capability_generation()
    }
}

struct DisclosureEntry {
    descriptor: ToolDescriptor,
    tool: Arc<McpRemoteTool>,
}

pub(crate) struct McpDisclosureState {
    binding: DisclosureBinding,
    entries: Mutex<BTreeMap<String, DisclosureEntry>>,
}

impl McpDisclosureState {
    fn new(context: &ToolExposureContext) -> Arc<Self> {
        Arc::new(Self {
            binding: DisclosureBinding::from_context(context),
            entries: Mutex::new(BTreeMap::new()),
        })
    }

    fn insert(&self, descriptor: ToolDescriptor, tool: Arc<McpRemoteTool>) -> Result<(), String> {
        let handle = uuid::Uuid::new_v4().simple().to_string();
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| "MCP disclosure registry is poisoned".to_string())?;
        if entries.contains_key(&handle) {
            return Err("MCP disclosure handle collision".to_string());
        }
        entries.insert(handle, DisclosureEntry { descriptor, tool });
        Ok(())
    }

    fn is_empty(&self) -> bool {
        self.entries
            .lock()
            .map(|entries| entries.is_empty())
            .unwrap_or(true)
    }
}

struct McpCatalogTool {
    state: Arc<McpDisclosureState>,
}

struct McpInvokeTool {
    state: Arc<McpDisclosureState>,
}

impl McpRemoteTool {
    fn new(
        prefix: &str,
        descriptor: ToolDescriptor,
        client: Arc<McpClient>,
        timeout: Duration,
        descriptor_digest: String,
    ) -> Self {
        Self::new_with_transport(
            prefix,
            descriptor,
            client,
            timeout,
            ToolTransport::McpStdio,
            descriptor_digest,
        )
    }

    fn new_with_transport(
        prefix: &str,
        descriptor: ToolDescriptor,
        client: Arc<McpClient>,
        timeout: Duration,
        transport: ToolTransport,
        descriptor_digest: String,
    ) -> Self {
        let name = model_tool_name(prefix, &descriptor.name)
            .expect("MCP descriptors are sanitized before tool construction");
        Self {
            name,
            description: NEUTRAL_DESCRIPTION.to_string(),
            schema: descriptor.input_schema,
            remote_name: descriptor.name,
            backend: McpToolBackend::Local {
                client,
                server: prefix.to_string(),
                descriptor_digest,
            },
            timeout,
            exposure: ToolExposure::always()
                .requiring_transport(transport)
                .requiring_extension(format!("mcp:{prefix}")),
        }
    }

    fn new_hosted(
        prefix: &str,
        descriptor: ToolDescriptor,
        timeout: Duration,
        transport: ToolTransport,
        descriptor_digest: String,
    ) -> Self {
        let name = model_tool_name(prefix, &descriptor.name)
            .expect("MCP descriptors are sanitized before tool construction");
        Self {
            name,
            description: NEUTRAL_DESCRIPTION.to_string(),
            schema: descriptor.input_schema,
            remote_name: descriptor.name,
            backend: McpToolBackend::Hosted {
                server: prefix.to_string(),
                descriptor_digest,
            },
            timeout,
            exposure: ToolExposure::always()
                .requiring_transport(transport)
                .requiring_extension(format!("mcp:{prefix}")),
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

    fn exposure(&self) -> ToolExposure {
        self.exposure.clone()
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
        let res = match &self.backend {
            McpToolBackend::Local {
                client,
                server,
                descriptor_digest,
            } => {
                if let Err(error) =
                    verify_descriptor_stability(server, client, self.timeout, descriptor_digest)
                        .await
                {
                    return ToolResult::err(error);
                }
                let call = client.call_tool(self.remote_name.clone(), arguments);
                match timeout(self.timeout, call).await {
                    Ok(result) => result.map_err(render_client_err),
                    Err(_) => Err(format!(
                        "MCP `{}` timed out after {}s",
                        self.name,
                        self.timeout.as_secs()
                    )),
                }
            }

            McpToolBackend::Hosted {
                server,
                descriptor_digest,
            } => {
                let Some(client) = crate::extension_host::client::current() else {
                    return ToolResult::err("the task extension host is unavailable");
                };
                client
                    .call_mcp(
                        server.clone(),
                        self.remote_name.clone(),
                        descriptor_digest.clone(),
                        arguments,
                        self.timeout,
                    )
                    .await
            }
        };
        match res {
            Ok(call_result) => render_call_result(&self.name, call_result),
            Err(error) => ToolResult::err(crate::agent::safety::untrusted::wrap_untrusted(
                crate::agent::safety::untrusted::TOOL_RESULT_TAG,
                &format!("MCP `{}` failed: {error}", self.name),
            )),
        }
    }
}

#[async_trait]
impl Tool for McpCatalogTool {
    fn name(&self) -> &str {
        "mcp_catalog"
    }

    fn description(&self) -> &str {
        "List configured MCP capabilities as untrusted data with opaque invocation handles."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    async fn exec(&self, _input: Value) -> ToolResult {
        let Some(context) = crate::agent::tools::exposure::current() else {
            return ToolResult::err("MCP catalog requires an execution context");
        };
        if !self.state.binding.matches(&context) {
            return ToolResult::err("MCP catalog binding does not match this session");
        }
        let catalog = match self.state.entries.lock() {
            Ok(entries) => entries
                .iter()
                .map(|(handle, entry)| {
                    json!({
                        "handle": handle,
                        "name": entry.descriptor.name,
                        "input_schema": entry.descriptor.input_schema,
                    })
                })
                .collect::<Vec<_>>(),
            Err(_) => return ToolResult::err("MCP disclosure registry is unavailable"),
        };
        let payload = match serde_json::to_string(&json!({"tools": catalog})) {
            Ok(payload) => payload,
            Err(_) => return ToolResult::err("MCP catalog encoding failed"),
        };
        ToolResult::ok(crate::agent::safety::untrusted::wrap_untrusted(
            crate::agent::safety::untrusted::TOOL_RESULT_TAG,
            &payload,
        ))
    }
}

#[async_trait]
impl Tool for McpInvokeTool {
    fn name(&self) -> &str {
        "mcp_invoke"
    }

    fn description(&self) -> &str {
        "Invoke one previously disclosed MCP capability using its opaque handle."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "handle": {
                    "type": "string",
                    "minLength": 32,
                    "maxLength": 32
                },
                "arguments": {
                    "type": ["object", "null"],
                    "additionalProperties": true
                }
            },
            "required": ["handle"],
            "additionalProperties": false
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let Some(context) = crate::agent::tools::exposure::current() else {
            return ToolResult::err("MCP invocation requires an execution context");
        };
        if !self.state.binding.matches(&context) {
            return ToolResult::err("MCP invocation handle is not valid for this session");
        }
        let Some(handle) = input.get("handle").and_then(Value::as_str) else {
            return ToolResult::err("missing `handle` field");
        };
        if handle.len() != 32
            || !handle
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return ToolResult::err("invalid MCP invocation handle");
        }
        let tool = match self.state.entries.lock() {
            Ok(entries) => entries.get(handle).map(|entry| entry.tool.clone()),
            Err(_) => return ToolResult::err("MCP disclosure registry is unavailable"),
        };
        let Some(tool) = tool else {
            return ToolResult::err("unknown or expired MCP invocation handle");
        };
        let arguments = input.get("arguments").cloned().unwrap_or(Value::Null);
        tool.exec(arguments).await
    }
}

fn register_disclosure_gateways(
    registry: &mut ToolRegistry,
    state: Arc<McpDisclosureState>,
) -> Result<(), String> {
    if state.is_empty() {
        return Ok(());
    }
    registry
        .register_unique(Arc::new(McpCatalogTool {
            state: state.clone(),
        }))
        .map_err(|_| "MCP catalog gateway name is already registered".to_string())?;
    registry
        .register_unique(Arc::new(McpInvokeTool { state }))
        .map_err(|_| "MCP invoke gateway name is already registered".to_string())
}

pub(crate) async fn verify_descriptor_stability(
    server: &str,
    client: &Arc<McpClient>,
    timeout_duration: Duration,
    expected_digest: &str,
) -> Result<(), String> {
    let listed = match timeout(timeout_duration, client.list_tools()).await {
        Ok(Ok(listed)) => listed,
        Ok(Err(_)) => {
            return Err(
                "MCP descriptor verification failed; tool execution was blocked".to_string(),
            )
        }
        Err(_) => {
            return Err(
                "MCP descriptor verification timed out; tool execution was blocked".to_string(),
            )
        }
    };
    let current = sanitize_descriptor_set(server, listed.tools).map_err(|_| {
        "MCP descriptor verification rejected the server response; tool execution was blocked"
            .to_string()
    })?;
    if current.digest != expected_digest {
        return Err(
                "MCP tool descriptors changed during this session; execution requires a new authorized attachment"
                    .to_string(),
            );
    }
    Ok(())
}

#[cfg(debug_assertions)]
#[doc(hidden)]
pub fn sanitized_descriptor_digest_for_test(
    server: &str,
    descriptors: Vec<ToolDescriptor>,
) -> Result<String, String> {
    sanitize_descriptor_set(server, descriptors).map(|set| set.digest)
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
async fn attach_server_into(
    spec: &McpServerSpec,
    disclosure: Option<&Arc<McpDisclosureState>>,
) -> Result<McpServerHandle, String> {
    if let Some(host) = crate::extension_host::client::current() {
        let attached = sanitize_descriptor_set(&spec.name, host.attach_mcp(spec.clone()).await?)?;
        let timeout = spec.timeout_duration();
        let transport = if spec.url.is_some() {
            ToolTransport::McpHttp
        } else {
            ToolTransport::McpStdio
        };
        if let Some(disclosure) = disclosure {
            for descriptor in attached.descriptors.iter().cloned() {
                disclosure.insert(
                    descriptor.clone(),
                    Arc::new(McpRemoteTool::new_hosted(
                        &spec.name,
                        descriptor,
                        timeout,
                        transport,
                        attached.digest.clone(),
                    )),
                )?;
            }
        }
        return Ok(McpServerHandle {
            client: None,
            child: None,
            name: spec.name.clone(),
            tool_count: attached.descriptors.len(),
            descriptors: attached.descriptors,
            descriptor_digest: attached.digest,
            timeout,
            hosted: true,
            _proc_session: None,
        });
    }
    if crate::paths::is_routed_job() {
        return Err(
            "the task extension host is unavailable; refusing to start MCP code in claw-agentd"
                .to_string(),
        );
    }
    attach_server_local(spec, disclosure).await
}

pub async fn attach_server(
    spec: &McpServerSpec,
    registry: &mut ToolRegistry,
    exposure: &ToolExposureContext,
) -> Result<McpServerHandle, String> {
    let disclosure = McpDisclosureState::new(exposure);
    let handle = attach_server_into(spec, Some(&disclosure)).await?;
    register_disclosure_gateways(registry, disclosure)?;
    Ok(handle)
}

pub(crate) async fn attach_server_local(
    spec: &McpServerSpec,
    disclosure: Option<&Arc<McpDisclosureState>>,
) -> Result<McpServerHandle, String> {
    if crate::paths::is_routed_job() {
        return Err(
            "MCP execution must be delegated to claw-extension-host; refusing to run it in claw-agentd"
                .to_string(),
        );
    }
    // Remote (HTTP/SSE) servers take a separate, child-less path.
    if spec.url.is_some() {
        return attach_http_server(spec, disclosure).await;
    }
    let proc_session = crate::bridge::McpProcSession::for_current_parent(&spec.command)?;
    let (program, initial_args) = if proc_session.is_some() {
        let mut args = vec![
            std::ffi::OsString::from("--"),
            std::ffi::OsString::from(&spec.command),
        ];
        args.extend(spec.args.iter().map(std::ffi::OsString::from));
        (crate::bridge::app_runner_path().into_os_string(), args)
    } else {
        (
            std::ffi::OsString::from(&spec.command),
            spec.args.iter().map(std::ffi::OsString::from).collect(),
        )
    };
    let launch = crate::extension_host::child_isolation::prepare(
        program,
        initial_args,
        spec.cwd.as_deref().map(std::path::Path::new),
    )?;
    let mut command = tokio::process::Command::new(launch.program);
    command.args(launch.args).envs(launch.env);
    // Wipe inherited environment then re-add an explicit allowlist.
    // The order is: env_clear → allowlist from os::env → spec.env
    // overlay. Caller-provided values win on collision.
    command.env_clear();
    for (k, v) in safe_env_allowlist() {
        command.env(k, v);
    }
    for (k, v) in &spec.env {
        if !reserved_environment_key(k) {
            command.env(k, v);
        }
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

    let attached = match sanitize_descriptor_set(&spec.name, tools.tools) {
        Ok(attached) => attached,
        Err(error) => {
            kill_and_reap(child);
            return Err(error);
        }
    };
    let mut registered = 0usize;
    for descriptor in attached.descriptors.iter().cloned() {
        if let Some(disclosure) = disclosure {
            if let Err(error) = disclosure.insert(
                descriptor.clone(),
                Arc::new(McpRemoteTool::new_with_transport(
                    &spec.name,
                    descriptor,
                    client.clone(),
                    timeout_dur,
                    ToolTransport::McpStdio,
                    attached.digest.clone(),
                )),
            ) {
                kill_and_reap(child);
                return Err(error);
            }
        }
        registered += 1;
    }

    Ok(McpServerHandle {
        client: Some(client),
        child: Some(child),
        name: spec.name.clone(),
        tool_count: registered,
        descriptors: attached.descriptors,
        descriptor_digest: attached.digest,
        timeout: timeout_dur,
        hosted: false,
        _proc_session: proc_session,
    })
}

/// Attach a **remote** MCP server over HTTP/SSE (Streamable HTTP).
/// Unlike the stdio path there is no child process — the transport
/// speaks JSON-RPC to `spec.url`. This is what lets the agent use
/// hosted MCP servers, not just local subprocesses. Optional bearer
/// auth is read from the env var named by `spec.bearer_env`, so tokens
/// never sit in an on-disk manifest.
pub(crate) async fn attach_http_server(
    spec: &McpServerSpec,
    disclosure: Option<&Arc<McpDisclosureState>>,
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
        .and_then(|var| {
            spec.env
                .get(var)
                .cloned()
                .or_else(|| std::env::var(var).ok())
        })
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

    let attached = sanitize_descriptor_set(&spec.name, tools.tools)?;
    let mut registered = 0usize;
    for descriptor in attached.descriptors.iter().cloned() {
        if let Some(disclosure) = disclosure {
            disclosure.insert(
                descriptor.clone(),
                Arc::new(McpRemoteTool::new_with_transport(
                    &spec.name,
                    descriptor,
                    client.clone(),
                    timeout_dur,
                    ToolTransport::McpHttp,
                    attached.digest.clone(),
                )),
            )?;
        }
        registered += 1;
    }

    Ok(McpServerHandle {
        client: Some(client),
        child: None,
        name: spec.name.clone(),
        tool_count: registered,
        descriptors: attached.descriptors,
        descriptor_digest: attached.digest,
        timeout: timeout_dur,
        hosted: false,
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
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LC_MESSAGES",
        "TZ",
        "TERM",
        "TMPDIR",
        "TEMP",
        "TMP",
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
        "COS_EXTENSION_CHILD_ISOLATION",
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
    ];
    for key in SAFE_COS {
        if let Ok(value) = std::env::var(key) {
            out.push(((*key).to_string(), value));
        }
    }
    out
}

fn reserved_environment_key(key: &str) -> bool {
    matches!(
        key,
        "COS_SESSION"
            | "COS_APP_ID"
            | "COS_PROC_DATA_DIR"
            | "COS_DATA_DIR"
            | "COS_HOME"
            | "HOME"
            | "USER"
            | "LOGNAME"
            | "PATH"
            | "COS_EXTENSION_CHILD_ISOLATION"
            | crate::extension_host::protocol::BROKER_SOCKET_ENV
    )
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
    exposure: &ToolExposureContext,
) -> Vec<McpServerHandle> {
    let disclosure = McpDisclosureState::new(exposure);
    let mut handles = Vec::with_capacity(specs.len());
    for spec in specs {
        match attach_server_into(spec, Some(&disclosure)).await {
            Ok(handle) => {
                tracing::info!(
                    "mcp `{}`: attached, disclosed {} capability handle(s)",
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
    if let Err(error) = register_disclosure_gateways(registry, disclosure) {
        tracing::error!(error = %error, "MCP disclosure gateways could not be registered");
        return Vec::new();
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
