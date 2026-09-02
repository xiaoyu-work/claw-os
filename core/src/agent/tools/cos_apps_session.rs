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
//! triggers `bring_up_app` in a direct runtime, or a host control call in a
//! supervised task. The spawned server, MCP handshake, and live handle are
//! owned by the task's `claw-extension-host`, never by `claw-agentd`.
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
//! 1. [`Manifest::resolve_session_tool_args`] validates the call and
//!    materializes every declared default.
//! 2. [`Manifest::resolve_session_tool_needs`] turns the manifest's
//!    `needs[]` plus those effective arguments into concrete [`Cap`]s.
//! 3. [`crate::caps::require`] checks each. A denial short-circuits
//!    before the app server sees the call.
//! 4. On both allow and deny the kernel emits one
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::time::timeout;

use crate::agent::llm::run_log::{record as record_run, LlmRunRecord};
use crate::agent::tools::mcp::client::{ClientError, McpClient};
use crate::agent::tools::mcp::protocol::{
    ClientCapabilities, Implementation, ToolDescriptor, PROTOCOL_VERSION,
};
use crate::agent::tools::mcp::transport::StdioTransport;
use crate::caps::manifest::{Manifest, Runtime, Session, SessionTransport};

use super::exposure::{ToolExposure, ToolTransport};
use super::progressive::ToolDisclosure;
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
    /// Keeps the kernel-attested App session registered for the lifetime of
    /// the MCP child.
    identity: crate::bridge::AppIdentitySession,
    /// Serializes grant + RPC + revoke so concurrent tool calls cannot
    /// exercise each other's transient capabilities.
    call_lock: Arc<Mutex<()>>,
    child_pid: u32,
    poisoned: Arc<AtomicBool>,
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

type SessionKey = (u32, String, String);
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
    let parent = crate::proc::current_session_info_for_caps()
        .ok_or_else(|| "App session requires a registered parent session".to_string())?;
    Ok((uid, parent.session_id, app_id.to_string()))
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
    launch_tool: Option<&str>,
    timeout_dur: Duration,
    isolation: Option<&crate::extension_host::child_isolation::IsolationAuthority>,
) -> Result<
    (
        Arc<McpClient>,
        Child,
        usize,
        crate::bridge::AppIdentitySession,
    ),
    String,
> {
    let startup_deadline = Instant::now()
        .checked_add(timeout_dur)
        .ok_or_else(|| "MCP App startup deadline overflowed".to_string())?;
    let session = manifest
        .mcp_service()
        .ok_or_else(|| format!("app `{app_id}` has no MCP service"))?;
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
    let manifest_path = canon_app.join("app.json");
    if !manifest_path.is_file() {
        return Err(format!(
            "app `{app_id}` verified snapshot has no app.json"
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
    let mut path_parts: Vec<String> = py_dirs
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    path_parts.push(apps_dir_str.clone());
    let pythonpath = path_parts.join(pathsep());

    let mut command = build_command(manifest.runtime, &entry_abs, app_dir, isolation)?;
    let mut app_session =
        crate::bridge::AppIdentitySession::for_mcp(app_id, manifest, launch_tool)?;
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
        .env("COS_APP_MANIFEST", &manifest_path)
        .env("COS_SESSION", app_session.id())
        .env("COS_DATA_DIR", &data_dir)
        .env("COS_PROC_DATA_DIR", app_session.proc_data_dir())
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
    let Some(child_pid) = child.id() else {
        kill_and_reap_child(child);
        return Err(format!("spawned `{app_id}` session has no pid"));
    };
    if let Err(error) = app_session.bind_process(child_pid) {
        kill_and_reap_child(child);
        return Err(error);
    }
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            kill_and_reap_child(child);
            return Err("child stdin unavailable".to_string());
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            kill_and_reap_child(child);
            return Err("child stdout unavailable".to_string());
        }
    };
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
    let init_timeout = startup_deadline.saturating_duration_since(Instant::now());
    if init_timeout.is_zero() {
        kill_and_reap_child(child);
        return Err("MCP App startup deadline expired before initialize".to_string());
    }
    let init = match timeout(init_timeout, init_fut).await {
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

    // The kernel registers from the manifest, never from remote text.
    // MCP-first servers must reproduce that exact descriptor set;
    // legacy session servers keep their historical advisory listing.
    let list_fut = client.list_tools();
    let list_timeout = startup_deadline.saturating_duration_since(Instant::now());
    if list_timeout.is_zero() {
        kill_and_reap_child(child);
        return Err("MCP App startup deadline expired before tools/list".to_string());
    }
    let listed_tools = match timeout(list_timeout, list_fut).await {
        Ok(Ok(v)) => v.tools,
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
    if manifest.mcp.is_some() {
        if let Err(error) = validate_mcp_descriptors(session, &listed_tools) {
            kill_and_reap_child(child);
            return Err(error);
        }
    }
    let listed_count = listed_tools.len();

    Ok((client, child, listed_count, app_session))
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

fn build_command(
    runtime: Runtime,
    entry: &Path,
    app_dir: &Path,
    isolation: Option<&crate::extension_host::child_isolation::IsolationAuthority>,
) -> Result<Command, String> {
    let runner = crate::bridge::app_runner_path();
    let mut args = vec![std::ffi::OsString::from("--")];
    match runtime {
        Runtime::Python => {
            args.push(if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            });
            args.push(entry.as_os_str().to_os_string());
        }
        Runtime::Node => {
            args.push("node".into());
            args.push(entry.as_os_str().to_os_string());
        }
        Runtime::Shell => {
            if cfg!(windows) {
                args.extend([
                    std::ffi::OsString::from("cmd"),
                    std::ffi::OsString::from("/c"),
                ]);
            } else {
                args.push("bash".into());
            }
            args.push(entry.as_os_str().to_os_string());
        }
        Runtime::Binary => args.push(entry.as_os_str().to_os_string()),
    }
    let launch =
        crate::extension_host::child_isolation::prepare(&runner, args, Some(app_dir), isolation)?;
    let mut command = Command::new(launch.program);
    crate::extension_host::child_isolation::close_unallowlisted_fds(command.as_std_mut());
    command.env_clear().args(launch.args).envs(launch.env);
    Ok(command)
}

// ---------------------------------------------------------------------------
// Session lookup / open / close
// ---------------------------------------------------------------------------

/// Default per-call timeout. App session calls share the same upper
/// bound as MCP catalog calls. Capability-bearing work must finish
/// within the request; grants are cleared as soon as the response is
/// received.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const HOSTED_GATEWAY_TIMEOUT: Duration =
    Duration::from_millis(crate::extension_host::protocol::MAX_REQUEST_TIMEOUT_MS);

/// Return the active client for `app_id`, opening the session lazily
/// if no entry exists. Holds the manager mutex across spawn (which is
/// fine — sessions are infrequent and the spawn happens off-thread
/// via tokio's blocking pool inside `Command::spawn`).
async fn get_or_open(
    app_id: &str,
    tool: &str,
    startup_timeout: Duration,
    isolation: Option<&crate::extension_host::child_isolation::IsolationAuthority>,
) -> Result<Arc<McpClient>, String> {
    let key = session_key(app_id)?;
    let stale = {
        let mut table = manager().lock().await;
        if let Some(s) = table.get_mut(&key) {
            if active_session_is_live(s) {
                return Ok(s.client.clone());
            }
        }
        table.remove(&key)
    };
    drop(stale);
    open_session(app_id, Some(tool), startup_timeout, isolation)
        .await
        .map(|(client, _)| client)
}

fn active_session_is_live(session: &mut ActiveSession) -> bool {
    !session.poisoned.load(Ordering::SeqCst)
        && session
            .child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
}

struct ActiveCallGuard {
    control: crate::bridge::AppSessionControl,
    child_pid: u32,
    completed: bool,
    poisoned: Arc<AtomicBool>,
    _lock: OwnedMutexGuard<()>,
}

impl ActiveCallGuard {
    fn mark_completed(&mut self) {
        self.completed = true;
    }
}

impl Drop for ActiveCallGuard {
    fn drop(&mut self) {
        let clear = self.control.set_transient_call(None);
        if let Err(error) = &clear {
            tracing::warn!(
                child_pid = self.child_pid,
                error = %error,
                "failed to clear App MCP transient capabilities; killing session"
            );
        }
        if !self.completed || clear.is_err() {
            self.poisoned.store(true, Ordering::SeqCst);
            #[cfg(unix)]
            unsafe {
                libc::kill(self.child_pid as i32, libc::SIGKILL);
            }
        }
    }
}

async fn begin_active_session_call(
    app_id: &str,
    tool: &str,
    args: &BTreeMap<String, Value>,
    caps: &[crate::caps::Cap],
    context: &super::app_gateway::McpCallContext,
    capability_generation: Option<&str>,
    package_digest: Option<&str>,
    deadline_unix_ms: u64,
    gateway_handle: Option<&str>,
) -> Result<ActiveCallGuard, String> {
    let key = session_key(app_id)?;
    let (control, child_pid, call_lock, poisoned) = {
        let table = manager().lock().await;
        let session = table
            .get(&key)
            .ok_or_else(|| format!("App session `{app_id}` is not open"))?;
        (
            session.identity.control(),
            session.child_pid,
            session.call_lock.clone(),
            session.poisoned.clone(),
        )
    };
    let lock_timeout = deadline_unix_ms
        .checked_sub(crate::agentd::grant::now_ms())
        .filter(|remaining| *remaining > 0)
        .map(Duration::from_millis)
        .ok_or_else(|| "MCP App call deadline expired in the call queue".to_string())?;
    let lock = timeout(lock_timeout, call_lock.lock_owned())
        .await
        .map_err(|_| "MCP App call deadline expired in the call queue".to_string())?;
    let install = {
        let control = control.clone();
        let tool = tool.to_string();
        let args = args.clone();
        let call_id = context.call_id.clone();
        let session_id = context.session_id.clone();
        let task_id = context.task_id.clone();
        let capability_generation = capability_generation.map(ToOwned::to_owned);
        let package_digest = package_digest.map(ToOwned::to_owned);
        let gateway_handle = gateway_handle.map(ToOwned::to_owned);
        let caps = crate::caps::CapSet::from_caps(caps.iter().cloned());
        tokio::task::spawn_blocking(move || {
            control.set_transient_call(Some(crate::bridge::TransientCall {
                tool: &tool,
                args: &args,
                caps,
                call_id: &call_id,
                session_id: session_id.as_deref(),
                task_id: task_id.as_deref(),
                capability_generation: capability_generation.as_deref(),
                package_digest: package_digest.as_deref(),
                deadline_unix_ms: Some(deadline_unix_ms),
                gateway_handle: gateway_handle.as_deref(),
            }))
        })
        .await
        .map_err(|error| format!("App transient authority task failed: {error}"))?
    };
    if let Err(error) = install {
        let clear_error = clear_transient_call(control.clone()).await.err();
        #[cfg(unix)]
        unsafe {
            libc::kill(child_pid as i32, libc::SIGKILL);
        }
        return Err(match clear_error {
            Some(clear) => {
                format!("{error}; transient state was uncertain and cleanup failed: {clear}")
            }
            None => error,
        });
    }
    if crate::agentd::grant::now_ms() >= deadline_unix_ms {
        let clear = clear_transient_call(control.clone()).await;
        if let Err(error) = clear {
            poisoned.store(true, Ordering::SeqCst);
            #[cfg(unix)]
            unsafe {
                libc::kill(child_pid as i32, libc::SIGKILL);
            }
            return Err(format!(
                "MCP App call expired before dispatch and transient cleanup failed: {error}"
            ));
        }
        return Err("MCP App call deadline expired before dispatch".to_string());
    }
    Ok(ActiveCallGuard {
        control,
        child_pid,
        completed: false,
        poisoned,
        _lock: lock,
    })
}

async fn clear_transient_call(control: crate::bridge::AppSessionControl) -> Result<(), String> {
    tokio::task::spawn_blocking(move || control.set_transient_call(None))
        .await
        .map_err(|error| format!("App transient cleanup task failed: {error}"))?
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
async fn open_session(
    app_id: &str,
    launch_tool: Option<&str>,
    startup_timeout: Duration,
    isolation: Option<&crate::extension_host::child_isolation::IsolationAuthority>,
) -> Result<(Arc<McpClient>, usize), String> {
    if crate::paths::is_routed_job() {
        return Err(
            "App session execution must be delegated to claw-extension-host; refusing to run it in claw-agentd"
                .to_string(),
        );
    }
    let key = session_key(app_id)?;
    let lock = app_open_lock(&key);
    let _open_guard = lock.lock().await;

    // Re-probe under the per-app lock — another racer may have just
    // finished the spawn we were blocked on.
    let stale = {
        let mut table = manager().lock().await;
        if let Some(s) = table.get_mut(&key) {
            if active_session_is_live(s) {
                return Ok((s.client.clone(), s.tool_count));
            }
        }
        table.remove(&key)
    };
    drop(stale);
    let app_dir = crate::apps::find(&apps_root(), app_id)
        .map(|app| app.dir)
        .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
    let manifest_path = app_dir.join("app.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read manifest for `{app_id}`: {e}"))?;
    let manifest = Manifest::from_json(&manifest_text)
        .map_err(|e| format!("parse manifest for `{app_id}`: {e}"))?;
    let (client, child, listed, identity) = bring_up_app(
        app_id,
        &app_dir,
        &manifest,
        launch_tool,
        startup_timeout,
        isolation,
    )
    .await?;
    let child_pid = child
        .id()
        .ok_or_else(|| format!("App session `{app_id}` lost its pid"))?;
    let mut table = manager().lock().await;
    table.insert(
        key,
        ActiveSession {
            client: client.clone(),
            child: Some(child),
            tool_count: listed,
            identity,
            call_lock: Arc::new(Mutex::new(())),
            child_pid,
            poisoned: Arc::new(AtomicBool::new(false)),
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
        "COS_EXTENSION_CHILD_ISOLATION",
        crate::extension_host::protocol::BROKER_SOCKET_ENV,
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
    /// Format: `app_<id>__<tool_name_dots_to_underscores>`.
    name: String,
    /// Description built from the tool's summary.
    description: String,
    /// JSON Schema derived from the manifest MCP tool arguments.
    schema: Value,
    /// The app's manifest id.
    app_id: String,
    /// The manifest's tool name (e.g. `kv.get`) — what we send over the wire.
    manifest_tool_name: String,
    /// Exact caller-side authority for this App/tool pair.
    invoke_scope: crate::caps::Scope,
    /// Cached manifest used for cap resolution. Kept here so every call
    /// avoids re-parsing the JSON file.
    manifest: Arc<Manifest>,
    /// Per-call timeout. Defaults to [`DEFAULT_TIMEOUT`].
    timeout: Duration,
}

impl AppSessionTool {
    fn from_manifest_tool(manifest: Arc<Manifest>, tool_idx: usize) -> Result<Self, String> {
        let session = manifest
            .mcp_service()
            .ok_or_else(|| "App manifest has no MCP service".to_string())?;
        let tool = session
            .tools
            .get(tool_idx)
            .ok_or_else(|| "App manifest has no MCP tool at that index".to_string())?;
        let app_id = manifest.id.clone();
        let manifest_tool_name = tool.name.clone();
        let invoke_scope = if manifest.mcp.is_some() {
            super::app_gateway::invoke_cap(&app_id, &manifest_tool_name)
                .map_err(|error| format!("invalid MCP invocation target: {error}"))?
                .scope
        } else {
            crate::caps::Scope::name(&app_id)
        };
        let name = registry_name_for(&app_id, &manifest_tool_name);
        let description = format!(
            "App `{app_id}` session tool `{manifest_tool_name}`. {}",
            tool.summary.en_str()
        );
        let schema = build_schema(&tool.args);
        Ok(Self {
            name,
            description,
            schema,
            app_id,
            manifest_tool_name,
            invoke_scope,
            manifest,
            timeout: DEFAULT_TIMEOUT,
        })
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
    use crate::caps::manifest::{ArgKind, NeedCondition};
    let mut properties = serde_json::Map::new();
    let mut required: Vec<String> = Vec::new();
    let mut conditional = Vec::new();
    for a in args {
        let json_type = match a.kind {
            ArgKind::Path | ArgKind::Host | ArgKind::Name | ArgKind::Text => "string",
            ArgKind::Number => "number",
            ArgKind::Integer => "integer",
            ArgKind::Bool => "boolean",
        };
        let mut prop = serde_json::Map::new();
        if a.repeatable {
            prop.insert("type".to_string(), Value::String("array".to_string()));
            let mut items = serde_json::Map::from_iter([(
                "type".to_string(),
                Value::String(json_type.to_string()),
            )]);
            if !a.choices.is_empty() {
                items.insert("enum".to_string(), Value::Array(a.choices.clone()));
            }
            prop.insert("items".to_string(), Value::Object(items));
        } else {
            prop.insert("type".to_string(), Value::String(json_type.to_string()));
            if !a.choices.is_empty() {
                prop.insert("enum".to_string(), Value::Array(a.choices.clone()));
            }
        }
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
        if let Some(condition) = &a.required_when {
            let condition = match condition {
                NeedCondition::ArgPresent { arg } => json!({"required":[arg]}),
                NeedCondition::ArgEquals { arg, value } => {
                    json!({"properties":{arg:{"const":value}},"required":[arg]})
                }
                NeedCondition::ArgNotEquals { arg, value } => {
                    json!({
                        "required":[arg],
                        "not":{"properties":{arg:{"const":value}},"required":[arg]}
                    })
                }
            };
            conditional.push(json!({
                "if": condition,
                "then": {"required":[a.name]},
                "else": {"not":{"required":[a.name]}}
            }));
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
    if !conditional.is_empty() {
        schema.insert("allOf".to_string(), Value::Array(conditional));
    }
    schema.insert("additionalProperties".to_string(), Value::Bool(false));
    Value::Object(schema)
}

fn validate_mcp_descriptors(
    service: &Session,
    listed: &[ToolDescriptor],
) -> Result<(), String> {
    if listed.len() != service.tools.len() {
        return Err("MCP App descriptor set does not match its manifest".to_string());
    }
    let mut by_name = BTreeMap::new();
    for descriptor in listed {
        if by_name.insert(descriptor.name.as_str(), descriptor).is_some() {
            return Err("MCP App returned duplicate tool descriptors".to_string());
        }
    }
    for tool in &service.tools {
        let Some(descriptor) = by_name.get(tool.name.as_str()) else {
            return Err("MCP App descriptor set does not match its manifest".to_string());
        };
        if descriptor.description.as_deref() != Some(tool.summary.en_str())
            || descriptor.input_schema != build_schema(&tool.args)
        {
            return Err(format!(
                "MCP App descriptor `{}` drifted from its manifest",
                tool.name
            ));
        }
    }
    Ok(())
}

#[async_trait]
impl Tool for AppSessionTool {
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
        ToolExposure::always()
            .requiring_caps([crate::caps::Cap::new(
                crate::caps::Verb::AGENT_INVOKE,
                self.invoke_scope.clone(),
            )])
            .requiring_transport(ToolTransport::AppSession)
    }

    fn disclosure(&self) -> ToolDisclosure {
        ToolDisclosure::extension(
            "app-session",
            Some(self.app_id.clone()),
            Some(self.manifest_tool_name.clone()),
            ["app".to_string(), "session".to_string()],
        )
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let started = Instant::now();
        let supplied_args = json_to_arg_map(&input);
        let paths = match crate::bridge::launcher_path_context() {
            Ok(paths) => paths,
            Err(error) => return ToolResult::err(format!("resolve App paths: {error}")),
        };
        let effective = match self.manifest.resolve_session_tool_call(
            &self.manifest_tool_name,
            &supplied_args,
            &paths,
        ) {
            Ok(effective) => effective,
            Err(error) => {
                let message = format!("argument resolution failed: {error}");
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    "",
                    "denied",
                    Some(&message),
                    Some(&message),
                    started.elapsed(),
                );
                return ToolResult::err(message);
            }
        };

        let args_map = effective.values;
        let caps = effective.needs.into_iter().flatten().collect::<Vec<_>>();
        if let Err(denial) = crate::caps::require(
            crate::caps::Verb::AGENT_INVOKE,
            self.invoke_scope.clone(),
        ) {
            let message = denial.to_string();
            emit_audit(
                &self.app_id,
                &self.manifest_tool_name,
                crate::caps::Verb::AGENT_INVOKE.as_str(),
                "denied",
                Some(&message),
                Some(&message),
                started.elapsed(),
            );
            return ToolResult::err(message);
        }

        if self.manifest.mcp.is_some() && crate::agentd::app_gateway::available() {
            return match crate::agentd::app_gateway::call(
                self.app_id.clone(),
                self.manifest_tool_name.clone(),
                Value::Object(args_map.into_iter().collect()),
                self.timeout,
            )
            .await
            {
                Ok(result) => {
                    let (content, is_error) = render_call_result(result);
                    emit_audit(
                        &self.app_id,
                        &self.manifest_tool_name,
                        verb_csv(&caps).as_str(),
                        "allowed",
                        None,
                        is_error.then_some(content.as_str()),
                        started.elapsed(),
                    );
                    if is_error {
                        ToolResult::err(content)
                    } else {
                        ToolResult::ok(content)
                    }
                }
                Err(error) => {
                    let message = crate::agent::safety::untrusted::wrap_untrusted(
                        crate::agent::safety::untrusted::TOOL_RESULT_TAG,
                        &error,
                    );
                    emit_audit(
                        &self.app_id,
                        &self.manifest_tool_name,
                        verb_csv(&caps).as_str(),
                        "allowed",
                        None,
                        Some(&message),
                        started.elapsed(),
                    );
                    ToolResult::err(message)
                }
            };
        }

        let host = crate::extension_host::client::current();
        let (context, gateway_timeout) = match host.as_ref() {
            Some(host) => (
                super::app_gateway::McpCallContext::for_extension_system_agent(
                    host.binding(),
                    HOSTED_GATEWAY_TIMEOUT,
                ),
                HOSTED_GATEWAY_TIMEOUT,
            ),
            None => (
                super::app_gateway::McpCallContext::for_current_system_agent(self.timeout),
                self.timeout,
            ),
        };
        let context = match context {
            Ok(context) => context,
            Err(error) => {
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    crate::caps::Verb::AGENT_INVOKE.as_str(),
                    "denied",
                    Some(&error),
                    Some(&error),
                    started.elapsed(),
                );
                return ToolResult::err(error);
            }
        };
        if let Err(error) =
            super::app_gateway::authorize_manifest(&self.manifest, &context.caller)
        {
            emit_audit(
                &self.app_id,
                &self.manifest_tool_name,
                crate::caps::Verb::AGENT_INVOKE.as_str(),
                "denied",
                Some(&error),
                Some(&error),
                started.elapsed(),
            );
            return ToolResult::err(error);
        }

        if let Some(host) = host {
            return match host
                .call_app(
                    self.app_id.clone(),
                    self.manifest_tool_name.clone(),
                    Value::Object(args_map.into_iter().collect()),
                    context,
                    gateway_timeout,
                )
                .await
            {
                Ok(result) => {
                    let (content, is_error) = render_call_result(result);
                    emit_audit(
                        &self.app_id,
                        &self.manifest_tool_name,
                        verb_csv(&caps).as_str(),
                        "allowed",
                        None,
                        is_error.then_some(content.as_str()),
                        started.elapsed(),
                    );
                    if is_error {
                        ToolResult::err(content)
                    } else {
                        ToolResult::ok(content)
                    }
                }
                Err(error) => {
                    let message = crate::agent::safety::untrusted::wrap_untrusted(
                        crate::agent::safety::untrusted::TOOL_RESULT_TAG,
                        &error,
                    );
                    emit_audit(
                        &self.app_id,
                        &self.manifest_tool_name,
                        verb_csv(&caps).as_str(),
                        "allowed",
                        None,
                        Some(&message),
                        started.elapsed(),
                    );
                    ToolResult::err(message)
                }
            };
        }

        if self.manifest.mcp.is_none() {
            for cap in &caps {
                if let Err(denial) = crate::caps::require(cap.verb, cap.scope.clone()) {
                    let msg = denial.to_string();
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
        }

        // 2) Open / reuse session.
        let startup_timeout = match context.remaining(self.timeout) {
            Ok(timeout) => timeout,
            Err(error) => return ToolResult::err(error),
        };
        let client = match get_or_open(
            &self.app_id,
            &self.manifest_tool_name,
            startup_timeout,
            None,
        )
        .await
        {
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
                return ToolResult::err(format!("could not bring up app `{}`: {e}", self.app_id));
            }
        };
        let Some(deadline_unix_ms) = context.deadline_unix_ms else {
            return ToolResult::err("MCP App call context omitted its deadline");
        };
        let mut active_call = match begin_active_session_call(
            &self.app_id,
            &self.manifest_tool_name,
            &args_map,
            &caps,
            &context,
            None,
            None,
            deadline_unix_ms,
            None,
        )
        .await
        {
            Ok(guard) => guard,
            Err(error) => {
                close_session(&self.app_id).await;
                return ToolResult::err(format!(
                    "could not grant App `{}` call capabilities: {error}",
                    self.app_id
                ));
            }
        };
        let effective_timeout = match context.remaining(self.timeout) {
            Ok(timeout) => timeout,
            Err(error) => {
                active_call.mark_completed();
                drop(active_call);
                return ToolResult::err(error);
            }
        };

        // 3) Forward tools/call.
        let arguments = if args_map.is_empty() {
            None
        } else {
            Some(Value::Object(args_map.clone().into_iter().collect()))
        };
        let call = call_manifest_tool(
            &client,
            &self.manifest,
            &self.manifest_tool_name,
            arguments,
            Some(context),
        );
        let res = match timeout(effective_timeout, call).await {
            Ok(r) => r,
            Err(_) => {
                let msg = format!(
                    "app `{}` tool `{}` timed out after {}s",
                    self.app_id,
                    self.manifest_tool_name,
                    effective_timeout.as_secs()
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
                drop(active_call);
                close_session(&self.app_id).await;
                return ToolResult::err(msg);
            }
        };
        match res {
            Ok(call_result) => {
                active_call.mark_completed();
                let (content, is_error) = render_call_result(call_result);
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    verb_csv(&caps).as_str(),
                    "allowed",
                    None,
                    if is_error {
                        Some(content.as_str())
                    } else {
                        None
                    },
                    started.elapsed(),
                );
                if is_error {
                    ToolResult::err(content)
                } else {
                    ToolResult::ok(content)
                }
            }
            Err(e) => {
                if matches!(e, ClientError::Server { .. }) {
                    active_call.mark_completed();
                } else {
                    drop(active_call);
                    close_session(&self.app_id).await;
                }
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
    // MCP protocol metadata lives in the tools/call envelope. The arguments
    // object contains only manifest-declared values and is validated strictly.
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
    (
        crate::agent::safety::untrusted::wrap_untrusted(
            crate::agent::safety::untrusted::TOOL_RESULT_TAG,
            &body,
        ),
        res.is_error.unwrap_or(false),
    )
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
    let session_id = crate::proc::current_session_id();
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
    fn name(&self) -> &str {
        "cos_app_session_open"
    }

    fn description(&self) -> &str {
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

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always()
            .requiring_all_verbs([crate::caps::Verb::AGENT_INVOKE])
            .requiring_transport(ToolTransport::AppSession)
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let app_id = match input.get("app").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing `app` field".to_string()),
        };
        if let Err(error) = require_legacy_session(&app_id) {
            return ToolResult::err(error);
        }
        if let Err(denial) = crate::caps::require(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name(&app_id),
        ) {
            return ToolResult::err(denial.to_string());
        }
        let opened = match crate::extension_host::client::current() {
            Some(host) => host.open_app(app_id.clone()).await,
            None => open_session(&app_id, None, DEFAULT_TIMEOUT, None)
                .await
                .map(|(_, count)| count),
        };
        match opened {
            Ok(count) => {
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
            Err(e) => ToolResult::err(crate::agent::safety::untrusted::wrap_untrusted(
                crate::agent::safety::untrusted::TOOL_RESULT_TAG,
                &format!("open `{app_id}`: {e}"),
            )),
        }
    }
}

/// Tell the kernel to terminate an app's session server. Tool calls
/// after this still work — the next one lazily re-opens the session
/// — but any in-memory state is discarded.
pub struct CosAppSessionClose;

#[async_trait]
impl Tool for CosAppSessionClose {
    fn name(&self) -> &str {
        "cos_app_session_close"
    }

    fn description(&self) -> &str {
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

    fn exposure(&self) -> ToolExposure {
        ToolExposure::always()
            .requiring_all_verbs([crate::caps::Verb::AGENT_INVOKE])
            .requiring_transport(ToolTransport::AppSession)
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let app_id = match input.get("app").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing `app` field".to_string()),
        };
        if let Err(error) = require_legacy_session(&app_id) {
            return ToolResult::err(error);
        }
        if let Err(denial) = crate::caps::require(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name(&app_id),
        ) {
            return ToolResult::err(denial.to_string());
        }
        let closed = match crate::extension_host::client::current() {
            Some(host) => match host.close_app(app_id.clone()).await {
                Ok(closed) => closed,
                Err(error) => {
                    return ToolResult::err(crate::agent::safety::untrusted::wrap_untrusted(
                        crate::agent::safety::untrusted::TOOL_RESULT_TAG,
                        &error,
                    ))
                }
            },
            None => close_session(&app_id).await,
        };
        ToolResult::ok(json!({"app": app_id, "closed": closed}).to_string())
    }
}

/// Host-side entry point. The worker-facing tool never calls this in-process;
/// `claw-extension-host` owns the dynamic server and invokes it here.
pub(crate) async fn host_open_session(
    app_id: &str,
    isolation: &crate::extension_host::child_isolation::IsolationAuthority,
) -> Result<usize, String> {
    require_legacy_session(app_id)?;
    open_session(app_id, None, DEFAULT_TIMEOUT, Some(isolation))
        .await
        .map(|(_, count)| count)
}

pub(crate) async fn host_warm_session(
    app_id: &str,
    tool_name: &str,
    isolation: &crate::extension_host::child_isolation::IsolationAuthority,
) -> Result<usize, String> {
    open_session(app_id, Some(tool_name), DEFAULT_TIMEOUT, Some(isolation))
        .await
        .map(|(_, count)| count)
}

/// Host-side call path. Arguments are revalidated against the installed
/// manifest, then the broker-backed transient grant is installed immediately
/// around the untrusted MCP request.
pub(crate) async fn host_call_session(
    app_id: &str,
    tool_name: &str,
    input: Value,
    context: super::app_gateway::McpCallContext,
    gateway_handle: Option<String>,
    capability_generation: String,
    package_digest: Option<String>,
    isolation: &crate::extension_host::child_isolation::IsolationAuthority,
    call_timeout: Duration,
) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, String> {
    context.validate()?;
    let app_dir = crate::apps::find(&apps_root(), app_id)
        .map(|app| app.dir)
        .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
    let manifest_path = app_dir.join("app.json");
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| format!("read manifest for `{app_id}`: {error}"))?;
    if package_digest
        .as_deref()
        .is_some_and(|expected| crate::crypto::sha256_hex(manifest_text.as_bytes()) != expected)
    {
        return Err(format!(
            "App `{app_id}` manifest changed after Gateway authorization"
        ));
    }
    let manifest = Manifest::from_json(&manifest_text)
        .map_err(|error| format!("parse manifest for `{app_id}`: {error}"))?;
    super::app_gateway::authorize_manifest(&manifest, &context.caller)?;
    let supplied = json_to_arg_map(&input);
    let paths = crate::bridge::launcher_path_context()?;
    let effective = manifest
        .resolve_session_tool_call(tool_name, &supplied, &paths)
        .map_err(|error| format!("argument resolution failed: {error}"))?;
    let args = effective.values;
    let caps = effective.needs.into_iter().flatten().collect::<Vec<_>>();
    let maximum_timeout = call_timeout.min(DEFAULT_TIMEOUT);
    let startup_timeout = context.remaining(maximum_timeout)?;
    let client = get_or_open(app_id, tool_name, startup_timeout, Some(isolation)).await?;
    let deadline_unix_ms = context
        .deadline_unix_ms
        .ok_or_else(|| "MCP App call context omitted its deadline".to_string())?;
    let mut active = begin_active_session_call(
        app_id,
        tool_name,
        &args,
        &caps,
        &context,
        Some(&capability_generation),
        package_digest.as_deref(),
        deadline_unix_ms,
        gateway_handle.as_deref(),
    )
    .await?;
    let effective_timeout = match context.remaining(maximum_timeout) {
        Ok(timeout) => timeout,
        Err(error) => {
            active.mark_completed();
            drop(active);
            return Err(error);
        }
    };
    let arguments = (!args.is_empty()).then(|| Value::Object(args.into_iter().collect()));
    match timeout(
        effective_timeout,
        call_manifest_tool(&client, &manifest, tool_name, arguments, Some(context)),
    )
    .await
    {
        Ok(Ok(result)) => {
            active.mark_completed();
            Ok(result)
        }
        Ok(Err(error)) => {
            if matches!(error, ClientError::Server { .. }) {
                active.mark_completed();
            } else {
                drop(active);
                close_session(app_id).await;
            }
            Err(format!("app `{app_id}` tool `{tool_name}` failed: {error}"))
        }
        Err(_) => {
            drop(active);
            close_session(app_id).await;
            Err(format!(
                "app `{app_id}` tool `{tool_name}` timed out after {}s",
                effective_timeout.as_secs()
            ))
        }
    }
}

pub(crate) async fn host_close_session(app_id: &str) -> bool {
    close_session(app_id).await
}

pub(crate) async fn host_close_all_sessions() {
    let sessions = {
        let mut table = manager().lock().await;
        std::mem::take(&mut *table)
    };
    drop(sessions);
}

async fn call_manifest_tool(
    client: &Arc<McpClient>,
    manifest: &Manifest,
    tool_name: &str,
    arguments: Option<Value>,
    context: Option<super::app_gateway::McpCallContext>,
) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, ClientError> {
    if manifest.mcp.is_some() {
        let context = context.ok_or_else(|| {
            ClientError::Protocol("MCP-first App call omitted its trusted context".to_string())
        })?;
        client
            .call_tool_with_context(tool_name, arguments, context)
            .await
    } else {
        client.call_tool(tool_name, arguments).await
    }
}

fn manifest_tool_names(app_id: &str) -> Result<Vec<String>, String> {
    let manifest_path = crate::apps::find(&apps_root(), app_id)
        .map(|app| app.dir.join("app.json"))
        .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
    let text =
        std::fs::read_to_string(&manifest_path).map_err(|e| format!("read manifest: {e}"))?;
    let manifest = Manifest::from_json(&text).map_err(|e| format!("parse manifest: {e}"))?;
    Ok(manifest
        .mcp_service()
        .map(|service| service.tools.iter().map(|tool| tool.name.clone()).collect())
        .unwrap_or_default())
}

fn require_legacy_session(app_id: &str) -> Result<(), String> {
    let app = crate::apps::find(&apps_root(), app_id)
        .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
    if app.manifest.mcp.is_some() {
        return Err(format!(
            "App `{app_id}` uses Gateway-managed MCP lifecycle"
        ));
    }
    if app.manifest.session.is_none() {
        return Err(format!("App `{app_id}` has no legacy session"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bulk registration entry point
// ---------------------------------------------------------------------------

/// Walk `$COS_APPS_DIR` and register one [`AppSessionTool`] per
/// manifest-declared MCP tool. MCP-first services are exposed only
/// when their caller policy admits the system Agent. Legacy session
/// manifests retain the explicit open/close meta-tools during migration.
pub fn register_all(registry: &mut ToolRegistry) {
    let apps = crate::apps::discover(&apps_root());
    let mut has_legacy_session_tools = false;
    for app in apps.values() {
        let Some(session) = app.manifest.mcp_service() else {
            continue;
        };
        if app.manifest.mcp.is_some() && !session.access.system_agent {
            continue;
        }
        has_legacy_session_tools |= app.manifest.session.is_some();
        let arc_manifest = Arc::new(app.manifest.clone());
        for idx in 0..session.tools.len() {
            match AppSessionTool::from_manifest_tool(arc_manifest.clone(), idx) {
                Ok(tool) => registry.register(Arc::new(tool)),
                Err(error) => {
                    tracing::warn!(
                        app = %app.manifest.id,
                        %error,
                        "skipping invalid MCP App tool"
                    );
                }
            }
        }
    }
    if has_legacy_session_tools {
        registry.register(Arc::new(CosAppSessionOpen));
        registry.register(Arc::new(CosAppSessionClose));
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_apps_session.rs"
    ));
}
