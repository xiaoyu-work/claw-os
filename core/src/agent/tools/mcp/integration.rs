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
    /// Verified package this spec came from.
    ///
    /// `Some` for discovered agent-API packages: the manifest, the
    /// command, the scripts it points at and every other file in the
    /// package authenticated before the spec was built, and the same
    /// snapshot is re-checked immediately before spawn. `None` only for
    /// specs written directly into `config.json` by the machine owner,
    /// which are operator configuration rather than installed packages.
    pub provenance: Option<Arc<crate::provenance::VerifiedPackage>>,
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

/// The revocable identity of one attached MCP server.
///
/// Shared by every [`McpRemoteTool`] registered from that server and by
/// the [`McpServerHandle`] that owns the child, so a revocation noticed
/// during a tool call can close the transport and stop the process
/// rather than only refusing that one call. A server whose package has
/// been revoked is hostile code with a live stdio channel to the agent;
/// leaving it running and merely declining to talk to it would keep it
/// holding its sandbox, its cgroup and whatever it has already opened.
pub(crate) struct McpInstance {
    /// The session id the runtime record is keyed by. Synthetic
    /// (`mcp:<name>`) when the server runs without a proc session.
    session_id: String,
    name: String,
    class: crate::provenance::runtime::InstanceClass,
    /// `None` for operator-configured servers, which have no package
    /// and therefore nothing a revocation can name.
    package: Option<crate::provenance::runtime::PackageRef>,
    /// The owner this server's record belongs to, captured when it was
    /// attached rather than re-derived per call.
    owner: u32,
    closed: std::sync::atomic::AtomicBool,
}

impl McpInstance {
    fn new(
        session_id: String,
        name: String,
        package: Option<crate::provenance::runtime::PackageRef>,
    ) -> Self {
        let class = match package {
            Some(_) => crate::provenance::runtime::InstanceClass::McpPackage,
            None => crate::provenance::runtime::InstanceClass::McpOperatorConfig,
        };
        Self {
            session_id,
            name,
            class,
            package,
            owner: crate::provenance::runtime::current_owner(),
            closed: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Is this server still allowed to be talked to?
    ///
    /// Checked before every request, against a freshly resolved trust
    /// store — the resolver re-stats the durable generation and rebuilds
    /// when another process moved it, so a revocation lands here with no
    /// notification and no restart.
    fn assert_live(&self) -> Result<(), String> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(format!(
                "MCP server `{}` was shut down after its package was revoked",
                self.name
            ));
        }
        let trust = crate::provenance::trust_store();
        if let Some(package) = &self.package {
            package.is_live(&trust).inspect_err(|reason| {
                crate::provenance::runtime::mark_for_shutdown(self.owner, &self.session_id, reason);
            })?;
            // Package-backed: a missing record is a denial.
            return crate::provenance::runtime::assert_live_instance(
                self.owner,
                &self.session_id,
                &trust,
            );
        }
        crate::provenance::runtime::assert_live(self.owner, &self.session_id, &trust)
    }

    /// Close this server for good: mark it, kill its process group and
    /// clear its runtime record.
    ///
    /// Idempotent, and safe to call from an async context — the bounded
    /// wait for the group to exit runs on a blocking thread.
    async fn shut_down(&self, reason: &str) {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        crate::provenance::runtime::mark_for_shutdown(self.owner, &self.session_id, reason);
        crate::provenance::audit(
            "provenance.revoked_instance_denied",
            json!({
                "session": self.session_id,
                "surface": "mcp-tool-call",
                "class": self.class.as_str(),
                "server": self.name,
                "package_id": self.package.as_ref().map(|p| p.id.clone()),
                "content_digest": self.package.as_ref().map(|p| p.content_digest.clone()),
                "reason": reason,
            }),
        );
        let session = self.session_id.clone();
        let owner = self.owner;
        let _ = tokio::task::spawn_blocking(move || {
            crate::provenance::runtime::terminate(
                owner,
                &session,
                crate::provenance::runtime::SHUTDOWN_GRACE,
            )
        })
        .await;
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
    /// Shared with every tool registered from this server.
    instance: Option<Arc<McpInstance>>,
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
        // The runtime record describes something that is running. This
        // server is not, so it is dropped rather than left for a
        // lifecycle pass to reason about.
        if let Some(instance) = self.instance.as_ref() {
            crate::provenance::runtime::deregister(instance.owner, instance.session_id());
        }
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
    /// The server this tool belongs to, re-checked before every call.
    instance: Option<Arc<McpInstance>>,
}

impl McpRemoteTool {
    fn new(
        prefix: &str,
        descriptor: ToolDescriptor,
        client: Arc<McpClient>,
        timeout: Duration,
        instance: Option<Arc<McpInstance>>,
    ) -> Self {
        let name = format!("mcp_{prefix}_{}", descriptor.name);
        let description =
            sanitise_remote_description(&descriptor.description.unwrap_or_else(|| {
                format!(
                    "Remote MCP tool `{}` from server `{prefix}`.",
                    descriptor.name
                )
            }));
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
            instance,
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
        // Provenance before protocol. A server that was verified when
        // it was attached may have been revoked since; the check runs
        // per call, and a failure ends the server rather than just this
        // call — the transport closes and the child's process group is
        // signalled, so no further tool call can reach it either.
        if let Some(instance) = self.instance.as_ref() {
            if let Err(reason) = instance.assert_live() {
                instance.shut_down(&reason).await;
                return ToolResult::err(format!(
                    "MCP `{}` is no longer trusted and was shut down: {reason}",
                    self.name
                ));
            }
        }
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
///
/// MCP servers are third parties, so the body is fenced as
/// [`SourceKind::McpToolResult`] before it can reach the model. The
/// fence names the server prefix, is bounded, and cannot be closed from
/// inside the payload.
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
    let wrapped = crate::agent::safety::untrusted::wrap_labeled(
        crate::agent::trust::SourceKind::McpToolResult,
        server_prefix(tool_name),
        &body,
    );
    if res.is_error.unwrap_or(false) {
        ToolResult::err(wrapped)
    } else {
        ToolResult::ok(wrapped)
    }
}

/// Recover the server prefix from a registered `mcp_<server>_<remote>`
/// tool name, for the fence's source locator. Bounded again by
/// [`crate::audit_policy::safe_reference`] inside the fence, so a
/// hostile server name cannot widen the header.
fn server_prefix(tool_name: &str) -> Option<&str> {
    tool_name
        .strip_prefix("mcp_")
        .and_then(|rest| rest.split('_').next())
        .filter(|prefix| !prefix.is_empty())
}

/// Test seam: drive [`render_call_result`] with a text body without
/// standing up a server, so the ingestion inventory exercises the real
/// fencing path rather than a re-implementation of it.
#[cfg(test)]
pub(crate) fn render_call_result_for_test(
    tool_name: &str,
    text: &str,
    is_error: bool,
) -> ToolResult {
    use super::protocol::{CallToolResult, ContentItem};
    render_call_result(
        tool_name,
        CallToolResult {
            content: vec![ContentItem::Text {
                text: text.to_string(),
            }],
            is_error: Some(is_error),
        },
    )
}

/// Bound and sanitise remote-authored tool metadata before it becomes a
/// provider tool definition.
///
/// A description or schema from a remote server is
/// [`SourceKind::McpToolMetadata`]: extension metadata, never operator
/// rules, even when the package that declared the server is signed. A
/// signature authenticates the publisher, not the semantics of the
/// text. It reaches the model as a tool *definition* rather than as a
/// message, so it is bounded and stripped of fence markers instead of
/// being fenced.
pub(crate) fn sanitise_remote_description(description: &str) -> String {
    let defanged = crate::agent::trust::envelope::defang(description);
    let mut bounded = defanged
        .chars()
        .take(MAX_REMOTE_DESCRIPTION_CHARS)
        .collect::<String>();
    if defanged.chars().count() > MAX_REMOTE_DESCRIPTION_CHARS {
        bounded.push('…');
    }
    bounded
}

/// Longest remote-authored tool description kept for a tool definition.
const MAX_REMOTE_DESCRIPTION_CHARS: usize = 4096;

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
    // Re-assert the package immediately before spawn. Discovery may
    // have run minutes ago; a package directory replaced since then, a
    // revoked publisher key, or a mutated script must stop the launch
    // rather than be executed.
    let mut pinned_entries: Vec<(std::path::PathBuf, (u64, u64))> = Vec::new();
    // Descriptors on the verified files this server executes. They stay
    // open until the child has been spawned: the sandbox binds those
    // exact inodes, so replacing a script between resolution and
    // `execve` is refused rather than silently honoured.
    let _binding;
    let program = match spec.provenance.as_ref() {
        Some(pkg) => {
            let trust = crate::provenance::trust_store();
            pkg.assert_current(&trust).map_err(|e| {
                format!("MCP package `{}` failed its pre-spawn check: {e}", spec.name)
            })?;
            // Unsigned developer content is refused an MCP attach
            // outright: a running server holding a live broker endpoint
            // is a standing attack surface even with no capabilities.
            if !pkg.ceiling().allows_mcp_attach() {
                return Err(format!(
                    "MCP package `{}` is developer-trusted and may not be attached; \
                     sign it, or run it outside Claw OS",
                    spec.name
                ));
            }
            let required = package_relative_entries(pkg, spec)?;
            let binding = pkg
                .bind_for_launch(&required)
                .map_err(|e| format!("bind MCP package `{}`: {e}", spec.name))?;
            pinned_entries = binding.entries();
            let program = resolve_verified_program(pkg, &spec.command, &binding)?;
            _binding = Some(binding);
            program
        }
        None => {
            _binding = None;
            resolve_mcp_program(&spec.command)?
        }
    };
    if let Some(pkg) = spec.provenance.as_ref() {
        crate::provenance::audit("provenance.mcp_attach", pkg.audit_facts());
    }
    let policy = crate::worker::derive::mcp_server(crate::worker::derive::McpServerInput {
        pinned_entries,
        name: &spec.name,
        program,
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
    // The revocable identity of this server, keyed by the session the
    // runtime record uses. A server without a proc session still gets a
    // stable synthetic key so it is recorded, classified and sweepable
    // rather than invisible.
    let instance_session = proc_session
        .as_ref()
        .map(|session| session.id().to_string())
        .unwrap_or_else(|| format!("mcp:{}", spec.name));
    let package_ref = spec
        .provenance
        .as_ref()
        .map(|pkg| crate::provenance::runtime::PackageRef::of(pkg));
    let owner = crate::provenance::runtime::current_owner();
    match spec.provenance.as_ref() {
        Some(pkg) => {
            crate::provenance::runtime::register_mcp_package(owner, &instance_session, pkg)
        }
        // Operator configuration, not an installed package: classified
        // explicitly so `cos provenance` can say what is running under
        // which policy instead of leaving a gap in the records.
        None => crate::provenance::runtime::register_operator_mcp(owner, &instance_session),
    }
    let instance = Arc::new(McpInstance::new(
        instance_session.clone(),
        spec.name.clone(),
        package_ref.clone(),
    ));

    let mut launch = crate::worker::WorkerLaunch::new(policy);
    if let Some(session) = proc_session.as_ref() {
        // An MCP server holds no standing capabilities. Its authority
        // is whatever the kernel has installed on the session at the
        // instant of the call — nothing at rest, and a session tool
        // call's transient set only while that call is in flight. The
        // endpoint reads it live and relays under the launcher's grant,
        // so clearing the transient set removes it immediately.
        launch = launch.with_authority(session.broker_authority().with_package(package_ref));
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
    let Some(child_pid) = child.id() else {
        crate::provenance::runtime::deregister(owner, &instance_session);
        kill_and_reap(child);
        return Err("spawned MCP server has no pid".to_string());
    };
    // Recorded while the child is still unreaped, so the identity read
    // here belongs to this process and cannot already have been
    // recycled onto something else.
    crate::provenance::runtime::bind_process(owner, &instance_session, child_pid);
    if let Some(session) = proc_session.as_ref() {
        if let Err(error) = session.bind_process(child_pid) {
            crate::provenance::runtime::deregister(owner, &instance_session);
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
            crate::provenance::runtime::deregister(owner, &instance_session);
            kill_and_reap(child);
            return Err(format!("initialize: {}", render_client_err(e)));
        }
        Err(_) => {
            crate::provenance::runtime::deregister(owner, &instance_session);
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
            crate::provenance::runtime::deregister(owner, &instance_session);
            kill_and_reap(child);
            return Err(format!("tools/list: {}", render_client_err(e)));
        }
        Err(_) => {
            crate::provenance::runtime::deregister(owner, &instance_session);
            kill_and_reap(child);
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
            Some(Arc::clone(&instance)),
        );
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
        instance: Some(instance),
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
        // A remote server is an endpoint the owner configured, not a
        // package and not a process: there is nothing for provenance to
        // revoke and nothing to signal, so no instance is attached.
        let tool = McpRemoteTool::new(&spec.name, descriptor, client.clone(), timeout_dur, None);
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
        instance: None,
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
/// Resolve the program for a *verified package*.
///
/// Two cases, and only two:
///
///   * the command names a file inside the package — it must be a
///     signed entrypoint, and its digest is re-checked from the pinned
///     directory descriptor before the path is handed to the sandbox;
///   * the command names a system interpreter — it is resolved on
///     `PATH` and then required to be a root-owned, non-symlink,
///     non-group/world-writable binary under an approved system root.
///
/// A writable interpreter earlier on `PATH`, or a script outside the
/// package, is refused. Provenance would be worthless if the signed
/// bytes were then run through an attacker-controlled `python3`.
fn resolve_verified_program(
    pkg: &crate::provenance::VerifiedPackage,
    command: &str,
    binding: &crate::provenance::verify::LaunchBinding,
) -> Result<std::path::PathBuf, String> {
    if let Some(rel) = package_relative(pkg, command) {
        // A package-relative program must be a *declared* entrypoint,
        // already bound by inode above. Re-resolving the path here
        // would reopen the TOCTOU the binding exists to close.
        let path = pkg.dir().join(&rel);
        if binding.identity_for(&path).is_none() {
            return Err(format!(
                "MCP command `{rel}` is not a declared, signed entrypoint of package `{}`",
                pkg.id()
            ));
        }
        return Ok(path);
    }
    let resolved = resolve_mcp_program(command)?;
    require_system_interpreter(&resolved)?;
    Ok(resolved)
}

/// Every package-relative path this spec will execute or read.
///
/// The program itself when it lives in the package, plus any argv entry
/// or env value pointing inside it. Each must be a declared entrypoint
/// in the envelope, so a package cannot run a helper script it never
/// declared.
fn package_relative_entries(
    pkg: &crate::provenance::VerifiedPackage,
    spec: &McpServerSpec,
) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let mut consider = |value: &str, what: &str| -> Result<(), String> {
        if let Some(rel) = package_relative(pkg, value) {
            if !pkg.entrypoints().iter().any(|e| e == &rel) {
                return Err(format!(
                    "MCP {what} `{rel}` is not a declared entrypoint of package `{}`; \
                     add it to the package's signed entrypoints",
                    pkg.id()
                ));
            }
            if !out.contains(&rel) {
                out.push(rel);
            }
        }
        Ok(())
    };
    consider(&spec.command, "command")?;
    for arg in &spec.args {
        consider(arg, "argument")?;
    }
    for value in spec.env.values() {
        consider(value, "environment path")?;
    }
    Ok(out)
}

/// Approved roots for a system interpreter or binary. These are
/// package-manager territory: writable only by root.
const SYSTEM_BINARY_ROOTS: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/opt"];

fn require_system_interpreter(path: &std::path::Path) -> Result<(), String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("resolve MCP interpreter {}: {e}", path.display()))?;
    if !SYSTEM_BINARY_ROOTS
        .iter()
        .any(|root| canonical.starts_with(root))
    {
        return Err(format!(
            "MCP interpreter {} is outside the approved system roots {SYSTEM_BINARY_ROOTS:?}; \
             a package may only run a distribution-installed interpreter or its own signed entrypoint",
            canonical.display()
        ));
    }
    crate::provenance::fsec::require_secure_location(&canonical, &[0])
        .map_err(|e| format!("MCP interpreter rejected: {e}"))?;
    Ok(())
}

/// Map an absolute path back to its package-relative form, or `None`
/// when it lies outside the package.
fn package_relative(
    pkg: &crate::provenance::VerifiedPackage,
    value: &str,
) -> Option<String> {
    let candidate = std::path::Path::new(value);
    if !candidate.is_absolute() {
        return None;
    }
    let root = pkg.dir().canonicalize().ok()?;
    let resolved = candidate.canonicalize().ok()?;
    let rel = resolved.strip_prefix(&root).ok()?;
    let rel = rel.to_str()?.replace('\\', "/");
    if rel.is_empty() {
        None
    } else {
        Some(rel)
    }
}

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
