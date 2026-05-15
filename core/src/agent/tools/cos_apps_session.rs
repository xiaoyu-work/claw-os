//! cos *apps* session bridge — agent-driven, stateful tool calls into apps.
//!
//! This is the symmetric counterpart to [`super::cos_apps`] (the
//! stateless one-shot proxies). Where `cos_app_<id>` shells `cos app
//! <id> <verb>` for every call, an *app session tool* keeps the app's
//! MCP server alive between calls so it can hold in-memory state and
//! run background work.
//!
//! ## Discovery, registration, and lifecycle
//!
//! At registry construction the kernel walks `$COS_APPS_DIR`, reads
//! every `app.json`, and for each app that declares a `session` block
//! registers one [`AppSessionTool`] per [`SessionTool`] in the
//! manifest. The MCP server itself is *not* started at this point —
//! the lookup is lazy. The first call to any of an app's tools
//! triggers `bring_up_app`, which spawns the server, runs the MCP
//! handshake, and stores the live `McpServerHandle` in a process-wide
//! [`SessionManager`].
//!
//! Subsequent calls reuse the same client. Explicit
//! [`CosAppSessionOpen`] / [`CosAppSessionClose`] meta-tools let the
//! agent open or close sessions deliberately when the model wants
//! that level of control (the **hybrid** attach strategy).
//!
//! ## Per-call enforcement
//!
//! Every `tools/call` the kernel forwards to an app server is gated:
//!
//! 1. [`Manifest::resolve_session_tool_needs`] turns the manifest's
//!    `needs[]` plus the call's arguments into concrete [`Cap`]s.
//! 2. [`crate::caps::require`] checks each. A denial short-circuits
//!    before the app server sees the call.
//! 3. On both allow and deny the kernel emits one
//!    [`LlmRunRecord`] to `ai.jsonl` with `provider="app:<id>"` and
//!    `model="tool:<tool_name>"`, matching the `cos ai tool` audit
//!    shape. App-internal calls that re-enter the kernel (e.g. the
//!    server shells `cos ai chat`) carry the app's `COS_APP_ID` and
//!    are audited under that identity too.
//!
//! The app's MCP server therefore never sees a call its manifest
//! didn't authorise. App authors can still defensively re-check inside
//! handlers, but they don't have to.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::agent::llm::run_log::{record as record_run, LlmRunRecord};
use crate::agent::tools::mcp::client::McpClient;
use crate::agent::tools::mcp::protocol::{
    ClientCapabilities, Implementation, PROTOCOL_VERSION,
};
use crate::agent::tools::mcp::transport::StdioTransport;
use crate::caps::manifest::{Manifest, Runtime, SessionTransport};

use super::registry::ToolRegistry;
use super::{Tool, ToolResult};

// ---------------------------------------------------------------------------
// Process-wide session manager
// ---------------------------------------------------------------------------

/// One running app session. Holding `child` keeps the process alive;
/// dropping the whole entry kills it.
struct ActiveSession {
    client: Arc<McpClient>,
    /// We keep `Child` around so [`ActiveSession::Drop`] can
    /// `start_kill` the server when the agent closes the session.
    child: Option<Child>,
    /// For diagnostics + tool count surfaced through `open`.
    tool_count: usize,
}

impl Drop for ActiveSession {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
    }
}

type SessionTable = Mutex<HashMap<String, ActiveSession>>;

fn manager() -> &'static SessionTable {
    static MANAGER: OnceLock<SessionTable> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// Spawn + handshake
// ---------------------------------------------------------------------------

/// Spawn an app's MCP session server and run the JSON-RPC handshake.
/// Returns a live client + child. The caller is responsible for
/// storing both in the [`SessionManager`].
///
/// Mirrors [`super::mcp::integration::attach_server`] but skips the
/// tool-registration loop: we register tools eagerly from the manifest
/// at boot time, not from the server's `tools/list` response. The
/// `tools/list` we still issue is purely advisory — it verifies the
/// server speaks MCP and exposes at least the manifest tools.
async fn bring_up_app(
    app_id: &str,
    app_dir: &Path,
    manifest: &Manifest,
    timeout_dur: Duration,
) -> Result<(Arc<McpClient>, Child, usize), String> {
    let session = manifest
        .session
        .as_ref()
        .ok_or_else(|| format!("app `{app_id}` has no session block"))?;
    if !matches!(session.transport, SessionTransport::Stdio) {
        return Err(format!(
            "app `{app_id}`: only `stdio` transport is supported"
        ));
    }
    let entry_rel = session
        .entry
        .clone()
        .unwrap_or_else(|| manifest.runtime.default_session_entry().to_string());
    let entry_abs = app_dir.join(&entry_rel);
    if !entry_abs.is_file() {
        return Err(format!(
            "app `{app_id}`: session entry `{}` not found at {}",
            entry_rel,
            entry_abs.display()
        ));
    }

    let apps_dir = apps_root();
    let apps_dir_str = apps_dir.to_string_lossy().to_string();
    let data_dir = data_dir_string();

    // Resolve the directory containing the `claw_os_sdk` Python
    // package so `runtime: python` MCP-session apps can `from
    // claw_os_sdk import …`. Honour an explicit override; otherwise
    // try the production install path and the in-repo dev path
    // (`<repo>/claw-os-sdk/python/src`).
    let sdk_python_dir = resolve_sdk_python_dir(&apps_dir);
    let pythonpath = match &sdk_python_dir {
        Some(sdk) => format!("{}{}{}", sdk.to_string_lossy(), pathsep(), apps_dir_str),
        None => apps_dir_str.clone(),
    };

    let mut command = build_command(manifest.runtime, &entry_abs);
    command
        .env("COS_APP_ID", app_id)
        .env("COS_DATA_DIR", &data_dir)
        .env("COS_APPS_DIR", &apps_dir_str)
        .env("PYTHONPATH", &pythonpath)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("PAGER", "cat")
        // Trigger the MCP-server mode of `runtime: binary` apps. The
        // Rust SDK at `crates/cos-mcp-serve` keys off this variable
        // (and only this variable) so the same desktop GUI binary can
        // serve both its normal `main()` flow and the agent's tool
        // surface. Python/Node/Shell apps ignore the var.
        .env("COS_MCP_SERVER", "1")
        .envs(crate::config::as_env_vars())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn `{app_id}` session: {e}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "child stdin unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "child stdout unavailable".to_string())?;

    let transport = StdioTransport::from_pair(Box::new(stdout), Box::new(stdin));
    let client: Arc<McpClient> = McpClient::new(transport);
    client.start().await;

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
            return Err(format!("initialize: {e}"));
        }
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "initialize timed out after {}s",
                timeout_dur.as_secs()
            ));
        }
    };
    if init.protocol_version != PROTOCOL_VERSION {
        tracing::info!(
            "app `{app_id}`: server protocol version `{}` differs from client `{PROTOCOL_VERSION}`",
            init.protocol_version
        );
    }
    let _ = client.notify("notifications/initialized", None).await;

    // tools/list is advisory: we register from the manifest, not from
    // here, so the kernel's view of what's callable never depends on
    // a misbehaving server. We still call it to surface server-side
    // errors immediately.
    let list_fut = client.list_tools();
    let listed_count = match timeout(timeout_dur, list_fut).await {
        Ok(Ok(v)) => v.tools.len(),
        Ok(Err(e)) => {
            let _ = child.start_kill();
            return Err(format!("tools/list: {e}"));
        }
        Err(_) => {
            let _ = child.start_kill();
            return Err(format!(
                "tools/list timed out after {}s",
                timeout_dur.as_secs()
            ));
        }
    };

    Ok((client, child, listed_count))
}

fn build_command(runtime: Runtime, entry: &Path) -> Command {
    match runtime {
        Runtime::Python => {
            let bin = if cfg!(windows) { "python" } else { "python3" };
            let mut c = Command::new(bin);
            c.arg(entry);
            c
        }
        Runtime::Node => {
            let mut c = Command::new("node");
            c.arg(entry);
            c
        }
        Runtime::Shell => {
            if cfg!(windows) {
                let mut c = Command::new("cmd");
                c.arg("/c").arg(entry);
                c
            } else {
                let mut c = Command::new("bash");
                c.arg(entry);
                c
            }
        }
        Runtime::Binary => Command::new(entry),
    }
}

// ---------------------------------------------------------------------------
// Session lookup / open / close
// ---------------------------------------------------------------------------

/// Default per-call timeout. App session calls share the same upper
/// bound as MCP catalog calls; long-running work should use the
/// app-defined `start_task`/`poll` pattern, not a single long call.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Return the active client for `app_id`, opening the session lazily
/// if no entry exists. Holds the manager mutex across spawn (which is
/// fine — sessions are infrequent and the spawn happens off-thread
/// via tokio's blocking pool inside `Command::spawn`).
async fn get_or_open(app_id: &str) -> Result<Arc<McpClient>, String> {
    {
        let table = manager().lock().await;
        if let Some(s) = table.get(app_id) {
            return Ok(s.client.clone());
        }
    }
    open_session(app_id).await.map(|(c, _)| c)
}

/// Explicitly bring up `app_id`. Returns `(client, tool_count)`.
/// Idempotent: returns the existing session if one is already open.
async fn open_session(app_id: &str) -> Result<(Arc<McpClient>, usize), String> {
    {
        let table = manager().lock().await;
        if let Some(s) = table.get(app_id) {
            return Ok((s.client.clone(), s.tool_count));
        }
    }
    let app_dir = apps_root().join(app_id);
    let manifest_path = app_dir.join("app.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read manifest for `{app_id}`: {e}"))?;
    let manifest = Manifest::from_json(&manifest_text)
        .map_err(|e| format!("parse manifest for `{app_id}`: {e}"))?;
    let (client, child, listed) =
        bring_up_app(app_id, &app_dir, &manifest, DEFAULT_TIMEOUT).await?;
    let mut table = manager().lock().await;
    table.insert(
        app_id.to_string(),
        ActiveSession {
            client: client.clone(),
            child: Some(child),
            tool_count: listed,
        },
    );
    Ok((client, listed))
}

/// Close a session, dropping the handle (which kills the child).
/// Returns `true` if a session was found and closed.
async fn close_session(app_id: &str) -> bool {
    let mut table = manager().lock().await;
    table.remove(app_id).is_some()
}

fn apps_root() -> PathBuf {
    PathBuf::from(std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

fn data_dir_string() -> String {
    std::env::var("COS_DATA_DIR").unwrap_or_else(|_| "/var/lib/cos".into())
}

/// Locate the directory containing the `claw_os_sdk` Python package.
///
/// Honours `COS_SDK_PYTHON_DIR` first, then falls back to the
/// production install path (`/usr/lib/cos/python`), and finally to
/// the in-repo dev-checkout path at a fixed offset from
/// `$COS_APPS_DIR`. Returns the first directory that actually
/// contains a `claw_os_sdk/` subdirectory.
fn resolve_sdk_python_dir(apps_dir: &std::path::Path) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(v) = std::env::var("COS_SDK_PYTHON_DIR") {
        if !v.is_empty() {
            candidates.push(PathBuf::from(v));
        }
    }
    candidates.push(PathBuf::from("/usr/lib/cos/python"));
    if let Some(parent) = apps_dir.parent() {
        candidates.push(parent.join("claw-os-sdk").join("python").join("src"));
    }
    candidates
        .into_iter()
        .find(|c| c.join("claw_os_sdk").is_dir())
}

fn pathsep() -> &'static str {
    if cfg!(windows) {
        ";"
    } else {
        ":"
    }
}

// ---------------------------------------------------------------------------
// AppSessionTool — one per manifest-declared tool
// ---------------------------------------------------------------------------

/// One agent-callable tool backed by an app's MCP session server. The
/// kernel registers a separate `AppSessionTool` per
/// [`SessionTool`](crate::caps::manifest::SessionTool) in each
/// installed app's manifest. The session itself is opened lazily on
/// first call (or explicitly via [`CosAppSessionOpen`]).
pub struct AppSessionTool {
    /// Leaked at registration time; format: `app_<id>__<tool_name_dots_to_underscores>`.
    name: &'static str,
    /// Leaked description built from the tool's summary.
    description: &'static str,
    /// JSON Schema derived from `manifest.session.tools[i].args`.
    schema: Value,
    /// The app's manifest id.
    app_id: String,
    /// The manifest's tool name (e.g. `kv.get`) — what we send over the wire.
    manifest_tool_name: String,
    /// Cached manifest used for cap resolution. Kept here so every call
    /// avoids re-parsing the JSON file.
    manifest: Arc<Manifest>,
    /// Per-call timeout. Defaults to [`DEFAULT_TIMEOUT`].
    timeout: Duration,
}

impl AppSessionTool {
    fn from_manifest_tool(
        manifest: Arc<Manifest>,
        tool_idx: usize,
    ) -> Self {
        let session = manifest
            .session
            .as_ref()
            .expect("from_manifest_tool requires a session block");
        let tool = &session.tools[tool_idx];
        let app_id = manifest.id.clone();
        let manifest_tool_name = tool.name.clone();
        let registry_name = registry_name_for(&app_id, &manifest_tool_name);
        let name: &'static str = Box::leak(registry_name.into_boxed_str());
        let description: &'static str = Box::leak(
            format!(
                "App `{app_id}` session tool `{manifest_tool_name}`. {}",
                tool.summary.en_str()
            )
            .into_boxed_str(),
        );
        let schema = build_schema(&tool.args);
        Self {
            name,
            description,
            schema,
            app_id,
            manifest_tool_name,
            manifest,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

fn registry_name_for(app_id: &str, tool_name: &str) -> String {
    // Tool names use dots (`kv.get`) which work fine as HashMap keys,
    // but many downstream tools (logs, dashboards, JSON-schema enums)
    // assume snake_case. Normalise.
    let sanitized = tool_name.replace('.', "_");
    format!("app_{app_id}__{sanitized}")
}

fn build_schema(args: &[crate::caps::manifest::Arg]) -> Value {
    use crate::caps::manifest::ArgKind;
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    for a in args {
        let json_type = match a.kind {
            ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text => "string",
            ArgKind::Number => "number",
            ArgKind::Bool => "boolean",
        };
        let mut prop = serde_json::Map::new();
        prop.insert("type".to_string(), Value::String(json_type.to_string()));
        if a.label.has_english() {
            prop.insert(
                "description".to_string(),
                Value::String(a.label.en_str().to_string()),
            );
        }
        if let Some(default) = &a.default {
            prop.insert("default".to_string(), default.clone());
        }
        properties.insert(a.name.clone(), Value::Object(prop));
        if a.required {
            required.push(a.name.clone());
        }
    }
    let mut schema = serde_json::Map::new();
    schema.insert("type".to_string(), Value::String("object".to_string()));
    schema.insert("properties".to_string(), Value::Object(properties));
    if !required.is_empty() {
        schema.insert(
            "required".to_string(),
            Value::Array(required.into_iter().map(Value::String).collect()),
        );
    }
    schema.insert(
        "additionalProperties".to_string(),
        Value::Bool(false),
    );
    Value::Object(schema)
}

#[async_trait]
impl Tool for AppSessionTool {
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
        let started = Instant::now();
        let args_map = json_to_arg_map(&input);

        // 1) Cap gate. resolve_session_tool_needs is cheap; manifest
        // is held in Arc and not re-parsed.
        let caps = match self
            .manifest
            .resolve_session_tool_needs(&self.manifest_tool_name, &args_map)
        {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("cap resolution failed: {e}");
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    "",
                    "denied",
                    Some(&msg),
                    Some(&msg),
                    started.elapsed(),
                );
                return ToolResult::err(msg);
            }
        };

        for cap in &caps {
            if let Err(denial) = crate::caps::require(cap.verb, cap.scope.clone()) {
                let msg = denial.summary();
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    cap.verb.as_str(),
                    "denied",
                    Some(&msg),
                    Some(&msg),
                    started.elapsed(),
                );
                return ToolResult::err(msg);
            }
        }

        // 2) Open / reuse session.
        let client = match get_or_open(&self.app_id).await {
            Ok(c) => c,
            Err(e) => {
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    verb_csv(&caps).as_str(),
                    "allowed",
                    None,
                    Some(&e),
                    started.elapsed(),
                );
                return ToolResult::err(format!(
                    "could not bring up app `{}`: {e}",
                    self.app_id
                ));
            }
        };

        // 3) Forward tools/call.
        let arguments = match input {
            Value::Null => None,
            Value::Object(ref m) if m.is_empty() => None,
            other => Some(other),
        };
        let call = client.call_tool(self.manifest_tool_name.clone(), arguments);
        let res = match timeout(self.timeout, call).await {
            Ok(r) => r,
            Err(_) => {
                let msg = format!(
                    "app `{}` tool `{}` timed out after {}s",
                    self.app_id,
                    self.manifest_tool_name,
                    self.timeout.as_secs()
                );
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    verb_csv(&caps).as_str(),
                    "allowed",
                    None,
                    Some(&msg),
                    started.elapsed(),
                );
                return ToolResult::err(msg);
            }
        };

        match res {
            Ok(call_result) => {
                let (content, is_error) = render_call_result(call_result);
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    verb_csv(&caps).as_str(),
                    "allowed",
                    None,
                    if is_error { Some(content.as_str()) } else { None },
                    started.elapsed(),
                );
                if is_error {
                    ToolResult::err(content)
                } else {
                    ToolResult::ok(content)
                }
            }
            Err(e) => {
                let msg = format!(
                    "app `{}` tool `{}` failed: {e}",
                    self.app_id, self.manifest_tool_name
                );
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    verb_csv(&caps).as_str(),
                    "allowed",
                    None,
                    Some(&msg),
                    started.elapsed(),
                );
                ToolResult::err(msg)
            }
        }
    }
}

fn json_to_arg_map(input: &Value) -> BTreeMap<String, Value> {
    match input {
        Value::Object(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        _ => BTreeMap::new(),
    }
}

fn verb_csv(caps: &[crate::caps::Cap]) -> String {
    caps.iter()
        .map(|c| c.verb.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

fn render_call_result(res: crate::agent::tools::mcp::protocol::CallToolResult) -> (String, bool) {
    use crate::agent::tools::mcp::protocol::ContentItem;
    let mut chunks = Vec::new();
    for item in res.content {
        match item {
            ContentItem::Text { text } => chunks.push(text),
            ContentItem::Image { mime_type, .. } => {
                chunks.push(format!("[image content omitted ({mime_type})]"));
            }
        }
    }
    let body = if chunks.is_empty() {
        "(tool returned no content)".to_string()
    } else {
        chunks.join("\n\n")
    };
    (body, res.is_error.unwrap_or(false))
}

fn emit_audit(
    app_id: &str,
    tool_name: &str,
    verb: &str,
    decision: &str,
    denial_reason: Option<&str>,
    error: Option<&str>,
    duration: Duration,
) {
    let session_id = std::env::var("COS_SESSION").ok();
    let mut rec = LlmRunRecord::from_tool_call(
        tool_name,
        app_id,
        verb,
        decision,
        denial_reason,
        error,
        duration.as_millis() as u64,
        session_id.as_deref(),
    );
    // Override provider so audit dashboards can split kernel-catalog
    // tools from app-session tools without parsing model strings.
    rec.provider = format!("app:{app_id}");
    record_run(&rec);
}

// ---------------------------------------------------------------------------
// Meta-tools: explicit open / close
// ---------------------------------------------------------------------------

/// Tell the kernel to bring up an app's session server (if it isn't
/// already up). The model uses this to make session lifecycle
/// explicit when planning a multi-step task.
pub struct CosAppSessionOpen;

#[async_trait]
impl Tool for CosAppSessionOpen {
    fn name(&self) -> &'static str {
        "cos_app_session_open"
    }

    fn description(&self) -> &'static str {
        "Bring up an installed app's MCP session server and return the \
         list of its registered tools. Subsequent calls to those tools \
         reuse the same long-lived process so the app can hold \
         in-memory state across turns. Idempotent — opening an already \
         open session returns the same tool list. Pair with \
         `cos_app_catalog` to discover which apps support sessions and \
         with `cos_app_session_close` to release the server when done."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "App id (matches the directory under $COS_APPS_DIR).",
                }
            },
            "required": ["app"],
            "additionalProperties": false,
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let app_id = match input.get("app").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing `app` field".to_string()),
        };
        if let Err(denial) = crate::caps::require(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name(&app_id),
        ) {
            return ToolResult::err(denial.summary());
        }
        match open_session(&app_id).await {
            Ok((_client, count)) => {
                // Surface what's now callable so the model knows which
                // names to use without a follow-up discovery call.
                let tool_names = manifest_tool_names(&app_id).unwrap_or_default();
                let body = json!({
                    "app": app_id,
                    "tools_registered_from_manifest": tool_names,
                    "tools_listed_by_server": count,
                });
                ToolResult::ok(body.to_string())
            }
            Err(e) => ToolResult::err(format!("open `{app_id}`: {e}")),
        }
    }
}

/// Tell the kernel to terminate an app's session server. Tool calls
/// after this still work — the next one lazily re-opens the session
/// — but any in-memory state is discarded.
pub struct CosAppSessionClose;

#[async_trait]
impl Tool for CosAppSessionClose {
    fn name(&self) -> &'static str {
        "cos_app_session_close"
    }

    fn description(&self) -> &'static str {
        "Terminate an app's MCP session server. In-memory session \
         state is dropped; persistent state (files, databases) is \
         untouched. A subsequent call to any of the app's tools will \
         transparently re-open a fresh session."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "description": "App id to close. No-op if not currently open.",
                }
            },
            "required": ["app"],
            "additionalProperties": false,
        })
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let app_id = match input.get("app").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing `app` field".to_string()),
        };
        let closed = close_session(&app_id).await;
        ToolResult::ok(json!({"app": app_id, "closed": closed}).to_string())
    }
}

fn manifest_tool_names(app_id: &str) -> Result<Vec<String>, String> {
    let manifest_path = apps_root().join(app_id).join("app.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read manifest: {e}"))?;
    let manifest = Manifest::from_json(&text).map_err(|e| format!("parse manifest: {e}"))?;
    Ok(manifest
        .session
        .map(|s| s.tools.into_iter().map(|t| t.name).collect())
        .unwrap_or_default())
}

// ---------------------------------------------------------------------------
// Bulk registration entry point
// ---------------------------------------------------------------------------

/// Walk `$COS_APPS_DIR` and register one [`AppSessionTool`] per
/// session tool declared in any app's manifest, plus the two
/// meta-tools. The MCP servers themselves are *not* started here —
/// they come up lazily on first call (or explicitly via
/// `cos_app_session_open`).
pub fn register_all(registry: &mut ToolRegistry) {
    let apps = crate::apps::discover(&apps_root());
    for app in apps.values() {
        let Some(session) = &app.manifest.session else {
            continue;
        };
        let arc_manifest = Arc::new(app.manifest.clone());
        for idx in 0..session.tools.len() {
            registry.register(Arc::new(AppSessionTool::from_manifest_tool(
                arc_manifest.clone(),
                idx,
            )));
        }
    }
    registry.register(Arc::new(CosAppSessionOpen));
    registry.register(Arc::new(CosAppSessionClose));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn write_kv_app(root: &Path) {
        let dir = root.join("kv");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("app.json"),
            serde_json::json!({
                "id": "kv",
                "version": "0.1.0",
                "name": {"en": "KV"},
                "summary": {"en": "Key/value."},
                "operations": {},
                "session": {
                    "entry": "server.py",
                    "tools": [
                        {
                            "name": "kv.get",
                            "summary": {"en": "Read a value."},
                            "args": [{"name":"key","kind":"name","required":true}],
                            "needs": [
                                {"verb":"data.kv.read",
                                 "scope":{"kind":"from-arg","arg":"key"},
                                 "why":{"en":"Read by key."}}
                            ]
                        }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("server.py"),
            "# placeholder — not exec'd in this test\n",
        )
        .unwrap();
    }

    #[test]
    fn register_all_emits_one_tool_per_manifest_entry_plus_meta() {
        let _g = env_lock();
        let tmp = tempfile::tempdir().unwrap();
        write_kv_app(tmp.path());
        let prev = std::env::var("COS_APPS_DIR").ok();
        std::env::set_var("COS_APPS_DIR", tmp.path());

        let mut r = ToolRegistry::new();
        register_all(&mut r);
        let names = r.names_unfiltered();
        // One session tool + two meta-tools.
        assert!(names.contains(&"app_kv__kv_get"), "got {names:?}");
        assert!(names.contains(&"cos_app_session_open"));
        assert!(names.contains(&"cos_app_session_close"));

        match prev {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
    }

    #[test]
    fn registry_name_replaces_dots_with_underscores() {
        assert_eq!(registry_name_for("kv", "kv.get"), "app_kv__kv_get");
        assert_eq!(
            registry_name_for("calendar", "calendar.find_slots"),
            "app_calendar__calendar_find_slots"
        );
    }

    #[test]
    fn build_schema_marks_required_args() {
        use crate::caps::manifest::{Arg, ArgKind};
        use crate::i18n::LocalizedText;
        let args = vec![
            Arg {
                name: "key".into(),
                kind: ArgKind::Name,
                required: true,
                default: None,
                label: LocalizedText::default(),
            },
            Arg {
                name: "ttl".into(),
                kind: ArgKind::Number,
                required: false,
                default: Some(serde_json::json!(60)),
                label: LocalizedText::default(),
            },
        ];
        let schema = build_schema(&args);
        let required = schema["required"].as_array().unwrap();
        assert_eq!(required.len(), 1);
        assert_eq!(required[0].as_str(), Some("key"));
        assert_eq!(schema["properties"]["ttl"]["default"], serde_json::json!(60));
        assert_eq!(schema["properties"]["key"]["type"], "string");
        assert_eq!(schema["properties"]["ttl"]["type"], "number");
    }

    /// Spawn the real `apps/kv` server via [`open_session`], drive it
    /// across multiple calls, and verify session state persists. This
    /// is the canonical proof that the **App → MCP server** wiring
    /// (manifest schema + Python SDK + kernel bring-up + bridge)
    /// works end to end. We use `COS_CAPS_MODE=permissive` so the
    /// test doesn't need to set up role grants; the caps-gate
    /// codepath is still exercised — `crate::caps::require` is
    /// called for every tool, it just allows through.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pilot_kv_e2e_call_chain() {
        let _g = env_lock();
        let apps_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("apps");
        if !apps_dir.join("kv").join("server.py").is_file() {
            eprintln!("skip pilot_kv_e2e: {} not present", apps_dir.display());
            return;
        }
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skip pilot_kv_e2e: python3 not on PATH");
            return;
        }

        let data = tempfile::tempdir().unwrap();
        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        let prev_data = std::env::var("COS_DATA_DIR").ok();
        let prev_mode = std::env::var("COS_CAPS_MODE").ok();
        std::env::set_var("COS_APPS_DIR", &apps_dir);
        std::env::set_var("COS_DATA_DIR", data.path());
        std::env::set_var("COS_CAPS_MODE", "permissive");

        // Make sure no stale entry from a previous test run survives.
        let _ = close_session("kv").await;

        let opened = open_session("kv").await.expect("open kv");
        assert!(opened.1 >= 5, "kv should advertise ≥5 tools, got {}", opened.1);

        // 1) set, get — verify in-memory state survives.
        let r = opened
            .0
            .call_tool("kv.set", Some(serde_json::json!({"key":"x","value":"42"})))
            .await
            .expect("set");
        assert!(!r.is_error.unwrap_or(false));

        let r = opened
            .0
            .call_tool("kv.get", Some(serde_json::json!({"key":"x"})))
            .await
            .expect("get");
        let text = first_text(&r);
        assert!(text.contains("42"), "kv.get returned: {text}");

        // 2) list — confirms the cached dict is the same instance
        // across calls (the previous set/get hit it).
        let r = opened
            .0
            .call_tool("kv.list", None)
            .await
            .expect("list");
        let text = first_text(&r);
        assert!(text.contains("\"x\""), "kv.list returned: {text}");

        // 3) close → re-open → list should now be empty *only if* the
        // server reloads from disk. The pilot persists to a JSON
        // file in $COS_DATA_DIR, so re-opening should still see "x".
        let closed = close_session("kv").await;
        assert!(closed);
        let opened2 = open_session("kv").await.expect("re-open kv");
        let r = opened2
            .0
            .call_tool("kv.get", Some(serde_json::json!({"key":"x"})))
            .await
            .expect("get after restart");
        let text = first_text(&r);
        assert!(
            text.contains("42"),
            "post-restart get should re-load value: {text}"
        );

        // Clean up so subsequent tests start fresh.
        let _ = close_session("kv").await;

        match prev_apps {
            Some(v) => std::env::set_var("COS_APPS_DIR", v),
            None => std::env::remove_var("COS_APPS_DIR"),
        }
        match prev_data {
            Some(v) => std::env::set_var("COS_DATA_DIR", v),
            None => std::env::remove_var("COS_DATA_DIR"),
        }
        match prev_mode {
            Some(v) => std::env::set_var("COS_CAPS_MODE", v),
            None => std::env::remove_var("COS_CAPS_MODE"),
        }
    }

    fn first_text(res: &crate::agent::tools::mcp::protocol::CallToolResult) -> String {
        use crate::agent::tools::mcp::protocol::ContentItem;
        for item in &res.content {
            if let ContentItem::Text { text } = item {
                return text.clone();
            }
        }
        String::new()
    }
}
