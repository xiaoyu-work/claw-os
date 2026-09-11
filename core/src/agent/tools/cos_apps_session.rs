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
//! 3. The broker freshly authorizes each hosted call; ordinary launchers
//!    retain their local capability gate. A denial precedes `tools/call`.
//!    Grant, RPC and revocation share one per-session lock.
//! 4. On both allow and deny the kernel emits one
//!    [`LlmRunRecord`] to `ai.jsonl` with `provider="app:<id>"` and
//!    `model="tool:<tool_name>"`, matching the `cos ai tool` audit
//!    shape. App-internal calls that re-enter the kernel (e.g. the
//!    server shells `cos ai chat`) carry the app's `COS_APP_ID` and
//!    are audited under that identity too.
//!
//! The App runs in the shared sandbox with private App state, not standing
//! host-file or network grants. Mediated effects use only the current call's
//! live authority. Activity-associated results use the shared receipt ledger.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::agent::llm::run_log::{record as record_run, LlmRunRecord};
use crate::agent::tools::mcp::client::{ClientError, McpClient};
use crate::agent::tools::mcp::protocol::{ClientCapabilities, Implementation, PROTOCOL_VERSION};
use crate::agent::tools::mcp::transport::StdioTransport;
use crate::caps::manifest::{Manifest, Runtime, SessionTransport};

use super::registry::ToolRegistry;
use super::{Tool, ToolResult};

mod call;
mod sandbox;
use sandbox::SessionProcess;

// ---------------------------------------------------------------------------
// Process-wide session manager
// ---------------------------------------------------------------------------

/// One running app session. Holding `child` keeps the process alive;
/// dropping the whole entry kills it.
struct ActiveSession {
    client: Arc<McpClient>,
    /// Owns the child, broker endpoints and sandbox teardown together.
    _process: SessionProcess,
    /// For diagnostics + tool count surfaced through `open`.
    tool_count: usize,
    /// Keeps the kernel-attested App session registered for the lifetime of
    /// the MCP child.
    identity: crate::bridge::AppIdentitySession,
    /// Serializes grant + RPC + revoke so concurrent tool calls cannot
    /// exercise each other's transient capabilities.
    call_lock: Arc<Mutex<()>>,
    process_identity: crate::provenance::runtime::ProcessIdentity,
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
        self.poisoned.store(true, Ordering::SeqCst);
        self._process.terminate();
        if !crate::clawd::client::has_gateway() {
            crate::provenance::runtime::deregister(self.process_identity.uid, self.identity.id());
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
fn declared_session_entry(launch: &crate::bridge::AppLaunch) -> Result<String, String> {
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
/// This binding authenticates the bytes. The separate SessionProcess owns
/// the shared worker sandbox and its lifetime; neither replaces the other.
pub(crate) struct SessionBinding {
    binding: crate::bridge::LaunchBindingRef,
    package: crate::provenance::runtime::PackageRef,
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
    ) -> Result<Self, String> {
        let package_identity = binding.dir_identity();
        let pinned_entries = binding.entries();
        let package = binding
            .package_ref()
            .ok_or_else(|| "App session has no verified package binding".to_string())?;
        Ok(Self {
            binding,
            package,
            entry_rel,
            entry_path,
            package_identity,
            pinned_entries,
        })
    }

    /// The absolute path of the pinned session entry.
    fn entry_path(&self) -> &Path {
        &self.entry_path
    }

    /// Audit-safe projection of what this launch is pinned to.
    ///
    /// The streamed worker policy pins these same directory and entrypoint
    /// identities. Cache reuse and tool calls still recheck the binding.
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

    fn assert_live(&self, session_id: &str) -> Result<(), String> {
        self.assert_pinned()?;
        if let Some(result) =
            crate::clawd::client::check_hosted_app(session_id, &self.package.content_digest)
        {
            return result;
        }
        self.package.is_live(&crate::provenance::trust_store())?;
        crate::provenance::runtime::assert_live_instance_now(
            crate::provenance::runtime::current_owner(),
            session_id,
        )
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
        SessionProcess,
        usize,
        crate::bridge::AppIdentitySession,
        SessionBinding,
    ),
    String,
> {
    let app_id = launch.app_id().to_string();
    let app_id = app_id.as_str();
    let entry_rel = declared_session_entry(launch)?;

    // Re-assert the snapshot against the current trust store and open
    // the manifest and the session entry by descriptor. The binding
    // holds those descriptors for the life of the session, so the
    // inode that was hashed is the inode that is executed — there is
    // no `app_dir.join(entry)` re-resolution anywhere below.
    let binding = launch.bind(std::slice::from_ref(&entry_rel))?;
    let entry_path = launch.dir().join(&entry_rel);
    let bound = SessionBinding::new(binding, entry_rel.clone(), entry_path)?;

    let data_dir = data_dir_string();

    // Resolve the directories holding `claw_os_sdk` and `cos_runtime`
    // Python packages so `runtime: python` MCP-session apps can
    // `from claw_os_sdk import ai` and `from cos_runtime import
    // policy`. Honour the explicit override first; otherwise probe
    // the production install path and the in-repo dev paths
    // (`<repo>/claw-os-sdk/python/src` and
    // `<repo>/cos-runtime/python/src`).
    let py_dirs = resolve_python_pkg_dirs(apps_dir);
    let mut app_session = crate::bridge::AppIdentitySession::for_mcp(launch)?;
    let owner = crate::provenance::runtime::current_owner();
    if !crate::clawd::client::has_gateway() {
        crate::provenance::runtime::register(owner, app_session.id(), launch.package());
    }

    // The last thing before `spawn`, with the descriptors still open:
    // is every pinned file still the inode that was verified? A tree
    // swapped between `bind` and here fails the launch instead of
    // running whatever now sits at the path.
    bound.assert_pinned()?;
    crate::provenance::audit("provenance.app_session_bound", {
        let mut facts = bound.audit_facts();
        if let Some(object) = facts.as_object_mut() {
            object.insert("package_id".to_string(), json!(app_id));
            object.insert("session".to_string(), json!(app_session.id()));
        }
        facts
    });
    let mut process = sandbox::spawn(launch, &bound, &app_session, apps_dir, &data_dir, &py_dirs)?;
    let child_pid = process
        .id()
        .ok_or_else(|| format!("spawned `{app_id}` session has no pid"))?;
    if !crate::clawd::client::has_gateway() {
        crate::provenance::runtime::bind_process(owner, app_session.id(), child_pid);
    }
    app_session.bind_process(child_pid)?;
    let stdin = match process.child().stdin.take() {
        Some(stdin) => stdin,
        None => {
            return Err("child stdin unavailable".to_string());
        }
    };
    let stdout = match process.child().stdout.take() {
        Some(stdout) => stdout,
        None => {
            return Err("child stdout unavailable".to_string());
        }
    };
    // Pipe + prefix child stderr so per-app log lines are
    // attributable and don't corrupt the parent's TUI/log stream.
    if let Some(stderr) = process.child().stderr.take() {
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
            return Err(format!("initialize: {e}"));
        }
        Err(_) => {
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
            return Err(format!("tools/list: {e}"));
        }
        Err(_) => {
            return Err(format!(
                "tools/list timed out after {}s",
                timeout_dur.as_secs()
            ));
        }
    };

    Ok((client, process, listed_count, app_session, bound))
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
    if let Err(error) = session.bound.assert_live(session.identity.id()) {
        tracing::warn!(
            target: "provenance",
            %error,
            "dropping a cached App session whose signed files changed"
        );
        return false;
    }
    true
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
    let (client, process, listed, identity, bound) =
        bring_up_app(&launch, apps_root, DEFAULT_TIMEOUT).await?;
    let child_pid = process
        .id()
        .ok_or_else(|| format!("App session `{app_id}` lost its pid"))?;
    let process_identity = crate::provenance::runtime::ProcessIdentity::of_process(
        crate::provenance::runtime::current_owner(),
        child_pid,
    )
    .filter(crate::provenance::runtime::ProcessIdentity::still_matches)
    .ok_or_else(|| format!("App session `{app_id}` lost its process identity"))?;
    // The App-session MCP child is a verified package holding a live
    // stdio channel to the agent. Record which artifact it came from
    // and which exact process it is, so a later revocation can both
    // deny it and stop it.
    let mut table = manager().lock().await;
    table.insert(
        key,
        ActiveSession {
            client: client.clone(),
            _process: process,
            tool_count: listed,
            identity,
            call_lock: Arc::new(Mutex::new(())),
            process_identity,
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
    close_matching_session_at(app_id, apps_root, None).await
}

async fn close_matching_session_at(app_id: &str, apps_root: &Path, expected: Option<&str>) -> bool {
    let Ok(key) = session_key(app_id, apps_root) else {
        return false;
    };
    let removed = {
        let mut table = manager().lock().await;
        if expected.is_some_and(|id| table.get(&key).is_none_or(|s| s.identity.id() != id)) {
            return false;
        }
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

/// Locale and terminal hints only; the sandbox owns PATH, HOME and scratch.
fn safe_session_env_allowlist() -> Vec<(String, String)> {
    const ALWAYS: &[&str] = &["LANG", "LC_ALL", "LC_CTYPE", "LC_MESSAGES", "TZ", "TERM"];
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
        let supplied_args = match json_to_arg_map(&input) {
            Ok(args) => args,
            Err(error) => {
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    "",
                    "denied",
                    Some(&error),
                    Some(&error),
                    started.elapsed(),
                );
                return ToolResult::err(error);
            }
        };
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

        let hosted = crate::clawd::client::has_gateway();
        if !hosted {
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

        if let Err(error) =
            get_or_open(&self.app_id, &self.app_dir, &self.apps_root, &self.manifest).await
        {
            emit_audit(
                &self.app_id,
                &self.manifest_tool_name,
                verb_csv(&caps).as_str(),
                "allowed",
                None,
                Some(&error),
                started.elapsed(),
            );
            return ToolResult::err(format!("could not bring up app `{}`: {error}", self.app_id));
        }
        let active_call = match call::begin(
            &self.app_id,
            &self.apps_root,
            &self.manifest_tool_name,
            if hosted { &supplied_args } else { &args_map },
            &caps,
        )
        .await
        {
            Ok(guard) => guard,
            Err(error) => {
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    verb_csv(&caps).as_str(),
                    "denied",
                    Some(&error),
                    Some(&error),
                    started.elapsed(),
                );
                return ToolResult::err(format!(
                    "could not grant App `{}` call capabilities: {error}",
                    self.app_id
                ));
            }
        };

        let session_id = active_call.session_id.clone();
        let package_digest = active_call.package_digest.clone();
        let arguments = if active_call.args.is_empty() {
            None
        } else {
            Some(Value::Object(
                active_call.args.clone().into_iter().collect(),
            ))
        };
        let call = active_call
            .client
            .call_tool(self.manifest_tool_name.clone(), arguments);
        let (result, response_received) = match timeout(self.timeout, call).await {
            Ok(Ok(result)) => (Ok(render_call_result(result)), true),
            Ok(Err(error)) => {
                let received = matches!(error, ClientError::Server { .. });
                let message = format!(
                    "app `{}` tool `{}` failed: {error}",
                    self.app_id, self.manifest_tool_name
                );
                if received {
                    (Ok((message, true)), true)
                } else {
                    (
                        Err(format!(
                            "{message}; effects are indeterminate, do not repeat automatically"
                        )),
                        false,
                    )
                }
            }
            Err(_) => (
                Err(format!(
                    "app `{}` tool `{}` timed out after {}s; effects are indeterminate, do not repeat automatically",
                    self.app_id, self.manifest_tool_name, self.timeout.as_secs()
                )),
                false,
            ),
        };
        let cleanup_error = active_call.finish(response_received).err();
        if !response_received || cleanup_error.is_some() {
            close_matching_session_at(&self.app_id, &self.apps_root, Some(&session_id)).await;
        }
        let error = match &result {
            Ok((content, true)) => Some(content.as_str()),
            Err(error) => Some(error.as_str()),
            _ => None,
        };
        emit_audit(
            &self.app_id,
            &self.manifest_tool_name,
            verb_csv(&caps).as_str(),
            "allowed",
            None,
            error.or(cleanup_error.as_deref()),
            started.elapsed(),
        );
        call::report(
            &self.app_id,
            &self.manifest_tool_name,
            &package_digest,
            result,
            cleanup_error,
        )
        .await
    }
}

fn json_to_arg_map(input: &Value) -> Result<BTreeMap<String, Value>, String> {
    // MCP protocol metadata lives in the tools/call envelope. The arguments
    // object contains only manifest-declared values and is validated strictly.
    match input {
        Value::Object(m) => Ok(m.iter().map(|(k, v)| (k.clone(), v.clone())).collect()),
        _ => Err("App session tool arguments must be a JSON object".to_string()),
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
