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
use crate::agent::tools::mcp::protocol::{ClientCapabilities, Implementation, PROTOCOL_VERSION};
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
    /// Keeps the kernel-attested App session registered for the lifetime of
    /// the MCP child.
    identity: crate::bridge::AppIdentitySession,
    /// Serializes grant + RPC + revoke so concurrent tool calls cannot
    /// exercise each other's transient capabilities.
    call_lock: Arc<Mutex<()>>,
    child_pid: u32,
    poisoned: Arc<AtomicBool>,
    /// The verified snapshot this server is running, with descriptors
    /// on the manifest and the session entry still open.
    ///
    /// Held for the whole life of the session, not dropped after
    /// `spawn`: a cached session is reused many times, and every reuse
    /// re-asserts the pinned inodes against it rather than trusting
    /// that a check at open time still describes what is on disk.
    bound: Arc<SessionBinding>,
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

type SessionKey = (u32, String, String, PathBuf);
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

fn session_key(app_id: &str, apps_root: &Path) -> Result<SessionKey, String> {
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
    Ok((
        uid,
        parent.session_id,
        app_id.to_string(),
        apps_root.to_path_buf(),
    ))
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
/// Resolve the session entry an App declares, as a *signed* entrypoint.
///
/// The name comes from the verified manifest — the explicit
/// `session.entry`, or the runtime's default — and must then appear in
/// the envelope's declared entrypoints. A file that happens to sit in
/// the package and happens to be covered by the file tree is still not
/// something the publisher said may be executed, and running it would
/// let a signed package become an arbitrary-code launcher for anything
/// shipped alongside it.
fn declared_session_entry(
    launch: &crate::bridge::AppLaunch,
) -> Result<String, String> {
    let app_id = launch.app_id();
    let manifest = launch.manifest();
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
    // Traversal, absolute paths and alternate separators are refused by
    // the envelope's own path rules, but saying so here gives a clearer
    // error than "not a declared entrypoint".
    if entry_rel.contains("..") || entry_rel.starts_with('/') || entry_rel.contains('\\') {
        return Err(format!(
            "app `{app_id}`: session entry `{entry_rel}` is not a plain package-relative path"
        ));
    }
    if !launch
        .package()
        .entrypoints()
        .iter()
        .any(|declared| declared == &entry_rel)
    {
        return Err(format!(
            "app `{app_id}`: session entry `{entry_rel}` is not a declared, signed entrypoint; \
             add it to the package's signed entrypoints"
        ));
    }
    Ok(entry_rel)
}

/// Everything one App session holds open for as long as it runs.
///
/// The binding is the point. It owns descriptors on the exact inodes
/// that were digest-verified — the manifest and the session entry — and
/// it is kept for the whole life of the session rather than dropped
/// after `spawn`, so "which bytes is this server running?" has an
/// answer that survives the launch. Every later call re-asserts against
/// it instead of re-reading a mutable path.
///
/// # Scope
///
/// Provenance only. This answers *which bytes run*; it does not
/// isolate them. The App-session stdio child on this path is spawned
/// directly, outside the worker sandbox — no mount namespace, no
/// egress policy, no seccomp filter — and that predates this binding.
/// Adding the binding does not make the path sandboxed and must not be
/// read as saying it is.
pub(crate) struct SessionBinding {
    binding: crate::bridge::LaunchBindingRef,
    entry_rel: String,
    entry_path: PathBuf,
    package_identity: Option<(u64, u64)>,
    pinned_entries: Vec<(PathBuf, (u64, u64))>,
}

impl SessionBinding {
    fn new(
        binding: crate::bridge::LaunchBindingRef,
        entry_rel: String,
        entry_path: PathBuf,
    ) -> Self {
        let package_identity = binding.dir_identity();
        let pinned_entries = binding.entries();
        Self {
            binding,
            entry_rel,
            entry_path,
            package_identity,
            pinned_entries,
        }
    }

    /// The absolute path of the pinned session entry.
    fn entry_path(&self) -> &Path {
        &self.entry_path
    }

    /// Audit-safe projection of what this launch is pinned to.
    ///
    /// These are the same `(dev, ino)` identities a sandbox policy
    /// binds through `AppOperationInput::package_identity` and
    /// `pinned_entries`. This path does not build such a policy: the
    /// App-session stdio child is spawned directly rather than through
    /// `worker::prepare`. That is a pre-existing property of this code
    /// path, not a consequence of anything here, and it is not a
    /// design justification — `worker::derive` already supports
    /// `StdioPlan::Streamed`, which is what the MCP/adapter attach path
    /// uses for exactly this shape of child. Moving this launch onto it
    /// is tracked separately.
    ///
    /// Until then the identities are enforced by this binding alone:
    /// the descriptors stay open, and every spawn, cache reuse and tool
    /// call re-asserts them. That is a provenance guarantee — the bytes
    /// executing are the bytes that were signed — and nothing more. It
    /// is not isolation, and it does not substitute for the sandbox
    /// this path is missing. Recording the identities makes the pinned
    /// set reconstructable from the audit log either way.
    fn audit_facts(&self) -> serde_json::Value {
        json!({
            "entry": self.entry_rel,
            "package_identity": self
                .package_identity
                .map(|(dev, ino)| json!({ "dev": dev, "ino": ino })),
            "pinned_entries": self
                .pinned_entries
                .iter()
                .map(|(path, (dev, ino))| {
                    json!({ "path": path.display().to_string(), "dev": dev, "ino": ino })
                })
                .collect::<Vec<_>>(),
        })
    }

    /// Re-assert that every pinned file is still the verified inode.
    ///
    /// Called immediately before `spawn` and again on every reuse of a
    /// cached session, so a warm cache can never be the reason a
    /// replaced script goes unnoticed. Comparing the inode identity is
    /// what makes this a check rather than a re-read: the descriptors
    /// this binding holds name the files that were hashed, and a
    /// replacement necessarily produces a different `(dev, ino)`.
    fn assert_pinned(&self) -> Result<(), String> {
        for (path, expected) in &self.pinned_entries {
            let meta = std::fs::metadata(path).map_err(|e| {
                format!("pinned session file {} is unreadable: {e}", path.display())
            })?;
            if current_identity(&meta) != *expected {
                return Err(format!(
                    "pinned session file {} was replaced after verification",
                    path.display()
                ));
            }
        }
        if let Some(expected) = self.package_identity {
            let meta = std::fs::metadata(self.binding.dir()).map_err(|e| {
                format!(
                    "pinned package directory {} is unreadable: {e}",
                    self.binding.dir().display()
                )
            })?;
            if current_identity(&meta) != expected {
                return Err(format!(
                    "pinned package directory {} was replaced after verification",
                    self.binding.dir().display()
                ));
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn current_identity(meta: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.dev(), meta.ino())
}

#[cfg(not(unix))]
fn current_identity(_meta: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

async fn bring_up_app(
    launch: &crate::bridge::AppLaunch,
    apps_dir: &Path,
    timeout_dur: Duration,
) -> Result<
    (
        Arc<McpClient>,
        Child,
        usize,
        crate::bridge::AppIdentitySession,
        SessionBinding,
    ),
    String,
> {
    let app_id = launch.app_id().to_string();
    let app_id = app_id.as_str();
    let manifest = launch.manifest();
    let entry_rel = declared_session_entry(launch)?;

    // Re-assert the snapshot against the current trust store and open
    // the manifest and the session entry by descriptor. The binding
    // holds those descriptors for the life of the session, so the
    // inode that was hashed is the inode that is executed — there is
    // no `app_dir.join(entry)` re-resolution anywhere below.
    let binding = launch.bind(std::slice::from_ref(&entry_rel))?;
    let entry_path = launch.dir().join(&entry_rel);
    let bound = SessionBinding::new(binding, entry_rel.clone(), entry_path);

    let apps_dir_str = apps_dir.to_string_lossy().to_string();
    let data_dir = data_dir_string();

    // Resolve the directories holding `claw_os_sdk` and `cos_runtime`
    // Python packages so `runtime: python` MCP-session apps can
    // `from claw_os_sdk import ai` and `from cos_runtime import
    // policy`. Honour the explicit override first; otherwise probe
    // the production install path and the in-repo dev paths
    // (`<repo>/claw-os-sdk/python/src` and
    // `<repo>/cos-runtime/python/src`).
    let py_dirs = resolve_python_pkg_dirs(apps_dir);
    let mut path_parts: Vec<String> = py_dirs
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    path_parts.push(apps_dir_str.clone());
    let pythonpath = path_parts.join(pathsep());

    // The interpreter selection comes from the same verified manifest
    // as the entry, so the runtime that executes the signed bytes and
    // the bytes themselves cannot be decided by two different reads.
    let mut command = build_command(manifest.runtime, bound.entry_path());
    let mut app_session = crate::bridge::AppIdentitySession::for_mcp(app_id, manifest)?;
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
    if app_id == "cosmic-notifications" {
        for key in [
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
    }
    crate::bridge::apply_routed_identity(command.as_std_mut())?;

    // The last thing before `spawn`, with the descriptors still open:
    // is every pinned file still the inode that was verified? A tree
    // swapped between `bind` and here fails the launch instead of
    // running whatever now sits at the path.
    bound.assert_pinned()?;
    crate::provenance::audit(
        "provenance.app_session_bound",
        {
            let mut facts = bound.audit_facts();
            if let Some(object) = facts.as_object_mut() {
                object.insert("package_id".to_string(), json!(app_id));
                object.insert("session".to_string(), json!(app_session.id()));
            }
            facts
        },
    );
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

    Ok((client, child, listed_count, app_session, bound))
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
    let runner = crate::bridge::app_runner_path();
    match runtime {
        Runtime::Python => {
            let bin = if cfg!(windows) { "python" } else { "python3" };
            let mut c = Command::new(&runner);
            c.arg("--").arg(bin).arg(entry);
            c
        }
        Runtime::Node => {
            let mut c = Command::new(&runner);
            c.arg("--").arg("node").arg(entry);
            c
        }
        Runtime::Shell => {
            if cfg!(windows) {
                let mut c = Command::new(&runner);
                c.arg("--").arg("cmd").arg("/c").arg(entry);
                c
            } else {
                let mut c = Command::new(&runner);
                c.arg("--").arg("bash").arg(entry);
                c
            }
        }
        Runtime::Binary => {
            let mut c = Command::new(&runner);
            c.arg("--").arg(entry);
            c
        }
    }
}

// ---------------------------------------------------------------------------
// Session lookup / open / close
// ---------------------------------------------------------------------------

/// Default per-call timeout. App session calls share the same upper
/// bound as MCP catalog calls. Capability-bearing work must finish
/// within the request; grants are cleared as soon as the response is
/// received.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Return the active client for `app_id`, opening the session lazily
/// if no entry exists. Holds the manager mutex across spawn (which is
/// fine — sessions are infrequent and the spawn happens off-thread
/// via tokio's blocking pool inside `Command::spawn`).
async fn get_or_open(
    app_id: &str,
    app_dir: &Path,
    apps_root: &Path,
    manifest: &Manifest,
) -> Result<Arc<McpClient>, String> {
    let key = session_key(app_id, apps_root)?;
    let stale = {
        let mut table = manager().lock().await;
        if let Some(s) = table.get(&key) {
            if !s.poisoned.load(Ordering::SeqCst) && reusable(s) {
                return Ok(s.client.clone());
            }
        }
        table.remove(&key)
    };
    drop(stale);
    open_session_at(app_id, app_dir, apps_root, manifest)
        .await
        .map(|(c, _)| c)
}

/// May this cached session be handed out again?
///
/// A warm cache is exactly where a replaced script would otherwise go
/// unnoticed, so the answer is never "yes, it is in the table". The
/// pinned inodes are re-asserted and the package's provenance is
/// re-checked against the current trust store; either failing drops the
/// entry and forces a fresh, fully verified bring-up.
fn reusable(session: &ActiveSession) -> bool {
    if let Err(error) = session.bound.assert_pinned() {
        tracing::warn!(
            target: "provenance",
            %error,
            "dropping a cached App session whose signed files changed"
        );
        return false;
    }
    if let Err(error) = crate::provenance::runtime::assert_live_instance_now(
        crate::provenance::runtime::current_owner(),
        session.identity.id(),
    ) {
        tracing::warn!(
            target: "provenance",
            %error,
            "dropping a cached App session whose package is no longer trusted"
        );
        return false;
    }
    true
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
    apps_root: &Path,
    tool: &str,
    args: &BTreeMap<String, Value>,
    caps: &[crate::caps::Cap],
) -> Result<ActiveCallGuard, String> {
    let key = session_key(app_id, apps_root)?;
    let (control, child_pid, call_lock, poisoned, session_id, bound) = {
        let table = manager().lock().await;
        let session = table
            .get(&key)
            .ok_or_else(|| format!("App session `{app_id}` is not open"))?;
        (
            session.identity.control(),
            session.child_pid,
            session.call_lock.clone(),
            session.poisoned.clone(),
            session.identity.id().to_string(),
            Arc::clone(&session.bound),
        )
    };
    // Per call, against the pinned snapshot. A session that has been
    // open for hours is exactly the case where the tree may have moved
    // underneath it.
    if let Err(error) = bound.assert_pinned() {
        poisoned.store(true, Ordering::SeqCst);
        close_session_at(app_id, apps_root).await;
        return Err(format!(
            "App session `{app_id}` no longer runs the verified package: {error}"
        ));
    }
    // Per call, against a freshly resolved trust store. A revocation
    // that landed since the session opened ends the session here — the
    // child's process group is signalled and the entry dropped — rather
    // than merely declining this one call and leaving revoked code
    // holding an open channel to the agent.
    let owner = crate::provenance::runtime::current_owner();
    if let Err(reason) = crate::provenance::runtime::assert_live_instance_now(owner, &session_id) {
        poisoned.store(true, Ordering::SeqCst);
        close_session_at(app_id, apps_root).await;
        let doomed = session_id.clone();
        let _ = tokio::task::spawn_blocking(move || {
            crate::provenance::runtime::terminate(
                owner,
                &doomed,
                crate::provenance::runtime::SHUTDOWN_GRACE,
            )
        })
        .await;
        return Err(format!(
            "App session `{app_id}` is no longer trusted and was shut down: {reason}"
        ));
    }
    let lock = call_lock.lock_owned().await;
    if let Err(error) = control.set_transient_call(Some(crate::bridge::TransientCall {
        tool,
        args,
        caps: crate::caps::CapSet::from_caps(caps.iter().cloned()),
    })) {
        let clear_error = control.set_transient_call(None).err();
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
    Ok(ActiveCallGuard {
        control,
        child_pid,
        completed: false,
        poisoned,
        _lock: lock,
    })
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
async fn open_session_at(
    app_id: &str,
    app_dir: &Path,
    apps_root: &Path,
    manifest: &Manifest,
) -> Result<(Arc<McpClient>, usize), String> {
    let key = session_key(app_id, apps_root)?;
    let lock = app_open_lock(&key);
    let _open_guard = lock.lock().await;

    // Re-probe under the per-app lock — another racer may have just
    // finished the spawn we were blocked on. Same rule as the warm
    // path: a cached entry is only reused if its signed files are
    // still the ones that were verified.
    let stale = {
        let mut table = manager().lock().await;
        if let Some(s) = table.get(&key) {
            if !s.poisoned.load(Ordering::SeqCst) && reusable(s) {
                return Ok((s.client.clone(), s.tool_count));
            }
        }
        table.remove(&key)
    };
    drop(stale);

    // Registration happened earlier and the tool schemas the model saw
    // came from that snapshot; the server is spawned now. Re-verify
    // before bring-up so a package revoked, replaced or tampered with
    // in between cannot be what actually starts.
    //
    // One `AppLaunch` from one `VerifiedPackage` is what the rest of
    // this function uses: the manifest, the runtime selection, the
    // session block, the capability ceiling and the executed entry all
    // come out of the same parse of the same signed bytes.
    let installed = crate::apps::find_verified(apps_root, app_id)?;
    let verified = installed.require_verified()?;
    verified
        .assert_current(&crate::provenance::trust_store())
        .map_err(|e| format!("App `{app_id}` changed after verification: {e}"))?;
    if installed.dir != app_dir {
        return Err(format!(
            "App `{app_id}` now resolves to {}, not the registered {}",
            installed.dir.display(),
            app_dir.display()
        ));
    }
    let launch = crate::bridge::AppLaunch::new(std::sync::Arc::clone(verified))?;
    let _ = manifest;
    let (client, child, listed, identity, bound) =
        bring_up_app(&launch, apps_root, DEFAULT_TIMEOUT).await?;
    let child_pid = child
        .id()
        .ok_or_else(|| format!("App session `{app_id}` lost its pid"))?;
    // The App-session MCP child is a verified package holding a live
    // stdio channel to the agent. Record which artifact it came from
    // and which exact process it is, so a later revocation can both
    // deny it and stop it.
    let owner = crate::provenance::runtime::current_owner();
    crate::provenance::runtime::register_mcp_package(owner, identity.id(), verified);
    crate::provenance::runtime::bind_process(owner, identity.id(), child_pid);
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
            bound: Arc::new(bound),
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
async fn close_session_at(app_id: &str, apps_root: &Path) -> bool {
    let Ok(key) = session_key(app_id, apps_root) else {
        return false;
    };
    let removed = {
        let mut table = manager().lock().await;
        table.remove(&key)
    };
    let was_present = removed.is_some();
    if let Some(session) = removed.as_ref() {
        crate::provenance::runtime::deregister(
            crate::provenance::runtime::current_owner(),
            session.identity.id(),
        );
    }
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
    /// JSON Schema derived from `manifest.session.tools[i].args`.
    schema: Value,
    /// The app's manifest id.
    app_id: String,
    /// The manifest's tool name (e.g. `kv.get`) — what we send over the wire.
    manifest_tool_name: String,
    /// Cached manifest used for cap resolution. Kept here so every call
    /// avoids re-parsing the JSON file.
    manifest: Arc<Manifest>,
    app_dir: PathBuf,
    apps_root: PathBuf,
    /// Per-call timeout. Defaults to [`DEFAULT_TIMEOUT`].
    timeout: Duration,
}

impl AppSessionTool {
    fn from_manifest_tool(
        manifest: Arc<Manifest>,
        app_dir: PathBuf,
        apps_root: PathBuf,
        tool_idx: usize,
    ) -> Self {
        let session = manifest
            .session
            .as_ref()
            .expect("from_manifest_tool requires a session block");
        let tool = &session.tools[tool_idx];
        let app_id = manifest.id.clone();
        let manifest_tool_name = tool.name.clone();
        let name = registry_name_for(&app_id, &manifest_tool_name);
        let description = format!(
            "App `{app_id}` session tool `{manifest_tool_name}`. {}",
            tool.summary.en_str()
        );
        let schema = build_schema(&tool.args);
        Self {
            name,
            description,
            schema,
            app_id,
            manifest_tool_name,
            manifest,
            app_dir,
            apps_root,
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

        // 2) Open / reuse session.
        let client = match get_or_open(
            &self.app_id,
            &self.app_dir,
            &self.apps_root,
            &self.manifest,
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
        let mut active_call = match begin_active_session_call(
            &self.app_id,
            &self.apps_root,
            &self.manifest_tool_name,
            &args_map,
            &caps,
        )
        .await
        {
            Ok(guard) => guard,
            Err(error) => {
                close_session_at(&self.app_id, &self.apps_root).await;
                return ToolResult::err(format!(
                    "could not grant App `{}` call capabilities: {error}",
                    self.app_id
                ));
            }
        };

        // 3) Forward tools/call.
        let arguments = if args_map.is_empty() {
            None
        } else {
            Some(Value::Object(args_map.clone().into_iter().collect()))
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
                drop(active_call);
                close_session_at(&self.app_id, &self.apps_root).await;
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
                    close_session_at(&self.app_id, &self.apps_root).await;
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
pub struct CosAppSessionOpen {
    apps_root: PathBuf,
}

impl CosAppSessionOpen {
    pub fn new(apps_root: PathBuf) -> Self {
        Self { apps_root }
    }
}

impl Default for CosAppSessionOpen {
    fn default() -> Self {
        Self::new(apps_root())
    }
}

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

    async fn exec(&self, input: Value) -> ToolResult {
        let app_id = match input.get("app").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing `app` field".to_string()),
        };
        if let Err(denial) = crate::caps::require(
            crate::caps::Verb::AGENT_INVOKE,
            crate::caps::Scope::name(&app_id),
        ) {
            return ToolResult::err(denial.to_string());
        }
        // The verified lookup: a quarantined install is not something
        // the model may open a session against.
        let app = match crate::apps::find_verified(&self.apps_root, &app_id) {
            Ok(app) => app,
            Err(error) => return ToolResult::err(error),
        };
        match open_session_at(&app_id, &app.dir, &self.apps_root, &app.manifest).await {
            Ok((_client, count)) => {
                // Surface what's now callable so the model knows which
                // names to use without a follow-up discovery call.
                let tool_names = match manifest_tool_names(&self.apps_root, &app_id) {
                    Ok(names) => names,
                    Err(error) => return ToolResult::err(error),
                };
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
pub struct CosAppSessionClose {
    apps_root: PathBuf,
}

impl CosAppSessionClose {
    pub fn new(apps_root: PathBuf) -> Self {
        Self { apps_root }
    }
}

impl Default for CosAppSessionClose {
    fn default() -> Self {
        Self::new(apps_root())
    }
}

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

    async fn exec(&self, input: Value) -> ToolResult {
        let app_id = match input.get("app").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return ToolResult::err("missing `app` field".to_string()),
        };
        let closed = close_session_at(&app_id, &self.apps_root).await;
        ToolResult::ok(json!({"app": app_id, "closed": closed}).to_string())
    }
}

/// Tool names disclosed to the model for one App.
///
/// Read from the verified snapshot, never from a fresh path read: the
/// names the model is told to call have to be the names that were
/// signed.
fn manifest_tool_names(apps_root: &Path, app_id: &str) -> Result<Vec<String>, String> {
    let installed = crate::apps::find_verified(apps_root, app_id)?;
    let text = installed
        .require_verified()?
        .manifest_text()
        .map_err(|e| format!("read verified manifest: {e}"))?;
    let manifest = Manifest::from_json(&text).map_err(|e| format!("parse manifest: {e}"))?;
    Ok(manifest
        .session
        .as_ref()
        .map(|s| s.tools.iter().map(|t| t.name.clone()).collect())
        .unwrap_or_default())
}

#[cfg(test)]
async fn open_session(app_id: &str) -> Result<(Arc<McpClient>, usize), String> {
    let root = apps_root();
    let app = crate::apps::find(&root, app_id)
        .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
    open_session_at(app_id, &app.dir, &root, &app.manifest).await
}

#[cfg(test)]
async fn close_session(app_id: &str) -> bool {
    close_session_at(app_id, &apps_root()).await
}

#[derive(Clone)]
pub(crate) struct RegisteredAppSession {
    pub manifest: Arc<Manifest>,
    pub app_dir: PathBuf,
}

// ---------------------------------------------------------------------------
// Bulk registration entry point
// ---------------------------------------------------------------------------

/// Walk `$COS_APPS_DIR` and register one [`AppSessionTool`] per
/// session tool declared in any app's manifest, plus the two
/// meta-tools. The MCP servers themselves are *not* started here —
/// they come up lazily on first call (or explicitly via
/// `cos_app_session_open`).
///
/// `apps` must come from a *verified* discovery: the manifests handed
/// in here become tool schemas the model reads and calls, so a
/// quarantined install must never reach this list.
pub(crate) fn register_manifests(
    registry: &mut ToolRegistry,
    apps_root: &Path,
    apps: &[RegisteredAppSession],
) {
    for app in apps {
        let manifest = &app.manifest;
        let Some(session) = &manifest.session else {
            continue;
        };
        for idx in 0..session.tools.len() {
            registry.register(Arc::new(AppSessionTool::from_manifest_tool(
                Arc::clone(manifest),
                app.app_dir.clone(),
                apps_root.to_path_buf(),
                idx,
            )));
        }
    }
    registry.register(Arc::new(CosAppSessionOpen::new(apps_root.to_path_buf())));
    registry.register(Arc::new(CosAppSessionClose::new(apps_root.to_path_buf())));
}

/// Compatibility composition helper for callers that intentionally discover
/// the process-default App root.
pub fn register_all(registry: &mut ToolRegistry) {
    let root = apps_root();
    // Verified discovery: these manifests become model-visible tool
    // schemas, so a quarantined install is skipped rather than
    // registered with a warning label.
    let apps = crate::apps::discover_verified(&root)
        .values()
        .map(|app| RegisteredAppSession {
            manifest: Arc::new(app.manifest.clone()),
            app_dir: app.dir.clone(),
        })
        .collect::<Vec<_>>();
    register_manifests(registry, &root, &apps);
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_apps_session.rs"
    ));
}
