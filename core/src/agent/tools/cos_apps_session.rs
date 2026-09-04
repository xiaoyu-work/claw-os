//! Authenticated MCP App Mesh — typed tool calls into App services.
//!
//! This is the sole model-visible App invocation path. Each App's MCP server
//! can stay alive between calls so it can hold in-memory state and run
//! background work; CLI-style one-shot App operations are not projected to
//! the model.
//!
//! ## Discovery, registration, and lifecycle
//!
//! At registry construction the kernel walks `$COS_APPS_DIR`, reads
//! every `app.json`, and for each app that declares an `mcp` block
//! registers one [`AppSessionTool`] per [`McpTool`] in the
//! manifest. The MCP server itself is *not* started at this point —
//! the lookup is lazy. The first call to any of an app's tools
//! triggers `bring_up_app`, which spawns the server, runs the MCP
//! handshake, and stores the live `McpServerHandle` in a process-wide
//! [`SessionManager`].
//!
//! Subsequent calls reuse the same client. Lifecycle is kernel-owned:
//! the first authenticated tool call starts a service lazily, and task
//! or host teardown closes it.
//!
//! ## Isolation
//!
//! The session server is third-party code holding a live channel to the
//! agent, so it runs where every other hostile worker runs: inside the
//! [`crate::worker`] sandbox, launched through
//! [`crate::bridge::prepare_app_session_worker`] with
//! [`StdioPlan::Streamed`](crate::worker::StdioPlan::Streamed). There is
//! no direct-spawn path and no downgrade — a host that cannot enforce
//! namespaces, seccomp and a resource governor refuses to open the
//! session instead of running it unconfined.
//!
//! Three bundled vendor Apps additionally hold the owner's session bus,
//! granted by [`crate::worker::trusted_desktop`] after vendor-provenance
//! and root-ownership checks. Nothing a manifest says selects it.
//!
//! ## Per-call enforcement
//!
//! Every `tools/call` the kernel forwards to an app server is gated:
//!
//! 1. [`Manifest::resolve_mcp_tool_args`] validates the call and
//!    materializes every declared default.
//! 2. [`Manifest::resolve_mcp_tool_needs`] turns the manifest's
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
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio::time::timeout;

use crate::agent::llm::run_log::{record as record_run, LlmRunRecord};
use crate::agent::tools::exposure::{ToolExposure, ToolTransport};
use crate::agent::tools::mcp::client::{ClientError, McpClient};
use crate::agent::tools::mcp::protocol::{ClientCapabilities, Implementation, PROTOCOL_VERSION};
use crate::agent::tools::mcp::transport::StdioTransport;
use crate::agent::tools::progressive::ToolDisclosure;
use crate::caps::manifest::{Manifest, McpTransport};
use crate::worker::LaunchResources;

use super::registry::ToolRegistry;
use super::{Tool, ToolResult};

// ---------------------------------------------------------------------------
// Process-wide session manager
// ---------------------------------------------------------------------------

/// Who and what a running session was launched as.
///
/// A session slot is keyed by *who* is asking (owner uid, parent
/// session, App id, apps root). That is an identity, not a guarantee:
/// the package under that identity can be replaced, re-signed, revoked
/// or granted a different trust tier while the child keeps running, and
/// the sandbox that child sits in was derived once from whatever was
/// true then. Reuse therefore compares this — the verified content
/// digest, the trust generation and tier that admitted it, and the
/// runtime and entry that were selected — against a freshly resolved
/// snapshot, and separately compares the digest of the enforced launch
/// policy. Any difference evicts the entry, which kills the process
/// group, and a fully re-verified session is opened in its place.
#[derive(Clone, Debug, Eq, PartialEq)]
struct SessionIdentity {
    owner_uid: u32,
    app_id: String,
    content_digest: String,
    trust_generation: String,
    trust_tier: String,
    runtime: String,
    entry: String,
    /// Desktop transports the kernel-side classification granted, as a
    /// stable label. Part of the reuse key: a worker that was launched
    /// holding the session bus must never be handed to a caller whose
    /// package no longer classifies for it, and the reverse.
    transports: String,
}

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
    /// What this session was launched as. Compared against a freshly
    /// resolved snapshot on every reuse.
    launched_as: SessionIdentity,
    /// Digest of the launch policy actually enforced on this worker.
    /// Reuse re-derives the policy the same session would get now and
    /// refuses the child if the two differ.
    policy_digest: String,
    /// The broker endpoint, egress broker, cgroup and launch directory
    /// the sandbox owns. Dropping it closes the worker's only route to
    /// kernel authority and releases its runtime directory; killing
    /// through it reaches the whole cgroup and process group rather
    /// than the direct child alone.
    sandbox: Arc<LaunchResources>,
}

impl ActiveSession {
    /// Kill the worker's entire process group and cgroup, then reap the
    /// direct child.
    ///
    /// The child is its own process-group leader (`setsid` runs in the
    /// sandbox provider's `pre_exec`), so nothing it forked — including
    /// a double-forked daemon that reparented away — survives this.
    fn terminate(&mut self) {
        self.sandbox.kill_all(Some(self.child_pid));
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

impl Drop for ActiveSession {
    fn drop(&mut self) {
        self.terminate();
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
// Launch + handshake
// ---------------------------------------------------------------------------

/// Where an App's MCP entry lives.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SessionEntry {
    /// A package-relative path that is a declared, signed entrypoint.
    Packaged(String),
    /// A root-owned system program the kernel-side desktop allowlist
    /// names for this App id. Only the fixed vendor rows in
    /// [`crate::worker::trusted_desktop`] can reach this, and only
    /// after the package itself passes the vendor-provenance and
    /// root-ownership checks there.
    System(&'static str),
}

impl SessionEntry {
    fn as_str(&self) -> &str {
        match self {
            SessionEntry::Packaged(rel) => rel,
            SessionEntry::System(abs) => abs,
        }
    }

    /// Entrypoints the launch binding must open and hold. A system
    /// program is not part of the package, so only the manifest is
    /// bound; the program's own root ownership is what pins it.
    fn bound_entrypoints(&self) -> Vec<String> {
        match self {
            SessionEntry::Packaged(rel) => vec![rel.clone()],
            SessionEntry::System(_) => Vec::new(),
        }
    }
}

/// Resolve the MCP entry an App declares, as a *signed* entrypoint.
///
/// The name comes from the verified manifest — the explicit
/// `mcp.entry`, or the runtime's default — and must then appear in
/// the envelope's declared entrypoints. A file that happens to sit in
/// the package and happens to be covered by the file tree is still not
/// something the publisher said may be executed, and running it would
/// let a signed package become an arbitrary-code launcher for anything
/// shipped alongside it.
///
/// An *absolute* entry is a different claim: "the thing that implements
/// my tools is a system binary, not a file I ship". The bundled
/// native-desktop Apps are built that way, and the kernel names them
/// and their programs in source. Every other App is refused, so the
/// absolute form cannot become a way to point a manifest at an
/// arbitrary binary.
fn declared_session_entry(launch: &crate::bridge::AppLaunch) -> Result<SessionEntry, String> {
    let app_id = launch.app_id();
    let manifest = launch.manifest();
    let service = manifest
        .mcp
        .as_ref()
        .ok_or_else(|| format!("app `{app_id}` has no `mcp` block"))?;
    if !matches!(service.transport, McpTransport::Stdio) {
        return Err(format!(
            "app `{app_id}`: only `stdio` transport is supported"
        ));
    }
    let entry_rel = service
        .entry
        .clone()
        .unwrap_or_else(|| manifest.runtime.default_mcp_entry().to_string());
    if entry_rel.starts_with('/') {
        return match crate::worker::trusted_desktop::allowlisted_system_program(app_id) {
            Some(allowed) if allowed == entry_rel => Ok(SessionEntry::System(allowed)),
            _ => Err(format!(
                "app `{app_id}`: mcp entry `{entry_rel}` is an absolute path, which only \
                 the kernel's fixed vendor desktop-session table may name"
            )),
        };
    }
    // Traversal and alternate separators are refused by the envelope's
    // own path rules, but saying so here gives a clearer error than
    // "not a declared entrypoint".
    if entry_rel.contains("..") || entry_rel.contains('\\') {
        return Err(format!(
            "app `{app_id}`: mcp entry `{entry_rel}` is not a plain package-relative path"
        ));
    }
    if !launch
        .package()
        .entrypoints()
        .iter()
        .any(|declared| declared == &entry_rel)
    {
        return Err(format!(
            "app `{app_id}`: mcp entry `{entry_rel}` is not a declared, signed entrypoint; \
             add it to the package's signed entrypoints"
        ));
    }
    Ok(SessionEntry::Packaged(entry_rel))
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
/// isolate them. Isolation is a separate, and separately enforced,
/// property: the child is launched through
/// [`crate::bridge::prepare_app_session_worker`] into the hostile-worker
/// sandbox, which binds these same inodes as mounts. The two reinforce
/// each other and neither substitutes for the other — a mount that
/// resolved to the wrong inode would be refused by the provider, and a
/// binding that still matched would not make an unsandboxed process
/// safe.
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

    /// The live launch binding, for the sandbox derivation that has to
    /// mount exactly these inodes.
    fn binding_ref(&self) -> &crate::bridge::LaunchBindingRef {
        &self.binding
    }

    /// The verified package directory's inode identity.
    fn package_identity(&self) -> Option<(u64, u64)> {
        self.package_identity
    }

    /// The pinned `(path, inode)` pairs this session runs.
    fn pinned_entries(&self) -> Vec<(PathBuf, (u64, u64))> {
        self.pinned_entries.clone()
    }

    /// Audit-safe projection of what this launch is pinned to.
    ///
    /// These are the same `(dev, ino)` identities the sandbox policy
    /// binds through `AppSessionInput::package_identity` and
    /// `pinned_entries`, and on this path they are both: the provider
    /// refuses to bind a source whose inode moved, and this binding
    /// re-asserts them on every spawn, cache reuse and tool call. The
    /// two answer different questions — the mount decides what the
    /// process can reach, the binding decides whether the bytes still
    /// match what was signed — so recording the pinned set keeps the
    /// provenance claim reconstructable from the audit log independently
    /// of the isolation claim.
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

/// One brought-up App session server and everything the caller must
/// keep alive alongside it.
struct BroughtUp {
    client: Arc<McpClient>,
    child: Child,
    child_pid: u32,
    tool_count: usize,
    identity: crate::bridge::AppIdentitySession,
    bound: SessionBinding,
    sandbox: LaunchResources,
    policy_digest: String,
}

#[derive(Debug)]
pub(crate) struct HostedAppError {
    category: crate::extension_host::protocol::ExtensionErrorCategory,
    message: String,
}

impl HostedAppError {
    fn new(
        category: crate::extension_host::protocol::ExtensionErrorCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    fn remote(message: impl Into<String>) -> Self {
        Self::new(
            crate::extension_host::protocol::ExtensionErrorCategory::RemoteCallFailure,
            message,
        )
    }

    fn connect(message: impl Into<String>) -> Self {
        Self::new(
            crate::extension_host::protocol::ExtensionErrorCategory::Connect,
            message,
        )
    }

    fn timeout(message: impl Into<String>) -> Self {
        Self::new(
            crate::extension_host::protocol::ExtensionErrorCategory::Timeout,
            message,
        )
    }

    fn crash(message: impl Into<String>) -> Self {
        Self::new(
            crate::extension_host::protocol::ExtensionErrorCategory::Crash,
            message,
        )
    }

    fn protocol(message: impl Into<String>) -> Self {
        Self::new(
            crate::extension_host::protocol::ExtensionErrorCategory::Protocol,
            message,
        )
    }

    pub(crate) fn category(&self) -> crate::extension_host::protocol::ExtensionErrorCategory {
        self.category
    }
}

impl std::fmt::Display for HostedAppError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HostedAppError {}

impl From<String> for HostedAppError {
    fn from(message: String) -> Self {
        Self::remote(message)
    }
}

fn client_error_category(
    error: &ClientError,
) -> crate::extension_host::protocol::ExtensionErrorCategory {
    use crate::extension_host::protocol::ExtensionErrorCategory;

    match error {
        ClientError::Server { .. } => ExtensionErrorCategory::RemoteCallFailure,
        ClientError::Timeout(_) => ExtensionErrorCategory::Timeout,
        ClientError::Transport(_) | ClientError::ConnectionClosed => ExtensionErrorCategory::Crash,
        ClientError::Encode(_) | ClientError::Decode(_) | ClientError::Protocol(_) => {
            ExtensionErrorCategory::Protocol
        }
    }
}

fn hosted_client_error(prefix: &str, error: ClientError) -> HostedAppError {
    HostedAppError::new(client_error_category(&error), format!("{prefix}: {error}"))
}

/// Everything the sandbox derivation for one App session reads, resolved
/// once from the verified snapshot.
///
/// Both the launch path and the reuse check build one of these. That is
/// the point: "may this cached child be handed out again?" is answered
/// by deriving the policy a launch started *now* would enforce and
/// comparing digests, and a second, drifting copy of the derivation
/// inputs would make that comparison meaningless.
struct SessionLaunchPlan {
    identity: SessionIdentity,
    app_dir: PathBuf,
    entry: SessionEntry,
    entry_path: PathBuf,
    program: PathBuf,
    argv: Vec<String>,
    data_dir: String,
    apps_dir: String,
    extra_env: BTreeMap<String, String>,
    /// Desktop transports the kernel-side classification granted. Empty
    /// for everything a manifest can describe.
    transports: Vec<crate::worker::trusted_desktop::Transport>,
}

impl SessionLaunchPlan {
    /// Digest of the policy `session` would be confined by if it were
    /// launched from this plan as a reusable server.
    ///
    /// The pinned inodes come from the running session's own binding
    /// rather than from a fresh `bind`: `reusable` has already asserted
    /// they still match what is on disk, and reopening descriptors for a
    /// cache probe would re-verify a package the caller just verified.
    fn policy_digest_for(
        &self,
        session: &crate::bridge::AppIdentitySession,
        bound: &SessionBinding,
    ) -> Result<String, String> {
        let policy = crate::worker::derive::app_session(crate::worker::derive::AppSessionInput {
            app_id: &self.identity.app_id,
            app_dir: &self.app_dir,
            program: self.program.clone(),
            argv: self.argv.clone(),
            caps: session.granted_caps(),
            authorized_mounts: &[],
            lifetime: crate::worker::derive::SessionLifetime::Reusable,
            session_id: session.id(),
            data_dir: &self.data_dir,
            apps_dir: &self.apps_dir,
            extra_env: self.extra_env.clone(),
            package_identity: bound.package_identity(),
            pinned_entries: bound.pinned_entries(),
            transports: &self.transports,
        })?;
        Ok(policy.digest())
    }
}

/// Resolve the launch inputs for one App session from a verified
/// snapshot. Reads nothing the caller supplied beyond the apps root.
fn plan_session_launch(
    owner_uid: u32,
    launch: &crate::bridge::AppLaunch,
    apps_dir: &Path,
) -> Result<SessionLaunchPlan, String> {
    let app_id = launch.app_id().to_string();
    let manifest = launch.manifest();
    let entry = declared_session_entry(launch)?;

    // Unsigned developer content may not hold a live stdio channel to
    // the agent. `clawd` refuses the session registration for the same
    // reason; refusing here too means a launcher that somehow reached a
    // permissive authority still stops.
    let ceiling = launch.ceiling();
    if !ceiling.allows_mcp_attach() {
        return Err(format!(
            "App `{app_id}` is {}-trusted and may not run a session server; \
             sign and install it to attach a session",
            ceiling.label()
        ));
    }

    let apps_dir_str = apps_dir.to_string_lossy().to_string();
    let data_dir = data_dir_string();
    let entry_path = match &entry {
        SessionEntry::Packaged(rel) => launch.dir().join(rel),
        SessionEntry::System(abs) => PathBuf::from(abs),
    };

    // Resolve the directories holding `claw_os_sdk` and `cos_runtime`
    // Python packages so `runtime: python` MCP-session apps can
    // `from claw_os_sdk import ai` and `from cos_runtime import
    // policy`. Honour the explicit override first; otherwise probe
    // the production install path and the in-repo dev paths
    // (`<repo>/claw-os-sdk/python/src` and
    // `<repo>/cos-runtime/python/src`). The apps root goes on the end
    // so a bundled App can `import _shared`; the sandbox mounts that
    // helper tree and nothing else from the root.
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
    let (program, argv) = crate::bridge::session_program(manifest.runtime, &entry_path)?;
    let package = launch.package();
    // The manifest the sandbox will see. It is the verified package's
    // own manifest file, at the absolute path the package mount
    // reproduces inside the worker.
    let manifest_path = launch.dir().join(package.manifest_path());

    // The one place a session may be granted a desktop transport, and
    // the only input it reads that is not the verified snapshot is the
    // kernel's own source table. A failure here is not an error for an
    // App that stays inside its package: it runs as an ordinary hostile
    // stdio server with no transport.
    let grant = crate::worker::trusted_desktop::classify(&app_id, package, &program);
    // A manifest that points outside its package is only meaningful
    // together with that classification. Losing one and keeping the
    // other would run a system binary with no transport and no
    // provenance over its bytes, so the launch stops instead.
    if matches!(entry, SessionEntry::System(_)) && grant.is_none() {
        return Err(format!(
            "App `{app_id}` names a system program as its `mcp` entry but no longer \
             classifies as a vendor desktop session; reinstall it from the system package"
        ));
    }
    let transports = grant
        .as_ref()
        .map(|grant| grant.transports().to_vec())
        .unwrap_or_default();
    // Which inode the transport resolves to right now, not merely which
    // transports the classification allows: a session whose bus socket
    // was replaced holds a descriptor on the old one and must not be
    // reused.
    let transport_label = crate::worker::trusted_desktop::transport_fingerprint(&transports);

    Ok(SessionLaunchPlan {
        identity: SessionIdentity {
            owner_uid,
            app_id,
            content_digest: package.content_digest().to_string(),
            trust_generation: package.trust_generation().to_string(),
            trust_tier: ceiling.label().to_string(),
            runtime: manifest.runtime.as_str().to_string(),
            entry: entry.as_str().to_string(),
            transports: transport_label,
        },
        app_dir: launch.dir().to_path_buf(),
        entry,
        entry_path,
        program,
        argv,
        data_dir,
        apps_dir: apps_dir_str,
        extra_env: BTreeMap::from([
            ("PYTHONPATH".to_string(), pythonpath),
            // The verified manifest, at the same absolute path the
            // read-only package mount exposes inside the sandbox. The
            // App reads its own operation/tool contract from the bytes
            // the kernel verified rather than guessing a location, and
            // the file is bound by inode so it cannot be swapped after
            // verification.
            (
                "COS_APP_MANIFEST".to_string(),
                manifest_path.to_string_lossy().into_owned(),
            ),
            // Trigger the MCP-server mode of `runtime: binary` apps.
            // The public Rust SDK's `claw_os_sdk::mcp` module keys off this
            // variable (and only this variable) so the same desktop GUI
            // binary can serve both its normal `main()` flow and the
            // agent's tool surface. This is host activation for a
            // dual-mode native binary, not a public compatibility API.
            // Python/Node/Shell apps ignore it.
            ("COS_MCP_SERVER".to_string(), "1".to_string()),
        ]),
        transports,
    })
}

/// Launch an App's session server into the hostile-worker sandbox and
/// run the JSON-RPC handshake.
///
/// Mirrors [`super::mcp::integration::attach_server`] — the same
/// derive → `worker::prepare` → spawn → `initialize` sequence, with the
/// same `StdioPlan::Streamed` transport — but skips the tool-
/// registration loop: tools are registered eagerly from the verified
/// manifest at boot, never from the server's `tools/list` response. The
/// `tools/list` we still issue is purely advisory; it confirms the
/// server speaks MCP and surfaces startup errors immediately, and
/// nothing it returns becomes callable or becomes authority.
///
/// There is no unsandboxed path out of here. A host missing bubblewrap,
/// unprivileged user namespaces, seccomp or a resource governor makes
/// `worker::prepare` refuse, and the session fails to open rather than
/// running the App's code unconfined.
async fn bring_up_app(
    launch: &crate::bridge::AppLaunch,
    plan: &SessionLaunchPlan,
    tool: &str,
    timeout_dur: Duration,
) -> Result<BroughtUp, HostedAppError> {
    let app_id = plan.identity.app_id.clone();
    let app_id = app_id.as_str();

    // Re-assert the snapshot against the current trust store and open
    // the manifest and the session entry by descriptor. The binding
    // holds those descriptors for the life of the session, so the
    // inode that was hashed is the inode that is executed — there is
    // no `app_dir.join(entry)` re-resolution anywhere below.
    let binding = launch.bind(&plan.entry.bound_entrypoints())?;
    let bound = SessionBinding::new(
        binding,
        plan.identity.entry.clone(),
        plan.entry_path.clone(),
    );

    // The session grant, minted by the authority. Everything the
    // sandbox is shaped from below reads from this object rather than
    // from anything the caller passed in.
    let mut app_session = crate::bridge::AppIdentitySession::for_mcp(launch, tool)?;

    // The last thing before the launch is built, with the descriptors
    // still open: is every pinned file still the inode that was
    // verified? A tree swapped between `bind` and here fails the launch
    // instead of running whatever now sits at the path.
    bound.assert_pinned()?;
    crate::provenance::audit("provenance.app_session_bound", {
        let mut facts = bound.audit_facts();
        if let Some(object) = facts.as_object_mut() {
            object.insert("package_id".to_string(), json!(app_id));
            object.insert("session".to_string(), json!(app_session.id()));
        }
        facts
    });

    let prepared = crate::bridge::prepare_app_session_worker(
        &app_session,
        app_id,
        &plan.app_dir,
        plan.program.clone(),
        plan.argv.clone(),
        &plan.data_dir,
        &plan.apps_dir,
        plan.extra_env.clone(),
        bound.binding_ref(),
        crate::worker::derive::SessionLifetime::Reusable,
        &plan.transports,
        None,
        &[],
    )?;
    let policy_digest = prepared.facts["policy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let crate::worker::PreparedLaunch {
        command, resources, ..
    } = prepared;
    // The provider owns argv, environment and namespaces; a consumer
    // may only choose the stdio wiring. Here that is three pipes: the
    // JSON-RPC transport in both directions plus a bounded stderr drain.
    let mut command = Command::from(command);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| HostedAppError::connect(format!("spawn `{app_id}` session: {e}")))?;
    let Some(child_pid) = child.id() else {
        kill_and_reap_child(child, &resources, None);
        return Err(HostedAppError::crash(format!(
            "spawned `{app_id}` session has no pid"
        )));
    };
    if let Err(error) = app_session.bind_process(child_pid) {
        kill_and_reap_child(child, &resources, Some(child_pid));
        return Err(HostedAppError::protocol(error));
    }
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err(HostedAppError::crash("child stdin unavailable"));
        }
    };
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err(HostedAppError::crash("child stdout unavailable"));
        }
    };
    // Pipe + prefix child stderr so per-app log lines are
    // attributable and don't corrupt the parent's TUI/log stream.
    // Bounded per line and non-authoritative: nothing read here is
    // parsed, and it never reaches the model or the audit record.
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
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err(hosted_client_error("initialize", e));
        }
        Err(_) => {
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err(HostedAppError::timeout(format!(
                "initialize timed out after {}s",
                timeout_dur.as_secs()
            )));
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
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err(hosted_client_error("tools/list", e));
        }
        Err(_) => {
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err(HostedAppError::timeout(format!(
                "tools/list timed out after {}s",
                timeout_dur.as_secs()
            )));
        }
    };

    Ok(BroughtUp {
        client,
        child,
        child_pid,
        tool_count: listed_count,
        identity: app_session,
        bound,
        sandbox: resources,
        policy_digest,
    })
}

/// Kill the whole sandbox and reap the direct child. Used on every
/// handshake-failure path inside [`bring_up_app`].
///
/// `resources.kill_all` reaches the cgroup and the child's process
/// group, so a server that already forked before failing its handshake
/// leaves nothing behind. Without the background `wait()` a long-lived
/// agent process would accumulate one zombie per failed spawn.
fn kill_and_reap_child(mut child: Child, resources: &LaunchResources, pid: Option<u32>) {
    resources.kill_all(pid);
    let _ = child.start_kill();
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async move {
            let _ = child.wait().await;
        });
    }
}

// ---------------------------------------------------------------------------
// Where one call's authority can safely be exercised
// ---------------------------------------------------------------------------

/// The decision the launcher takes before it sends a request anywhere.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CallPlacement {
    /// Every capability this call was granted is answerable through the
    /// broker endpoint. The reusable session may serve it: nothing about
    /// the sandbox has to change, so nothing about it is left changed.
    Reusable,
    /// The call needs filesystem or network reach the reusable worker's
    /// fixed policy does not have and cannot safely acquire. It gets its
    /// own worker, derived from exactly this set and destroyed with the
    /// response.
    Ephemeral,
    /// The call was granted a capability that cannot be expressed as
    /// either. Refused at authorization, with the reason, instead of
    /// letting the App discover it as `EPERM` halfway through a
    /// half-completed operation.
    Unsupported(String),
}

/// Classify the capabilities bound to one `tools/call`.
///
/// The question is not "is this allowed" — the capability gate already
/// answered that — but "can this authority be *used* without changing a
/// sandbox that other calls share". Three answers:
///
/// * a capability the broker answers (`data.kv.*`, `memory.*`,
///   `ui.notify`, an admitted `system.*` route) needs no policy at all;
/// * a filesystem or network capability naming one exact resource
///   becomes a mount or an egress rule, which a live worker cannot
///   grow, so it gets a worker of its own;
/// * a filesystem or network capability that names *no* resolvable
///   resource — a bare wildcard, a glob matching nothing — cannot
///   become either. Granting it would look like success and behave like
///   a permission error, so it is refused here instead.
pub(crate) fn classify_call(caps: &[crate::caps::Cap]) -> CallPlacement {
    use crate::caps::{Scope, Verb};

    let mut placement = CallPlacement::Reusable;
    for cap in caps {
        let filesystem = matches!(
            cap.verb,
            Verb::FS_READ
                | Verb::FS_WRITE
                | Verb::FS_DELETE
                | Verb::FS_META
                | Verb::FS_WATCH
                | Verb::FS_EXEC
        );
        let network = matches!(cap.verb, Verb::NET_DIAL | Verb::NET_LISTEN);
        if !filesystem && !network {
            continue;
        }
        match &cap.scope {
            // A name-scoped metadata lookup is a brokered executable
            // resolution request (for example `exec.which`), not a
            // filesystem path the App worker can mount.
            Scope::Name(_) if cap.verb == Verb::FS_META => {}
            Scope::Path(pattern) if filesystem => {
                if pattern
                    .trim_end_matches('*')
                    .trim_end_matches('/')
                    .is_empty()
                {
                    return CallPlacement::Unsupported(format!(
                        "`{}` was granted over every path, which a session tool cannot be \
                         given: name the exact file or directory in the manifest's scope \
                         binding so the sandbox can mount it",
                        cap.verb.as_str()
                    ));
                }
                placement = CallPlacement::Ephemeral;
            }
            Scope::Host(_) if network => placement = CallPlacement::Ephemeral,
            scope => {
                let kind = if filesystem { "filesystem" } else { "network" };
                return CallPlacement::Unsupported(format!(
                    "`{}` was granted with a {kind} scope this session cannot act on \
                     (`{}`): a session tool must bind it to one exact resource so the \
                     sandbox can grant it for the call and take it back afterwards",
                    cap.verb.as_str(),
                    scope_label(scope)
                ));
            }
        }
    }
    placement
}

fn scope_label(scope: &crate::caps::Scope) -> String {
    use crate::caps::Scope;
    match scope {
        Scope::Wild => "*".to_string(),
        Scope::Path(value) => format!("path:{value}"),
        Scope::Host(value) => format!("host:{value}"),
        Scope::Name(value) => format!("name:{value}"),
        other => format!("{other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Ephemeral single-call workers
// ---------------------------------------------------------------------------

/// One worker that exists for exactly one `tools/call`.
///
/// Everything about it — its session identity, its broker endpoint, its
/// mounts, its egress and its process group — is created for one
/// request and destroyed with the response. That is what makes it safe
/// to derive its filesystem and network reach from the call's own
/// capability set: there is no later call to inherit it, and no other
/// caller to observe it.
struct SingleCallWorker {
    client: Option<Arc<McpClient>>,
    gate_stdin: Option<ChildStdin>,
    gate_stdout: Option<ChildStdout>,
    gate_token: String,
    child: Option<Child>,
    child_pid: u32,
    identity: crate::bridge::AppIdentitySession,
    sandbox: LaunchResources,
    /// Held for the life of the worker so the pinned inodes cannot move
    /// underneath it.
    _bound: SessionBinding,
}

impl SingleCallWorker {
    /// Launch and bind a worker for `caps`.
    async fn start(
        launch: &crate::bridge::AppLaunch,
        plan: &SessionLaunchPlan,
        tool: &str,
        caps: &crate::caps::CapSet,
        authorized_mounts: &[crate::worker::AuthorizedMount],
    ) -> Result<Self, String> {
        let app_id = plan.identity.app_id.as_str();
        let binding = launch.bind(&plan.entry.bound_entrypoints())?;
        let bound = SessionBinding::new(
            binding,
            plan.identity.entry.clone(),
            plan.entry_path.clone(),
        );
        let mut identity = crate::bridge::AppIdentitySession::for_mcp(launch, tool)?;
        bound.assert_pinned()?;

        let gate_token = uuid::Uuid::new_v4().simple().to_string();
        let app_program = plan
            .program
            .to_str()
            .ok_or_else(|| format!("App `{app_id}` session program is not UTF-8"))?
            .to_string();
        let mut gated_argv = vec![
            "--launch-gate".to_string(),
            gate_token.clone(),
            "--".to_string(),
            app_program,
        ];
        gated_argv.extend(plan.argv.iter().cloned());
        let prepared = crate::bridge::prepare_app_session_worker(
            &identity,
            app_id,
            &plan.app_dir,
            crate::bridge::app_runner_path(),
            gated_argv,
            &plan.data_dir,
            &plan.apps_dir,
            plan.extra_env.clone(),
            bound.binding_ref(),
            crate::worker::derive::SessionLifetime::SingleCall,
            &plan.transports,
            Some(caps),
            authorized_mounts,
        )?;
        let crate::worker::PreparedLaunch {
            command, resources, ..
        } = prepared;
        let mut command = Command::from(command);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| format!("spawn single-call worker for `{app_id}`: {e}"))?;
        let Some(child_pid) = child.id() else {
            kill_and_reap_child(child, &resources, None);
            return Err(format!("single-call worker for `{app_id}` has no pid"));
        };
        if let Err(error) = identity.bind_process(child_pid) {
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err(error);
        }
        let owner = crate::provenance::runtime::current_owner();
        if identity.uses_local_backend() {
            crate::provenance::runtime::register(owner, identity.id(), launch.package());
            crate::provenance::runtime::bind_process(owner, identity.id(), child_pid);
        }

        let (Some(gate_stdin), Some(gate_stdout)) = (child.stdin.take(), child.stdout.take())
        else {
            if identity.uses_local_backend() {
                crate::provenance::runtime::deregister(owner, identity.id());
            }
            kill_and_reap_child(child, &resources, Some(child_pid));
            return Err("single-call worker stdio unavailable".to_string());
        };
        if let Some(stderr) = child.stderr.take() {
            let prefix = app_id.to_string();
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt, BufReader};
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::warn!(target: "cos_app", "[app:{prefix}] {line}");
                }
            });
        }
        Ok(Self {
            client: None,
            gate_stdin: Some(gate_stdin),
            gate_stdout: Some(gate_stdout),
            gate_token,
            child: Some(child),
            child_pid,
            identity,
            sandbox: resources,
            _bound: bound,
        })
    }

    /// Install this call's daemon authorization, then release the trusted
    /// runner to exec the untrusted App. Package code cannot run before this
    /// method succeeds.
    async fn authorize(
        &mut self,
        caps: &[crate::caps::Cap],
        authorization: &str,
        action_digest: &str,
    ) -> Result<SingleCallGrant, HostedAppError> {
        let guard =
            SingleCallGrant::install(self.identity.control(), caps, authorization, action_digest)
                .map_err(HostedAppError::remote)?;
        if let Err(error) = self.release_launch_gate().await {
            drop(guard);
            self.destroy();
            return Err(HostedAppError::crash(error));
        }
        Ok(guard)
    }

    async fn release_launch_gate(&mut self) -> Result<(), String> {
        let mut stdin = self
            .gate_stdin
            .take()
            .ok_or_else(|| "single-call App launch gate is unavailable".to_string())?;
        let stdout = self
            .gate_stdout
            .take()
            .ok_or_else(|| "single-call App output is unavailable".to_string())?;
        stdin
            .write_all(self.gate_token.as_bytes())
            .await
            .map_err(|error| format!("release single-call App launch gate: {error}"))?;
        stdin
            .flush()
            .await
            .map_err(|error| format!("flush single-call App launch gate: {error}"))?;
        let transport = StdioTransport::from_pair(Box::new(stdout), Box::new(stdin));
        let client: Arc<McpClient> = McpClient::new(transport);
        client.start().await;
        self.client = Some(client);
        Ok(())
    }

    async fn initialize(&mut self, timeout_dur: Duration) -> Result<(), HostedAppError> {
        let client =
            self.client.as_ref().cloned().ok_or_else(|| {
                HostedAppError::protocol("single-call App launch was not authorized")
            })?;
        let init = client.initialize(
            Implementation {
                name: "cos-agent".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            ClientCapabilities::default(),
        );
        match timeout(timeout_dur, init).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                self.destroy();
                return Err(hosted_client_error("initialize", e));
            }
            Err(_) => {
                self.destroy();
                return Err(HostedAppError::timeout(format!(
                    "initialize timed out after {}s",
                    timeout_dur.as_secs()
                )));
            }
        }
        let _ = client.notify("notifications/initialized", None).await;
        Ok(())
    }

    fn client(&self) -> Result<Arc<McpClient>, HostedAppError> {
        self.client
            .as_ref()
            .cloned()
            .ok_or_else(|| HostedAppError::protocol("single-call App launch was not authorized"))
    }

    /// Kill the whole cgroup and process group, reap, and drop the
    /// worker's kernel session. Safe to call on every outcome.
    fn destroy(&mut self) {
        self.client.take();
        self.gate_stdin.take();
        self.gate_stdout.take();
        if self.identity.uses_local_backend() {
            crate::provenance::runtime::deregister(
                crate::provenance::runtime::current_owner(),
                self.identity.id(),
            );
        }
        self.sandbox.kill_all(Some(self.child_pid));
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

impl Drop for SingleCallWorker {
    fn drop(&mut self) {
        self.destroy();
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
/// if no entry exists.
///
/// Every call goes through [`open_session_at`], which re-resolves the
/// verified package under the per-app lock before it decides whether a
/// cached child may be handed out. A cheaper probe here would be a
/// probe against data the reuse decision is not allowed to trust.
async fn get_or_open(
    app_id: &str,
    app_dir: &Path,
    apps_root: &Path,
    tool: &str,
) -> Result<Arc<McpClient>, HostedAppError> {
    open_session_at(app_id, app_dir, apps_root, tool)
        .await
        .map(|(c, _)| c)
}

/// May this cached session be handed out again?
///
/// A warm cache is exactly where a replaced script, a revoked publisher
/// or a re-derived sandbox would otherwise go unnoticed, so the answer
/// is never "yes, it is in the table". Four independent checks have to
/// agree:
///
/// * the pinned inodes still are the files that were verified;
/// * the runtime record for this instance is still live against a
///   freshly resolved trust store;
/// * the package identity the session launched as — content digest,
///   trust generation, trust tier, runtime, entry, owner — still equals
///   the freshly resolved one;
/// * the launch policy this worker is actually confined by is still the
///   policy the same session would be given now.
///
/// The last two are what make a *changed* package as fatal as a revoked
/// one, and a changed sandbox as fatal as a changed package. The policy
/// is re-derived against the cached session's own identity and grant, so
/// the comparison isolates real drift — a moved package directory, a
/// different entry inode, a changed data root or SDK path, a wider
/// standing grant — from the per-launch session id that necessarily
/// differs between two launches.
fn reusable(session: &ActiveSession, plan: &SessionLaunchPlan) -> bool {
    if let Err(error) = session.bound.assert_pinned() {
        tracing::warn!(
            target: "provenance",
            %error,
            "dropping a cached App session whose signed files changed"
        );
        return false;
    }
    if session.identity.uses_local_backend() {
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
    }
    if session.launched_as != plan.identity {
        tracing::warn!(
            target: "provenance",
            app = %plan.identity.app_id,
            "dropping a cached App session whose package or trust changed"
        );
        return false;
    }
    match plan.policy_digest_for(&session.identity, &session.bound) {
        Ok(digest) if digest == session.policy_digest => true,
        Ok(_) => {
            tracing::warn!(
                target: "provenance",
                app = %plan.identity.app_id,
                "dropping a cached App session whose sandbox policy changed"
            );
            false
        }
        Err(error) => {
            tracing::warn!(
                target: "provenance",
                app = %plan.identity.app_id,
                %error,
                "dropping a cached App session whose sandbox policy no longer derives"
            );
            false
        }
    }
}

/// RAII holder for one in-flight `tools/call`.
///
/// The transient capability set is installed before the request and
/// cleared here, on every exit: success, server error, transport
/// failure, timeout, cancellation and panic alike. The clear lives in
/// `Drop` precisely so that no early `return` and no dropped future can
/// leave a session holding authority for a call that is no longer
/// running.
///
/// A call that did not complete, or a clear that failed, is not merely
/// reported — the worker is killed. A server that hung up mid-request
/// may have observed the grant, and the only safe assumption about what
/// it is doing with it is that it should stop.
struct ActiveCallGuard {
    control: crate::bridge::AppSessionControl,
    child_pid: u32,
    completed: bool,
    poisoned: Arc<AtomicBool>,
    /// The launch's sandbox, so a kill here reaches the whole cgroup
    /// and process group rather than the direct child alone.
    sandbox: Arc<LaunchResources>,
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
            self.sandbox.kill_all(Some(self.child_pid));
        }
    }
}

async fn begin_active_session_call(
    app_id: &str,
    apps_root: &Path,
    caps: &[crate::caps::Cap],
    authorization: &str,
    action_digest: &str,
) -> Result<ActiveCallGuard, String> {
    let key = session_key(app_id, apps_root)?;
    let (control, child_pid, call_lock, poisoned, session_id, bound, sandbox) = {
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
            Arc::clone(&session.sandbox),
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
    if control.uses_local_backend() {
        let owner = crate::provenance::runtime::current_owner();
        if let Err(reason) =
            crate::provenance::runtime::assert_live_instance_now(owner, &session_id)
        {
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
    }
    // Taken before the grant is installed and released only when the
    // guard drops, so two concurrent tool calls against one session can
    // never overlap each other's transient capabilities.
    let lock = call_lock.lock_owned().await;
    if let Err(error) = control.set_transient_call(Some(crate::bridge::TransientCall {
        authorization,
        action_digest,
        caps: crate::caps::CapSet::from_caps(caps.iter().cloned()),
    })) {
        let clear_error = control.set_transient_call(None).err();
        poisoned.store(true, Ordering::SeqCst);
        sandbox.kill_all(Some(child_pid));
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
        sandbox,
        _lock: lock,
    })
}

/// Explicitly bring up `app_id`. Returns `(client, tool_count)`.
/// Idempotent: returns the existing session if one is already open and
/// still matches what a launch started now would produce.
///
/// Race safety: an earlier implementation released the manager mutex
/// between the "is there a session?" probe and the spawn. Two callers
/// racing on the same app would each see "no session", each spawn a
/// child, and the slower one would overwrite the faster's table entry —
/// leaving an orphan child whose stdin/stdout get dropped immediately.
/// A *per-app* mutex is held across the whole
/// verify-then-probe-then-launch-then-insert sequence, so exactly one
/// child is created per app per process.
///
/// Order matters here: the package is re-verified and the launch plan
/// resolved *before* the cache is consulted, because the cache is only
/// allowed to answer a question whose terms come from fresh,
/// authenticated data.
async fn open_session_at(
    app_id: &str,
    app_dir: &Path,
    apps_root: &Path,
    tool: &str,
) -> Result<(Arc<McpClient>, usize), HostedAppError> {
    let key = session_key(app_id, apps_root)?;
    let lock = app_open_lock(&key);
    let _open_guard = lock.lock().await;

    // Registration happened earlier and the tool schemas the model saw
    // came from that snapshot; the server starts now. Re-verify before
    // bring-up so a package revoked, replaced or tampered with in
    // between cannot be what actually starts — or what a warm cache
    // hands back.
    //
    // A failure here is not just "no new session": whatever is already
    // running under this key came from bytes the trust store has since
    // disowned, and it holds a live channel to the agent. The entry is
    // evicted first — which kills its process group — and only then is
    // the refusal returned.
    let resolved = resolve_session_launch(app_id, app_dir, apps_root, key.0);
    let (verified, launch, plan) = match resolved {
        Ok(resolved) => resolved,
        Err(error) => {
            if close_session_key(&key).await {
                tracing::warn!(
                    target: "provenance",
                    app = %app_id,
                    %error,
                    "shut down an open App session whose package no longer verifies"
                );
            }
            return Err(error.into());
        }
    };
    // Probe under the per-app lock — another racer may have just
    // finished the launch we were blocked on. A cached entry is reused
    // only when its files, its runtime trust record, its package
    // identity and its enforced sandbox policy still match; anything
    // else is evicted, and dropping the entry kills that worker's
    // process group.
    let stale = {
        let mut table = manager().lock().await;
        if let Some(s) = table.get(&key) {
            if !s.poisoned.load(Ordering::SeqCst) && reusable(s, &plan) {
                return Ok((s.client.clone(), s.tool_count));
            }
        }
        table.remove(&key)
    };
    drop(stale);

    let BroughtUp {
        client,
        child,
        child_pid,
        tool_count,
        identity,
        bound,
        sandbox,
        policy_digest,
    } = bring_up_app(&launch, &plan, tool, DEFAULT_TIMEOUT).await?;
    // The App-session MCP child is a verified package holding a live
    // stdio channel to the agent. Record which artifact it came from
    // and which exact process it is, so a later revocation can both
    // deny it and stop it.
    let owner = crate::provenance::runtime::current_owner();
    if identity.uses_local_backend() {
        crate::provenance::runtime::register(owner, identity.id(), &verified);
        crate::provenance::runtime::bind_process(owner, identity.id(), child_pid);
    }
    let mut table = manager().lock().await;
    table.insert(
        key,
        ActiveSession {
            client: client.clone(),
            child: Some(child),
            tool_count,
            identity,
            call_lock: Arc::new(Mutex::new(())),
            child_pid,
            poisoned: Arc::new(AtomicBool::new(false)),
            bound: Arc::new(bound),
            launched_as: plan.identity,
            policy_digest,
            sandbox: Arc::new(sandbox),
        },
    );
    Ok((client, tool_count))
}

/// Re-verify the installed package and resolve everything one launch
/// needs from it.
///
/// One `AppLaunch` from one `VerifiedPackage`: the manifest, the runtime
/// selection, the `mcp` block, the capability ceiling and the executed
/// entry all come out of the same parse of the same signed bytes.
#[allow(clippy::type_complexity)]
fn resolve_session_launch(
    app_id: &str,
    app_dir: &Path,
    apps_root: &Path,
    owner_uid: u32,
) -> Result<
    (
        std::sync::Arc<crate::provenance::VerifiedPackage>,
        crate::bridge::AppLaunch,
        SessionLaunchPlan,
    ),
    String,
> {
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
    let verified = std::sync::Arc::clone(verified);
    let launch = crate::bridge::AppLaunch::new(std::sync::Arc::clone(&verified))?;
    let plan = plan_session_launch(owner_uid, &launch, apps_root)?;
    Ok((verified, launch, plan))
}

/// Close a session, dropping the handle (which kills the child).
/// Returns `true` if a session was found and closed.
///
/// We move the `ActiveSession` out of the table *before* dropping it
/// so the manager mutex isn't held across the kill+reap. The Drop
/// impl on `ActiveSession` kills the launch's cgroup and process group
/// and spawns a detached `wait()` task, so we don't block here either —
/// any in-flight `tools/call` against this session will return
/// `ConnectionClosed` once the child's stdio is torn down.
async fn close_session_key(key: &SessionKey) -> bool {
    let removed = {
        let mut table = manager().lock().await;
        table.remove(key)
    };
    let was_present = removed.is_some();
    if let Some(session) = removed
        .as_ref()
        .filter(|session| session.identity.uses_local_backend())
    {
        crate::provenance::runtime::deregister(
            crate::provenance::runtime::current_owner(),
            session.identity.id(),
        );
    }
    // Explicit drop here to make the lifetime obvious — the Drop
    // impl does the kill and the async reap.
    drop(removed);
    was_present
}

async fn close_session_at(app_id: &str, apps_root: &Path) -> bool {
    let Ok(key) = session_key(app_id, apps_root) else {
        return false;
    };
    close_session_key(&key).await
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

/// Locate the directories containing the `claw_os_sdk` and
/// `cos_runtime` Python packages.
///
/// Honours the path list in `COS_SDK_PYTHON_DIR` first, then falls back to the
/// production install path (`/usr/lib/cos/python`), and finally to
/// the in-repo dev-checkout paths at fixed offsets from
/// `$COS_APPS_DIR`. Returns the *distinct* candidates that actually
/// host one of the wanted packages, deduplicated and order-preserving.
fn resolve_python_pkg_dirs(apps_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(paths) = std::env::var_os("COS_SDK_PYTHON_DIR") {
        candidates
            .extend(std::env::split_paths(&paths).filter(|path| !path.as_os_str().is_empty()));
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
/// [`McpTool`](crate::caps::manifest::McpTool) in each
/// installed app's manifest. The session itself is opened lazily on
/// the first authenticated tool call.
pub struct AppSessionTool {
    /// Format: `app_<id>__<tool_name_dots_to_underscores>`.
    name: String,
    /// Description built from the tool's summary.
    description: String,
    /// JSON Schema derived from `manifest.mcp.tools[i].args`.
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
    ) -> Result<Self, String> {
        let service = manifest
            .mcp
            .as_ref()
            .expect("from_manifest_tool requires an `mcp` block");
        let tool = &service.tools[tool_idx];
        let app_id = manifest.id.clone();
        let manifest_tool_name = tool.name.clone();
        let invoke_scope = super::app_gateway::invoke_cap(&app_id, &manifest_tool_name)
            .map_err(|error| format!("invalid MCP invocation target: {error}"))?
            .scope;
        let name = registry_name_for(&app_id, &manifest_tool_name);
        let description = format!(
            "App `{app_id}` mcp tool `{manifest_tool_name}`. {}",
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
            app_dir,
            apps_root,
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

impl AppSessionTool {
    /// Serve one call in a worker that exists only for it.
    ///
    /// Used when the call was granted filesystem or network reach the
    /// reusable server's fixed policy does not have. The worker's
    /// mounts and egress are derived from exactly this call's
    /// capability set, its transient grant is installed on its own
    /// kernel session, and the whole cgroup and process group is
    /// destroyed before this function returns — on the success path,
    /// the error path, the timeout path and the panic path alike.
    ///
    /// State the reusable server holds is deliberately not visible
    /// here. A tool that needs both a resource grant and cross-call
    /// state cannot be served safely, and the manifest is the place to
    /// split it into an operation and a session tool.
    async fn exec_ephemeral(
        &self,
        caps: &[crate::caps::Cap],
        authorized_mounts: &[crate::worker::AuthorizedMount],
        arguments: Option<Value>,
        context: super::app_gateway::McpCallContext,
        authorization: &str,
        action_digest: &str,
        started: Instant,
    ) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, HostedAppError> {
        let cap_set = crate::caps::CapSet::from_caps(caps.iter().cloned());
        let fail = |error: HostedAppError| {
            let message = error.to_string();
            emit_audit(
                &self.app_id,
                &self.manifest_tool_name,
                verb_csv(caps).as_str(),
                "allowed",
                None,
                Some(&message),
                started.elapsed(),
            );
            Err(error)
        };

        let owner_uid = match session_key(&self.app_id, &self.apps_root) {
            Ok(key) => key.0,
            Err(error) => {
                return fail(HostedAppError::remote(format!(
                    "resolve App session owner: {error}"
                )));
            }
        };
        let (_, launch, plan) =
            match resolve_session_launch(&self.app_id, &self.app_dir, &self.apps_root, owner_uid) {
                Ok(resolved) => resolved,
                Err(error) => {
                    return fail(HostedAppError::remote(format!(
                        "could not bring up app `{}`: {error}",
                        self.app_id
                    )));
                }
            };

        let mut worker = match SingleCallWorker::start(
            &launch,
            &plan,
            &self.manifest_tool_name,
            &cap_set,
            authorized_mounts,
        )
        .await
        {
            Ok(worker) => worker,
            Err(error) => {
                return fail(HostedAppError::connect(format!(
                    "could not bring up a single-call worker for `{}`: {error}",
                    self.app_id
                )));
            }
        };

        // The App is still blocked in the trusted runner here. Installing the
        // grant and releasing exec are one ordered operation, and the guard
        // clears the grant on every later exit.
        let mut guard = match worker.authorize(caps, authorization, action_digest).await {
            Ok(guard) => guard,
            Err(error) => {
                let category = error.category();
                worker.destroy();
                return fail(HostedAppError::new(
                    category,
                    format!(
                        "could not grant App `{}` call capabilities: {error}",
                        self.app_id
                    ),
                ));
            }
        };
        let client = match worker.client() {
            Ok(client) => client,
            Err(error) => {
                drop(guard);
                worker.destroy();
                return fail(error);
            }
        };

        let initialize_timeout = match context.remaining(self.timeout) {
            Ok(timeout) => timeout,
            Err(error) => {
                drop(guard);
                worker.destroy();
                return fail(HostedAppError::remote(error));
            }
        };
        if let Err(error) = worker.initialize(initialize_timeout).await {
            let category = error.category();
            drop(guard);
            worker.destroy();
            return fail(HostedAppError::new(
                category,
                format!(
                    "could not initialize a single-call worker for `{}`: {error}",
                    self.app_id
                ),
            ));
        }
        let effective_timeout = match context.remaining(self.timeout) {
            Ok(timeout) => timeout,
            Err(error) => {
                guard.complete();
                drop(guard);
                worker.destroy();
                return fail(HostedAppError::remote(error));
            }
        };
        let call =
            client.call_tool_with_context(self.manifest_tool_name.clone(), arguments, context);
        let outcome = timeout(effective_timeout, call).await;
        // Order matters: clear the grant, then destroy the worker. A
        // clear that fails leaves the guard poisoned, and the worker is
        // torn down either way.
        guard.complete();
        drop(guard);
        worker.destroy();

        match outcome {
            Ok(Ok(call_result)) => {
                let is_error = call_result.is_error == Some(true);
                emit_audit(
                    &self.app_id,
                    &self.manifest_tool_name,
                    verb_csv(caps).as_str(),
                    "allowed",
                    None,
                    is_error.then_some("App returned a tool error"),
                    started.elapsed(),
                );
                Ok(call_result)
            }
            Ok(Err(error)) => fail(hosted_client_error(
                &format!(
                    "app `{}` tool `{}` failed",
                    self.app_id, self.manifest_tool_name
                ),
                error,
            )),
            Err(_) => fail(HostedAppError::timeout(format!(
                "app `{}` tool `{}` timed out after {}s",
                self.app_id,
                self.manifest_tool_name,
                effective_timeout.as_secs()
            ))),
        }
    }
}

/// RAII holder for the transient grant on a single-call worker.
///
/// The reusable path's [`ActiveCallGuard`] also owns a session-wide
/// lock and a poison flag, neither of which means anything for a worker
/// that is about to be destroyed. What both share is the invariant that
/// matters: the grant is cleared on every exit, and a clear that fails
/// is loud.
struct SingleCallGrant {
    control: crate::bridge::AppSessionControl,
    completed: bool,
}

impl SingleCallGrant {
    fn install(
        control: crate::bridge::AppSessionControl,
        caps: &[crate::caps::Cap],
        authorization: &str,
        action_digest: &str,
    ) -> Result<Self, String> {
        control.set_transient_call(Some(crate::bridge::TransientCall {
            authorization,
            action_digest,
            caps: crate::caps::CapSet::from_caps(caps.iter().cloned()),
        }))?;
        Ok(Self {
            control,
            completed: false,
        })
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for SingleCallGrant {
    fn drop(&mut self) {
        if let Err(error) = self.control.set_transient_call(None) {
            tracing::warn!(
                error = %error,
                completed = self.completed,
                "failed to clear a single-call App grant; the worker is destroyed regardless"
            );
        }
    }
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
            "app-mcp",
            Some(self.app_id.clone()),
            Some(self.manifest_tool_name.clone()),
            ["app".to_string(), "mcp".to_string()],
        )
    }

    async fn exec(&self, input: Value) -> ToolResult {
        let started = Instant::now();
        let supplied_args = json_to_arg_map(&input);
        let paths = match crate::bridge::launcher_path_context() {
            Ok(paths) => paths,
            Err(error) => return ToolResult::err(format!("resolve App paths: {error}")),
        };
        let effective = match self.manifest.resolve_mcp_tool_call(
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

        if let Err(denial) =
            crate::caps::require(crate::caps::Verb::AGENT_INVOKE, self.invoke_scope.clone())
        {
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

        let Some(host) = crate::extension_host::client::current() else {
            let error = "App MCP calls require the authenticated task App Host".to_string();
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
        };
        let authority = super::app_gateway::McpCallContext::for_extension_caller_with_generation(
            host.binding(),
            host.lease_deadline_ms(),
            self.timeout,
        );
        let (context, _) = match authority {
            Ok(authority) => authority,
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
        if let Err(error) = super::app_gateway::authorize_manifest(&self.manifest, &context.caller)
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

        match host
            .call_app(
                self.app_id.clone(),
                self.manifest_tool_name.clone(),
                Value::Object(args_map.into_iter().collect()),
                context,
                self.timeout,
            )
            .await
        {
            Ok(result) => {
                let (content, is_error) = render_call_result(result);
                if is_error {
                    ToolResult::err(content)
                } else {
                    ToolResult::ok(content)
                }
            }
            Err(error) => ToolResult::err(error),
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
        crate::agent::safety::untrusted::wrap_labeled(
            crate::agent::trust::SourceKind::AppToolResult,
            None,
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
    // tools from MCP App tools without parsing model strings.
    rec.provider = format!("app:{app_id}");
    record_run(&rec);
}

fn resolve_daemon_authorized_call(
    manifest: &Manifest,
    tool_name: &str,
    input: &Value,
) -> Result<(BTreeMap<String, Value>, Vec<crate::caps::Cap>), String> {
    let supplied = input
        .as_object()
        .ok_or_else(|| "daemon-authorized App arguments must be an object".to_string())?
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    let args = manifest
        .resolve_mcp_tool_args(tool_name, &supplied)
        .map_err(|error| format!("argument validation failed: {error}"))?;
    if args != supplied {
        return Err(
            "App arguments changed after daemon authorization; refusing execution".to_string(),
        );
    }
    let caps = manifest
        .resolve_mcp_tool_needs(tool_name, &args)
        .map_err(|error| format!("capability resolution failed: {error}"))?
        .into_iter()
        .flatten()
        .collect();
    Ok((args, caps))
}

pub(crate) async fn host_call_session(
    app_id: &str,
    tool_name: &str,
    input: Value,
    authorized_mounts: Vec<crate::worker::AuthorizedMount>,
    context: super::app_gateway::McpCallContext,
    authorization: String,
    call_timeout: Duration,
) -> Result<crate::agent::tools::mcp::protocol::CallToolResult, HostedAppError> {
    context.validate()?;
    let action_digest = crate::clawd::app_services::app_call_action_digest(
        app_id,
        tool_name,
        &input,
        &context,
        &authorized_mounts,
    )?;
    let root = apps_root();
    let app = match crate::apps::find_verified_fresh(&root, app_id) {
        Ok(app) => app,
        Err(error) => {
            close_session_at(app_id, &root).await;
            return Err(error.into());
        }
    };
    super::app_gateway::authorize_manifest(&app.manifest, &context.caller)?;
    let (args, caps) = resolve_daemon_authorized_call(&app.manifest, tool_name, &input)?;
    let maximum_timeout = call_timeout.min(DEFAULT_TIMEOUT);
    context.remaining(maximum_timeout)?;
    let placement = classify_call(&caps);
    if let CallPlacement::Unsupported(reason) = &placement {
        return Err(HostedAppError::remote(format!(
            "app `{app_id}` tool `{tool_name}` cannot be authorized: {reason}"
        )));
    }
    if matches!(placement, CallPlacement::Reusable) && !authorized_mounts.is_empty() {
        return Err(HostedAppError::remote(
            "reusable App call received call-scoped mount authority",
        ));
    }
    let arguments = (!args.is_empty()).then(|| Value::Object(args.clone().into_iter().collect()));
    if matches!(placement, CallPlacement::Ephemeral) {
        let tool_index = app
            .manifest
            .mcp
            .as_ref()
            .and_then(|service| {
                service
                    .tools
                    .iter()
                    .position(|candidate| candidate.name == tool_name)
            })
            .ok_or_else(|| format!("App `{app_id}` has no MCP tool `{tool_name}`"))?;
        let mut tool = AppSessionTool::from_manifest_tool(
            Arc::new(app.manifest.clone()),
            app.dir.clone(),
            root,
            tool_index,
        )?;
        tool.timeout = tool.timeout.min(maximum_timeout);
        return tool
            .exec_ephemeral(
                &caps,
                &authorized_mounts,
                arguments,
                context,
                &authorization,
                &action_digest,
                Instant::now(),
            )
            .await;
    }
    let client = get_or_open(app_id, &app.dir, &root, tool_name).await?;
    let mut active =
        begin_active_session_call(app_id, &root, &caps, &authorization, &action_digest).await?;
    let effective_timeout = context.remaining(maximum_timeout)?;
    match timeout(
        effective_timeout,
        client.call_tool_with_context(tool_name, arguments, context),
    )
    .await
    {
        Ok(Ok(result)) => {
            active.mark_completed();
            Ok(result)
        }
        Ok(Err(error)) => {
            let category = client_error_category(&error);
            if matches!(error, ClientError::Server { .. }) {
                active.mark_completed();
            } else {
                drop(active);
                close_session_at(app_id, &root).await;
            }
            Err(HostedAppError::new(
                category,
                format!("app `{app_id}` tool `{tool_name}` failed: {error}"),
            ))
        }
        Err(_) => {
            drop(active);
            close_session_at(app_id, &root).await;
            Err(HostedAppError::timeout(format!(
                "app `{app_id}` tool `{tool_name}` timed out after {}s",
                effective_timeout.as_secs()
            )))
        }
    }
}

pub(crate) async fn host_warm_session(app_id: &str) -> Result<(), HostedAppError> {
    let root = apps_root();
    let app = crate::apps::find_verified_fresh(&root, app_id)?;
    let first_tool = app
        .manifest
        .mcp
        .as_ref()
        .and_then(|service| service.tools.first())
        .ok_or_else(|| format!("App `{app_id}` has no MCP tools"))?;
    get_or_open(app_id, &app.dir, &root, &first_tool.name).await?;
    Ok(())
}

pub(crate) async fn host_close_all_sessions() {
    let sessions = {
        let mut table = manager().lock().await;
        std::mem::take(&mut *table)
    };
    drop(sessions);
}

#[cfg(test)]
async fn open_session(app_id: &str, tool: &str) -> Result<(Arc<McpClient>, usize), HostedAppError> {
    let root = apps_root();
    let app = crate::apps::find(&root, app_id)
        .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
    open_session_at(app_id, &app.dir, &root, tool).await
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

/// Register one [`AppSessionTool`] per MCP tool declared in the verified
/// App manifests. MCP servers are not started here; the first authenticated
/// tool call starts the service lazily.
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
        let Some(service) = &manifest.mcp else {
            continue;
        };
        for idx in 0..service.tools.len() {
            match AppSessionTool::from_manifest_tool(
                Arc::clone(manifest),
                app.app_dir.clone(),
                apps_root.to_path_buf(),
                idx,
            ) {
                Ok(tool) => registry.register(Arc::new(tool)),
                Err(error) => {
                    tracing::warn!(
                        app = %manifest.id,
                        %error,
                        "skipping invalid MCP App tool"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/agent/tools/cos_apps_session.rs"
    ));
}
