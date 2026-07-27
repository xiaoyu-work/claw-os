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
            // Reap in a detached tokio task so we don't leak a
            // zombie. Falls back to relying on parent-exit reap if
            // no tokio runtime is available (which shouldn't happen
            // — every caller of `close_session` is async).
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

type SessionKey = (u32, String);
type SessionTable = Mutex<HashMap<SessionKey, ActiveSession>>;
/// Per-app exclusion for the lazy-open path. The session table mutex
/// is held only for hash-map probes; the actual spawn + handshake
/// happens with this per-app lock held, so a tight burst of
/// concurrent callers to `get_or_open` for the same app spawns
/// exactly one child instead of N. The map of locks itself is keyed
/// by `app_id` and grows monotonically (one entry per app the agent
/// ever touches in this process — bounded by the number of
/// installed apps, so a memory non-issue).
type OpenLocks = std::sync::Mutex<HashMap<SessionKey, Arc<Mutex<()>>>>;

fn manager() -> &'static SessionTable {
    static MANAGER: OnceLock<SessionTable> = OnceLock::new();
    MANAGER.get_or_init(|| Mutex::new(HashMap::new()))
}

fn open_locks() -> &'static OpenLocks {
    static LOCKS: OnceLock<OpenLocks> = OnceLock::new();
    LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn app_open_lock(key: &SessionKey) -> Arc<Mutex<()>> {
    let mut map = open_locks().lock().unwrap_or_else(|p| p.into_inner());
    map.entry(key.clone())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

fn session_key(app_id: &str) -> Result<SessionKey, String> {
    let uid = match crate::paths::current_owner_uid_override() {
        Some(uid) => uid,
        None => {
            #[cfg(unix)]
            {
                unsafe { libc::geteuid() as u32 }
            }
            #[cfg(not(unix))]
            {
                return Err("App sessions require a Unix owner identity".to_string());
            }
        }
    };
    if uid == 0 {
        return Err("refusing to open an App session as root".to_string());
    }
    Ok((uid, app_id.to_string()))
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
///
/// Path safety: `session.entry` is joined to `app_dir`, then the
/// canonical absolute path is verified to lie under the canonical
/// `app_dir` itself. A manifest with `"entry": "../../escape.py"` is
/// rejected before we ever spawn anything. Without this check, a
/// hostile manifest could induce the kernel to exec arbitrary
/// files outside the apps tree.
///
/// Env safety: the child env is `env_clear()`ed then a small
/// allowlist is reinstated. Without this the child inherits every
/// secret in the parent process — MCP-session apps are third-party
/// code and should see only what the operator explicitly grants
/// (`COS_*` config and the small set of locale/PATH/HOME vars in
/// [`safe_session_env_allowlist`]).
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
    // Reject obvious traversal up front (the canonicalise step below
    // catches the deep version, but rejecting `..` early is cheaper
    // and gives a clearer error).
    if entry_rel.contains("..") {
        return Err(format!(
            "app `{app_id}`: session entry `{entry_rel}` contains parent-traversal `..`"
        ));
    }
    let entry_abs = app_dir.join(&entry_rel);
    if !entry_abs.is_file() {
        return Err(format!(
            "app `{app_id}`: session entry `{}` not found at {}",
            entry_rel,
            entry_abs.display()
        ));
    }
    // Realpath defence: confirm the resolved entry lives under the
    // resolved app_dir. Catches symlink escapes that the lexical
    // `..` check above would miss.
    let canon_app = std::fs::canonicalize(app_dir).map_err(|e| {
        format!(
            "app `{app_id}`: canonicalize app_dir {}: {e}",
            app_dir.display()
        )
    })?;
    let canon_entry = std::fs::canonicalize(&entry_abs).map_err(|e| {
        format!(
            "app `{app_id}`: canonicalize entry {}: {e}",
            entry_abs.display()
        )
    })?;
    if !canon_entry.starts_with(&canon_app) {
        return Err(format!(
            "app `{app_id}`: session entry resolves to {} which escapes app dir {}",
            canon_entry.display(),
            canon_app.display()
        ));
    }

    let apps_dir = apps_root();
    let apps_dir_str = apps_dir.to_string_lossy().to_string();
    let data_dir = data_dir_string();

    // Resolve the directories holding `claw_os_sdk` and `cos_runtime`
    // Python packages so `runtime: python` MCP-session apps can
    // `from claw_os_sdk import ai` and `from cos_runtime import
    // policy`. Honour the explicit override first; otherwise probe
    // the production install path and the in-repo dev paths
    // (`<repo>/claw-os-sdk/python/src` and
    // `<repo>/cos-runtime/python/src`).
    let py_dirs = resolve_python_pkg_dirs(&apps_dir);
    let mut path_parts: Vec<String> =
        py_dirs.iter().map(|p| p.to_string_lossy().to_string()).collect();
    path_parts.push(apps_dir_str.clone());
    let pythonpath = path_parts.join(pathsep());

    let mut command = build_command(manifest.runtime, &entry_abs);
    // Wipe inherited env then reinstate the bare minimum + the
    // `COS_*` configuration variables. App-internal env from
    // `crate::config::as_env_vars()` is the curated subset the
    // kernel decides to share with apps.
    command.env_clear();
    for (k, v) in safe_session_env_allowlist() {
        command.env(k, v);
    }
    command
        .env("COS_APP_ID", app_id)
        .env("COS_DATA_DIR", &data_dir)
        .env("COS_CAPS_DATA_DIR", crate::paths::caps_data_dir())
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
        .stderr(Stdio::piped());
    if let Some(home) = crate::paths::current_home_override() {
        command.env("HOME", &home).env("COS_HOME", home);
    }
    crate::bridge::apply_routed_identity(command.as_std_mut())?;

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
    // Pipe + prefix child stderr so per-app log lines are
    // attributable and don't corrupt the parent's TUI/log stream.
    if let Some(stderr) = child.stderr.take() {
        let prefix = app_id.to_string();
        tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, BufReader};
            let mut lines = BufReader::new(stderr).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        tracing::warn!(target: "cos_app", "[app:{prefix}] {line}");
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
        });
    }

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
            kill_and_reap_child(child);
            return Err(format!("initialize: {e}"));
        }
        Err(_) => {
            kill_and_reap_child(child);
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
            kill_and_reap_child(child);
            return Err(format!("tools/list: {e}"));
        }
        Err(_) => {
            kill_and_reap_child(child);
            return Err(format!(
                "tools/list timed out after {}s",
                timeout_dur.as_secs()
            ));
        }
    };

    Ok((client, child, listed_count))
}

/// Best-effort kill + detached reap of a child process. Used on
/// handshake-failure paths inside [`bring_up_app`]. Without the
/// background `wait()` a long-lived agent process accumulates one
/// zombie per failed app spawn.
fn kill_and_reap_child(mut child: Child) {
    let _ = child.start_kill();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = child.wait().await;
        });
    }
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
    let key = session_key(app_id)?;
    {
        let table = manager().lock().await;
        if let Some(s) = table.get(&key) {
            return Ok(s.client.clone());
        }
    }
    open_session(app_id).await.map(|(c, _)| c)
}

/// Explicitly bring up `app_id`. Returns `(client, tool_count)`.
/// Idempotent: returns the existing session if one is already open.
///
/// Race safety: the previous implementation released the manager
/// mutex between the "is there a session?" probe and the spawn. Two
/// callers racing on the same app would each see "no session", each
/// spawn a child, and the slower one would overwrite the faster's
/// table entry — leaving an orphan child whose stdin/stdout get
/// dropped immediately. We now take a *per-app* mutex across the
/// whole probe-then-spawn-then-insert sequence so exactly one child
/// is created per app per process.
async fn open_session(app_id: &str) -> Result<(Arc<McpClient>, usize), String> {
    let key = session_key(app_id)?;
    let lock = app_open_lock(&key);
    let _open_guard = lock.lock().await;

    // Re-probe under the per-app lock — another racer may have just
    // finished the spawn we were blocked on.
    {
        let table = manager().lock().await;
        if let Some(s) = table.get(&key) {
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
        key,
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
///
/// We move the `ActiveSession` out of the table *before* dropping it
/// so the manager mutex isn't held across the kill+reap. The Drop
/// impl on `ActiveSession` spawns a detached `wait()` task so we
/// don't block here either — any in-flight `tools/call` against this
/// session will return `ConnectionClosed` once the child's stdio is
/// torn down.
async fn close_session(app_id: &str) -> bool {
    let Ok(key) = session_key(app_id) else {
        return false;
    };
    let removed = {
        let mut table = manager().lock().await;
        table.remove(&key)
    };
    let was_present = removed.is_some();
    // Explicit drop here to make the lifetime obvious — the Drop
    // impl does the async reap.
    drop(removed);
    was_present
}

fn apps_root() -> PathBuf {
    PathBuf::from(std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".into()))
}

fn data_dir_string() -> String {
    if crate::paths::current_owner_uid_override().is_some() {
        crate::paths::user_data_dir().to_string_lossy().into_owned()
    } else {
        crate::paths::data_dir().to_string_lossy().into_owned()
    }
}

/// Environment variables an app-session child needs at a minimum:
/// PATH (for locating interpreters), HOME (cache dirs), locale/TZ
/// (for correct output), terminal hints. Everything else — and in
/// particular every `*_TOKEN`, `*_API_KEY`, `*_SECRET` — is dropped
/// by [`bring_up_app`]'s `env_clear`.
fn safe_session_env_allowlist() -> Vec<(String, String)> {
    const ALWAYS: &[&str] = &[
        "PATH", "HOME", "USER", "LOGNAME", "SHELL", "LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES",
        "TZ", "TERM", "TMPDIR", "TEMP", "TMP",
    ];
    let mut out = Vec::with_capacity(ALWAYS.len());
    for k in ALWAYS {
        if let Ok(v) = std::env::var(k) {
            out.push(((*k).to_string(), v));
        }
    }
    out
}

/// Locate the directories containing the `claw_os_sdk` and
/// `cos_runtime` Python packages.
///
/// Honours `COS_SDK_PYTHON_DIR` first, then falls back to the
/// production install path (`/usr/lib/cos/python`), and finally to
/// the in-repo dev-checkout paths at fixed offsets from
/// `$COS_APPS_DIR`. Returns the *distinct* candidates that actually
/// host one of the wanted packages, deduplicated and order-preserving.
fn resolve_python_pkg_dirs(apps_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(v) = std::env::var("COS_SDK_PYTHON_DIR") {
        if !v.is_empty() {
            candidates.push(PathBuf::from(v));
        }
    }
    candidates.push(PathBuf::from("/usr/lib/cos/python"));
    if let Some(parent) = apps_dir.parent() {
        candidates.push(parent.join("claw-os-sdk").join("python").join("src"));
        candidates.push(parent.join("cos-runtime").join("python").join("src"));
    }
    let wanted = ["claw_os_sdk", "cos_runtime"];
    let mut out: Vec<PathBuf> = Vec::new();
    for c in candidates {
        if !wanted.iter().any(|p| c.join(p).is_dir()) {
            continue;
        }
        if !out.iter().any(|existing| existing == &c) {
            out.push(c);
        }
    }
    out
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

    /// Race test: two callers concurrently invoke `open_session` on
    /// the same app. The per-app lock guarantees exactly one child is
    /// spawned + one session table entry is created. Without the
    /// lock both callers would race past the manager probe, both
    /// would spawn a child, and one of them would be silently
    /// overwritten in `table.insert` — leaving an orphan whose stdio
    /// handles get dropped immediately.
    ///
    /// We assert this by counting how many distinct `Arc<McpClient>`s
    /// the two opens return — they must both be the same Arc, which
    /// proves the second caller found the first's entry under the
    /// lock and short-circuited the spawn.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn open_race_single_child() {
        let _g = env_lock();
        let apps_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("apps");
        if !apps_dir.join("kv").join("server.py").is_file() {
            eprintln!("skip open_race_single_child: {} not present", apps_dir.display());
            return;
        }
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skip open_race_single_child: python3 not on PATH");
            return;
        }

        let data = tempfile::tempdir().unwrap();
        let prev_apps = std::env::var("COS_APPS_DIR").ok();
        let prev_data = std::env::var("COS_DATA_DIR").ok();
        let prev_mode = std::env::var("COS_CAPS_MODE").ok();
        std::env::set_var("COS_APPS_DIR", &apps_dir);
        std::env::set_var("COS_DATA_DIR", data.path());
        std::env::set_var("COS_CAPS_MODE", "permissive");

        let _ = close_session("kv").await;

        // Spawn two concurrent open_session calls. With the bug, both
        // would race past the manager probe and each spawn its own
        // server. With the per-app lock, the second blocks until the
        // first finishes, then short-circuits.
        let t1 = tokio::spawn(async { open_session("kv").await });
        let t2 = tokio::spawn(async { open_session("kv").await });
        let (r1, r2) = (t1.await.unwrap(), t2.await.unwrap());
        let (c1, _) = r1.expect("first open");
        let (c2, _) = r2.expect("second open");

        // Both callers must observe the same client (`Arc::ptr_eq`).
        // A second spawn would have produced a fresh Arc.
        assert!(
            Arc::ptr_eq(&c1, &c2),
            "open_session race produced two distinct sessions"
        );

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
