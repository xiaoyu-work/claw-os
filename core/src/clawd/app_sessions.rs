//! Trusted App / MCP session authority.
//!
//! Every App session in the routed capability registry is root-owned
//! state that privileged providers later trust (see
//! `packages::authorize_package_session` and friends). This module is
//! the only place those rows are minted for an unprivileged caller, so
//! nothing here may treat a request field as authority:
//!
//! * identity (`session_id`, `group`, `app_id`, `role`, `tier`) is
//!   generated here, never copied out of the request;
//! * capabilities are derived from the *installed* manifest plus the
//!   schema-validated arguments the App will actually receive;
//! * the ceiling is the launcher's authenticated authority — either a
//!   trusted parent row resolved from the peer's process ancestry, or,
//!   when the peer belongs to no registered session, the daemon's own
//!   unprivileged home-bounded policy;
//! * `parent_caps` supplied by the caller may only *narrow* that
//!   ceiling, never widen it;
//! * anything above the ceiling needs an approved permission grant,
//!   which only the privileged approval helper can create.
//!
//! A launch that needs consent files one pending request per missing
//! capability, consumes nothing, and returns their ids so the same
//! launcher process can wait for the decisions and retry over its own
//! authenticated connection. Grants are only ever settled as a complete
//! set, so a launch never burns part of an approval.
//!
//! Registration returns an opaque grant handle. It references a grant
//! the capability authority holds, bound to the connecting launcher
//! process (uid, pid, start time, cgroup), short-lived for the bind
//! step, and never handed to the launched App — so an App cannot bind,
//! re-scope, or drop sessions, and an unrelated local process cannot
//! drive somebody else's launch. Binding derives the narrower session
//! grant the App itself runs under; see
//! [`crate::clawd::authority`].

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{json, Value};

use crate::apps::App;
use crate::caps::{Cap, CapSet, Manifest, Need, Role, Scope, ScopeBinding, ScopeKind, Verb};
use crate::clawd::protocol::BrokerError;
use crate::proc::SessionInfo;

use super::authority;
use super::client_identity::ClientIdentity;

/// How long an issued handle may be used to bind a child process.
/// Long enough for a slow interpreter start, short enough that a
/// forgotten handle is not standing authority.
const BIND_WINDOW: Duration = Duration::from_secs(120);

/// How long a launch grant may live. It outlives the bind window
/// because the same handle is what re-scopes an MCP session and
/// deregisters it, but it is still bounded: a launcher that never tears
/// its session down loses the grant, and the App loses the session
/// grant derived from it, at this deadline.
const LAUNCH_GRANT_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// How long a bound App session's own grant may live.
///
/// Strictly shorter than the launch grant it is derived from: an
/// attenuation may never extend an expiry, and leaving headroom keeps
/// a bind that happens seconds after registration from being refused
/// for asking for exactly as long as its parent has left.
const SESSION_GRANT_TTL: Duration = Duration::from_secs(11 * 60 * 60);
const TARGET_CALL_GRANT_TTL: Duration = Duration::from_secs(2 * 60);

/// What the launcher asked to run. The kind decides which part of the
/// manifest the capability derivation reads.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LaunchKind {
    Operation,
    Gui,
    Mcp,
}

impl LaunchKind {
    fn as_str(self) -> &'static str {
        match self {
            LaunchKind::Operation => "operation",
            LaunchKind::Gui => "gui",
            LaunchKind::Mcp => "mcp",
        }
    }
}

/// Authenticated authority of the process on the other end of the
/// socket. Resolved by `clawd`, never described by the caller.
#[derive(Debug)]
struct LauncherAuthority {
    pid: u32,
    start_time_ticks: Option<u64>,
    parent: Option<String>,
    caps: CapSet,
    tier: Option<u8>,
    scope: Option<String>,
    priority: Option<String>,
    role: Option<String>,
}

/// Everything the capability derivation is allowed to consult.
///
/// `ceiling` is the authenticated launcher authority, already narrowed
/// by any parent capabilities the caller reported. `grant_session` is
/// the identity an approved permission grant must be bound to. It is
/// the authenticated parent session when the daemon resolved one, and
/// otherwise a value derived purely from the peer's own uid, pid and
/// process start time — never anything the request supplied, and never
/// anything a sibling process shares, so one local process can never
/// present itself as another's launch context.
#[derive(Debug)]
struct Delegation {
    uid: u32,
    grant_session: String,
    requester: String,
    ceiling: CapSet,
    /// Where this launcher's relative and `~` path arguments resolve.
    /// Taken from the kernel's view of the peer — its passwd home and
    /// `/proc/<pid>/cwd` — so the scope derived for a path argument
    /// names the resource the App will actually touch.
    paths: crate::caps::args::PathContext,
}

impl Delegation {
    fn new(
        launcher: &LauncherAuthority,
        uid: u32,
        home: &std::path::Path,
        params: &Value,
    ) -> Result<Self, String> {
        let grant_session = match &launcher.parent {
            Some(parent) => parent.clone(),
            None => unregistered_grant_identity(uid, launcher.pid, launcher.start_time_ticks)?,
        };
        Ok(Self {
            uid,
            grant_session,
            requester: requester_identity(uid, launcher.pid, launcher.start_time_ticks),
            ceiling: attenuated_ceiling(launcher, params)?,
            paths: crate::caps::args::PathContext {
                home: home.to_path_buf(),
                cwd: process_cwd(launcher.pid),
            },
        })
    }
}

#[cfg(target_os = "linux")]
fn process_cwd(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

#[cfg(not(target_os = "linux"))]
fn process_cwd(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

/// One launch's complete capability set, split by what the launcher can
/// already delegate.
///
/// The plan is built in full before anything is authorised, so a launch
/// that needs several approvals asks for all of them at once and either
/// gets all of them or none.
#[derive(Debug, Default)]
struct LaunchPlan {
    caps: CapSet,
    missing: Vec<Cap>,
}

#[derive(Debug)]
struct McpCallAuthority {
    invoke: Cap,
    deadline_unix_ms: u64,
}

impl LaunchPlan {
    fn require(&mut self, cap: Cap, delegation: &Delegation) {
        if !delegation.ceiling.covers(&cap) && !self.missing.contains(&cap) {
            self.missing.push(cap.clone());
        }
        self.caps.insert(cap);
    }

    fn inherit(&mut self, caps: impl IntoIterator<Item = Cap>) {
        self.caps.extend(caps);
    }
}

/// Identity an approval grant is bound to when the launcher belongs to
/// no registered session.
///
/// Derived only from kernel-reported facts about this exact peer: its
/// uid, pid and process start time. Deliberately *not* a login session
/// or any other value shared with sibling processes — a sibling can
/// read another launcher's pending request but can never make the
/// daemon derive that launcher's identity for its own connection, so it
/// cannot consume an approval granted to someone else. A denied
/// launcher therefore stays alive, waits for the decision, and retries
/// over the same connection; nothing is carried between processes.
fn unregistered_grant_identity(
    uid: u32,
    pid: u32,
    start_time_ticks: Option<u64>,
) -> Result<String, String> {
    let ticks = start_time_ticks.ok_or_else(|| {
        format!("cannot establish a launch identity for process {pid} without its start time")
    })?;
    Ok(format!("app-launch:uid={uid}:pid={pid}:start={ticks}"))
}

/// Requester label recorded on an approval request so the approval UI
/// and audit trail can tell two launchers of the same user apart. It
/// carries authenticated facts only and is never used for matching, so
/// it is not a reusable secret.
fn requester_identity(uid: u32, pid: u32, start_time_ticks: Option<u64>) -> String {
    match start_time_ticks {
        Some(ticks) => format!("uid:{uid} pid:{pid} start:{ticks}"),
        None => format!("uid:{uid} pid:{pid}"),
    }
}

/// Routes this authority owns.
///
/// Each one is a row in [`crate::clawd::routes`] pointing straight at
/// the function below it, so there is no second dispatcher here that
/// could drift from the registry's access class, budget or typed
/// decode.
pub const COMMANDS: &[&str] = &[
    "app_session.register",
    "app_session.register_native",
    "mcp_session.register",
    "app_session.bind",
    "app_session.set_transient",
    "app_session.deregister",
];

fn mcp_launch_command_and_cap(
    app: &App,
    params: &Value,
) -> Result<(String, Cap), BrokerError> {
    let app_id = &app.manifest.id;
    if let Some(service) = app.manifest.mcp.as_ref() {
        let tool = required_string(params, "tool")?;
        if !service.tools.iter().any(|declared| declared.name == tool) {
            return Err(BrokerError::execution(format!(
                "App `{app_id}` manifest has no MCP tool `{tool}`"
            )));
        }
        Ok((
            format!("cos app {app_id} mcp {tool}"),
            crate::agent::tools::app_gateway::invoke_cap(app_id, &tool)
                .map_err(BrokerError::execution)?,
        ))
    } else {
        if params.get("tool").is_some() {
            return Err(BrokerError::execution(
                "legacy App session launch cannot name an MCP-first tool",
            ));
        }
        Ok((
            format!("cos app {app_id} session"),
            Cap::new(Verb::AGENT_INVOKE, Scope::name(app_id)),
        ))
    }
}

fn target_session_caps(
    authorized_caps: CapSet,
    mcp_first: bool,
    launch_invoke: &Cap,
) -> CapSet {
    if mcp_first {
        CapSet::from_caps(
            authorized_caps
                .iter()
                .filter(|cap| *cap != launch_invoke)
                .cloned(),
        )
    } else {
        authorized_caps
    }
}

pub async fn register(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let app_id = required_string(&params, "app_id")?;
    let kind = launch_kind(&params)?;
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    let delegation = Delegation::new(&launcher, uid, &home, &params)?;
    let app = installed_app(&app_id)?;

    let (command, mut plan, invoke) = match kind {
        LaunchKind::Operation => {
            if params.get("tool").is_some() {
                return Err(BrokerError::execution(
                    "operation launch cannot name an MCP tool",
                ));
            }
            let operation = required_string(&params, "operation")?;
            let args = string_array(&params, "args")?;
            let plan = operation_plan(&app, &operation, &args, &delegation)?;
            (
                format!("cos app {app_id} {operation}"),
                plan,
                Cap::new(Verb::AGENT_INVOKE, Scope::name(&app_id)),
            )
        }
        LaunchKind::Gui => {
            if params.get("tool").is_some() {
                return Err(BrokerError::execution(
                    "GUI launch cannot name an MCP tool",
                ));
            }
            let exec = required_string(&params, "operation")?;
            let plan = gui_plan(&app, &exec, &delegation)?;
            (
                format!("cos app {app_id} {exec}"),
                plan,
                Cap::new(Verb::AGENT_INVOKE, Scope::name(&app_id)),
            )
        }
        LaunchKind::Mcp => {
            let (command, invoke) = mcp_launch_command_and_cap(&app, &params)?;
            (command, LaunchPlan::default(), invoke)
        }
    };
    let mcp_first = kind == LaunchKind::Mcp && app.manifest.mcp.is_some();
    let launch_invoke = invoke.clone();
    plan.require(invoke, &delegation);
    let authorized_caps = authorize_plan(&delegation, plan)?;
    let grant_caps = authorized_caps.clone();
    // The caller's exact invoke capability authorizes this launch but is not
    // target authority. MCP-first App sessions start empty and receive only
    // the selected tool's manifest-derived transient capabilities per call.
    let caps = target_session_caps(authorized_caps, mcp_first, &launch_invoke);

    let session_id = format!("app-{}", uuid::Uuid::new_v4().simple());
    let info = SessionInfo {
        session_id: session_id.clone(),
        // Bound to the spawned child immediately after launch. App
        // sessions with pid 0 are denied by caps enforcement for the
        // duration of that window.
        pid: 0,
        command: vec![command],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("app".to_string()),
        parent: launcher.parent.clone(),
        workdir: Some(app.dir.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: Some(worker_floor(launcher.tier)),
        scope: launcher.scope.clone(),
        priority: launcher.priority.clone(),
        caps: Some(caps),
        transient_caps: None,
        role: launcher.role.clone(),
        app_id: Some(app_id.clone()),
        pending_bind: true,
        start_time_ticks: None,
        client: crate::session::SessionClient::new(crate::session::SessionSource::App, false, true),
    };

    let proc_dir = install_session(uid, home, info).await?;
    let handle = issue_launch_grant(&session_id, Some(&app_id), uid, &launcher, &grant_caps)?;
    Ok(json!({
        "session_id": session_id,
        "proc_data_dir": proc_dir,
        "app_id": app_id,
        "handle": handle,
    }))
}

pub async fn register_native(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let app_id = required_string(&params, "app_id")?;
    require_trusted_native_launcher(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    let app = installed_app(&app_id)?;
    // The native host is not an unauthenticated local launcher: its
    // executable and parent are pinned root-owned binaries, so its
    // authority is the installed manifest itself. `native_manifest_caps`
    // still refuses argument-bound needs, which have no invocation to
    // bind to here.
    let mut caps = native_manifest_caps(&app.manifest)?;
    caps.insert(Cap::new(Verb::AGENT_INVOKE, Scope::name(&app_id)));
    let session_id = format!("app-{}", uuid::Uuid::new_v4().simple());
    let role = Role::Worker;
    let info = SessionInfo {
        session_id: session_id.clone(),
        pid: 0,
        command: vec![format!("native host for {app_id}")],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("app".to_string()),
        parent: launcher.parent.clone(),
        workdir: Some(app.dir.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: Some(role.credential_tier()),
        scope: Some("native-host".to_string()),
        priority: None,
        caps: Some(caps.clone()),
        transient_caps: None,
        role: Some(role.name().to_string()),
        app_id: Some(app_id.clone()),
        pending_bind: true,
        start_time_ticks: None,
        client: crate::session::SessionClient::new(crate::session::SessionSource::App, false, true),
    };
    let proc_dir = install_session(uid, home, info).await?;
    let handle = issue_launch_grant(&session_id, Some(&app_id), uid, &launcher, &caps)?;
    Ok(json!({
        "session_id": session_id,
        "proc_data_dir": proc_dir,
        "app_id": app_id,
        "handle": handle,
    }))
}

pub async fn register_mcp(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let command = required_string(&params, "command")?;
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    let caps = attenuated_ceiling(&launcher, &params)?;
    if caps.is_empty() {
        return Err("launcher has no capabilities to delegate to an MCP child".to_string());
    }
    let session_id = format!("mcp-{}", uuid::Uuid::new_v4().simple());
    let info = SessionInfo {
        session_id: session_id.clone(),
        pid: 0,
        command: vec![command],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("mcp".to_string()),
        parent: launcher.parent.clone(),
        workdir: None,
        exit_code: None,
        ended_at: None,
        tier: launcher.tier,
        scope: launcher.scope.clone(),
        priority: launcher.priority.clone(),
        caps: Some(caps.clone()),
        transient_caps: None,
        role: launcher.role.clone(),
        app_id: None,
        pending_bind: true,
        start_time_ticks: None,
        client: crate::session::SessionClient::new(
            crate::session::SessionSource::ExternalMcp,
            false,
            true,
        ),
    };
    let proc_dir = install_session(uid, home, info).await?;
    let handle = issue_launch_grant(&session_id, None, uid, &launcher, &caps)?;
    Ok(json!({
        "session_id": session_id,
        "proc_data_dir": proc_dir,
        "handle": handle,
    }))
}

pub async fn bind(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let session_id = required_string(&params, "session_id")?;
    let handle = required_string(&params, "handle")?;
    let launcher_pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    // The route middleware already resolved this handle to a live
    // grant bound to this exact launcher process; what is left is the
    // route's own contract — the handle covers this session, it is
    // still inside the bind window, and the process being bound really
    // is the launcher's child.
    let launch = require_launch_grant(client, &handle, &session_id, uid)?;
    if launch.issued_ago > BIND_WINDOW {
        return Err("App launch handle expired before binding a process".to_string());
    }
    let child_pid = required_u32(&params, "pid")?;
    if child_pid == launcher_pid {
        return Err("App session must bind a child process".to_string());
    }
    if !is_descendant_of(child_pid, launcher_pid) {
        return Err(format!(
            "process {child_pid} is not descended from launcher {launcher_pid}"
        ));
    }
    let execution_uid = client.process_uid().unwrap_or(uid);
    if process_uid(child_pid) != Some(execution_uid) {
        return Err(format!(
            "App process {child_pid} is not owned by execution uid {execution_uid}"
        ));
    }
    let bind_id = session_id.clone();
    let bound_caps = crate::paths::with_user_override(uid, home, async move {
        crate::proc::bind_session_process(&bind_id, child_pid)?;
        crate::proc::session_info_by_id(&bind_id)
            .and_then(|session| session.caps)
            .ok_or_else(|| "App session lost its capabilities during bind".to_string())
    })
    .await?;
    // Deriving the session grant is what makes `bind` one-shot: the
    // store refuses a second claim on a session index, so two
    // concurrent binds cannot both install one.
    issue_session_grant(
        &handle,
        &session_id,
        launch.subject.app_id.as_deref(),
        uid,
        child_pid,
        &bound_caps,
    )?;
    Ok(json!({"bound": true}))
}

/// Serializer for one App session's capability transitions.
///
/// The registry swap and the grant re-derivation are two separate
/// critical sections — one over the routed registry file, one over the
/// authority store — and nothing in either makes them one. Two calls
/// left to interleave produce exactly the mismatch this path exists to
/// prevent:
///
/// ```text
///   A: swap(x)                              registry = x
///   B:          swap(y)                     registry = y
///   B:          reissue(y)                  authority = y
///   A: reissue(x)                           authority = x   <-- registry says y
/// ```
///
/// and a teardown landing in the same window leaves a live grant for a
/// row that no longer exists. So every transition for a given session —
/// re-scope, clear and deregistration alike — takes this lock first and
/// holds it across both halves, including the rollback. It is keyed by
/// session id, so unrelated Apps never wait on each other, and the
/// entry is dropped when the session is deregistered.
fn session_locks() -> &'static Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    LOCKS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_lock(session_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let mut locks = session_locks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    locks
        .entry(session_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

/// Forget a session's serializer once nothing is waiting on it.
///
/// Dropping the entry unconditionally would let a caller already
/// blocked on the old mutex run alongside a later caller that got a
/// fresh one. The strong count tells us whether that can happen: two
/// means the map and this caller, and nobody else. Anything higher
/// leaves the entry in place, and the next teardown collects it.
fn release_session_lock(session_id: &str) {
    let mut locks = session_locks()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let is_idle = locks
        .get(session_id)
        .map(|lock| Arc::strong_count(lock) <= 2)
        .unwrap_or(false);
    if is_idle {
        locks.remove(session_id);
    }
}

pub async fn set_transient(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let session_id = required_string(&params, "session_id")?;
    let handle = required_string(&params, "handle")?;
    require_launch_grant(client, &handle, &session_id, uid)?;

    // Held across the read, the write and the re-derivation, so a
    // concurrent re-scope, clear or teardown cannot land between them.
    let serializer = session_lock(&session_id);
    let _transition = serializer.lock().await;

    // Everything the new grant needs is read under the owner's own path
    // view — the routed registry is partitioned per uid, and reading
    // the daemon's own partition would answer about a different
    // session entirely.
    let lookup_id = session_id.clone();
    let bound = crate::paths::with_user_override(uid, home.clone(), async move {
        crate::proc::session_info_by_id(&lookup_id)
    })
    .await
    .ok_or_else(|| "App session not found".to_string())?;
    let app_id = bound
        .app_id
        .clone()
        .ok_or_else(|| "App session not found".to_string())?;
    if bound.pending_bind || bound.pid == 0 {
        return Err("App session is not bound to a process".to_string());
    }
    let child_pid = bound.pid;

    // Derive and authorize the requested capabilities *before* anything
    // is written. A launch that cannot settle its approvals leaves both
    // the registry and the authority untouched.
    let (caps, mcp_call_authority) = match params.get("call") {
        None | Some(Value::Null) => (None, None),
        Some(call) => {
            let launcher = authenticate_launcher(client, uid, home.clone()).await?;
            // A serialized MCP call is answered in the caller's own
            // process, so a denial is retried under the same
            // pid/start identity.
            let delegation = Delegation::new(&launcher, uid, &home, &params)?;
            let (plan, caller_authority) =
                session_tool_plan(&app_id, call, &delegation).map_err(|error| error.message)?;
            let authorized =
                authorize_plan(&delegation, plan).map_err(|error| error.message)?;
            (
                Some(match caller_authority.as_ref() {
                    Some(authority) => {
                        target_session_caps(authorized, true, &authority.invoke)
                    }
                    None => authorized,
                }),
                caller_authority,
            )
        }
    };

    let mut effective = bound.caps.clone().unwrap_or_else(CapSet::new);
    if let Some(transient) = caps.as_ref() {
        effective.extend(transient.iter().cloned());
    }

    // Commit the two halves as one security transaction. The registry
    // write comes first because it is the one that can be rolled back
    // exactly; if re-deriving the grant fails for any reason — the
    // attenuation is refused, the process disappeared, the store hit a
    // ceiling — the previous transient set is restored before the error
    // is returned, so a failed call never leaves widened authority
    // behind for `caps::require` or a later peer-session grant to find.
    let write_id = session_id.clone();
    let write_caps = caps.clone();
    let write_home = home.clone();
    let previous = crate::paths::with_user_override(uid, write_home, async move {
        crate::proc::swap_app_session_transient_caps(&write_id, write_caps)
    })
    .await?;

    let grant = if let Some(authority) = mcp_call_authority.filter(|_| !effective.is_empty()) {
        issue_gateway_target_grant(
            &session_id,
            &app_id,
            uid,
            child_pid,
            &effective,
            authority.deadline_unix_ms,
        )
    } else {
        reissue_session_grant(
            &handle,
            &session_id,
            Some(&app_id),
            uid,
            child_pid,
            &effective,
        )
    };
    if let Err(error) = grant {
        rollback_transient_caps(uid, home, &session_id, previous).await;
        return Err(error);
    }
    Ok(json!({"updated": true}))
}

/// Put an App session's transient capabilities back the way they were.
///
/// Best effort by necessity — the registry write that failed the
/// operation may also fail here — but the failure mode is deliberately
/// loud *and* narrowing: if the restore cannot be written, the session
/// grant is revoked outright, so the App is left with no authority
/// rather than the widened set the caller could not be given.
async fn rollback_transient_caps(
    uid: u32,
    home: std::path::PathBuf,
    session_id: &str,
    previous: Option<CapSet>,
) {
    let restore_id = session_id.to_string();
    let restored = crate::paths::with_user_override(uid, home, async move {
        crate::proc::set_app_session_transient_caps(&restore_id, previous)
    })
    .await;
    if let Err(error) = restored {
        tracing::error!(
            error = %error,
            "could not restore App session transient capabilities; revoking its authority"
        );
    }
    // Either way the grant that matched the old set is gone: `bind`
    // installed it, `reissue` revoked it, and the re-derivation is what
    // failed. Revoking again is idempotent and guarantees no live grant
    // outlives the transient state it was derived from.
    authority::revoke_indexed_session(session_id);
}

pub async fn deregister(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let session_id = required_string(&params, "session_id")?;
    let handle = required_string(&params, "handle")?;
    let launch = require_launch_grant(client, &handle, &session_id, uid)?;
    // Teardown is a capability transition like any other: taking the
    // same serializer is what stops a deregistration from racing an
    // in-flight re-scope and leaving a grant behind for a row that is
    // already gone.
    let serializer = session_lock(&session_id);
    let transition = serializer.lock().await;
    let remove_id = session_id.clone();
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::deregister_session(&remove_id);
    })
    .await;
    // Revoking the launch grant cascades to the session grant derived
    // from it, so an App whose session row is gone also loses every
    // provider route in the same transaction.
    authority::authority().revoke(launch.id);
    authority::revoke_indexed_session(&session_id);
    super::authority::audit::record_revoked("app-session", Some(&session_id), 1);
    // Drop the guard before forgetting the entry, so a caller already
    // waiting on it is released rather than stranded on a mutex nothing
    // will ever unlock.
    drop(transition);
    release_session_lock(&session_id);
    Ok(json!({"removed": true}))
}

/// Re-resolve the launch grant this route was admitted under.
///
/// The middleware already proved the handle belongs to this process; a
/// route still states its own contract, because the middleware knows
/// nothing about which session a launch handle is allowed to name.
fn require_launch_grant(
    client: &ClientIdentity,
    handle: &str,
    session_id: &str,
    uid: u32,
) -> Result<authority::GrantView, String> {
    let pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    let view = authority::authority()
        .resolve(
            handle,
            &authority::Presentation {
                uid,
                pid,
                start_time_ticks: client.start_time_ticks,
                audience: authority::Audience::AppLaunch,
                route: "app_session",
                session_id: None,
            },
        )
        .map_err(|error| error.to_string())?;
    if view.subject.session_id.as_deref() != Some(session_id) {
        return Err("App launch handle does not cover this session".to_string());
    }
    Ok(view)
}

// ---------------------------------------------------------------------------
// Launcher authentication
// ---------------------------------------------------------------------------

/// Resolve the authority of the process on the other end of the socket.
///
/// The peer must own its own process and must not be running inside an
/// App session — a launched App may never mint further sessions. When
/// the peer descends from a session `clawd` itself registered in the
/// root-owned routed registry (a routed cron job, an agent-launched
/// child), that row is the parent and its capabilities are the ceiling.
/// Otherwise the daemon's own local-launcher policy applies; the
/// caller never supplies either value.
async fn authenticate_launcher(
    client: &ClientIdentity,
    uid: u32,
    home: std::path::PathBuf,
) -> Result<LauncherAuthority, String> {
    let pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    if pid <= 1 {
        return Err("clawd peer pid is not a launchable process".to_string());
    }
    let execution_uid = client.process_uid().unwrap_or(uid);
    if process_uid(pid) != Some(execution_uid) {
        return Err(format!(
            "clawd peer {pid} is not owned by execution uid {execution_uid}"
        ));
    }
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid);
    let sessions = crate::paths::with_user_override(uid, home.clone(), async {
        crate::proc::registry_sessions()
    })
    .await;
    let trusted_extension_host =
        is_trusted_extension_host_launcher(&sessions, pid, start_time_ticks);
    if process_no_new_privs(pid) != Some(false) && !trusted_extension_host {
        return Err("App processes cannot manage App sessions".to_string());
    }

    launcher_authority(&sessions, pid, start_time_ticks, &home)
}

fn is_trusted_extension_host_launcher(
    sessions: &[SessionInfo],
    pid: u32,
    start_time_ticks: Option<u64>,
) -> bool {
    sessions.iter().any(|session| {
        session.group.as_deref() == Some(crate::extension_host::protocol::EXTENSION_HOST_GROUP)
            && session.app_id.is_none()
            && session.pid == pid
            && session.start_time_ticks == start_time_ticks
            && session_process_is_current(session)
    })
}

fn launcher_authority(
    sessions: &[SessionInfo],
    pid: u32,
    start_time_ticks: Option<u64>,
    home: &std::path::Path,
) -> Result<LauncherAuthority, String> {
    match nearest_registered_session(sessions, pid)? {
        Some(session) if session.app_id.is_some() => Err(format!(
            "App session `{}` cannot register further App sessions",
            session.session_id
        )),
        Some(session) => Ok(LauncherAuthority {
            pid,
            start_time_ticks,
            parent: Some(session.session_id.clone()),
            caps: session.caps.clone().unwrap_or_else(CapSet::new),
            tier: session.tier,
            scope: session.scope.clone(),
            priority: session.priority.clone(),
            role: session.role.clone(),
        }),
        None => Ok(LauncherAuthority {
            pid,
            start_time_ticks,
            parent: None,
            caps: super::system_caps::local_launcher_ceiling(home),
            tier: Some(Role::Worker.credential_tier()),
            scope: None,
            priority: None,
            role: Some(Role::Worker.name().to_string()),
        }),
    }
}

/// Walk the peer's process ancestry looking for a session `clawd`
/// registered itself.
///
/// `Ok(None)` means the walk completed and found nothing — the peer is
/// genuinely unregistered. An unreadable `/proc` entry, an ancestor
/// that exited mid-walk, or a chain longer than the budget is *not*
/// the same thing: the daemon cannot prove the peer is outside a
/// registered session, so it reports an error and the caller fails
/// closed rather than dropping to the unregistered policy.
fn nearest_registered_session(
    sessions: &[SessionInfo],
    pid: u32,
) -> Result<Option<&SessionInfo>, String> {
    let mut current = pid;
    for _ in 0..64 {
        if let Some(session) = sessions.iter().find(|session| {
            !session.pending_bind && session.pid == current && session_process_is_current(session)
        }) {
            return Ok(Some(session));
        }
        if current <= 1 {
            return Ok(None);
        }
        current = process_parent_pid(current).ok_or_else(|| {
            format!("could not resolve the launcher's ancestry above process {current}")
        })?;
    }
    Err(format!(
        "launcher ancestry for process {pid} is longer than the supported depth"
    ))
}

fn session_process_is_current(session: &SessionInfo) -> bool {
    if session.pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        match session.start_time_ticks {
            Some(expected) => crate::proc::read_start_time_ticks_pub(session.pid) == Some(expected),
            None => false,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Narrow the authenticated ceiling by the parent capabilities the
/// caller reports.
///
/// A launcher may honestly describe *less* authority than the daemon
/// resolved for it — `cos proc spawn --role observer` should not hand
/// an App more than the observer session holds. Because the caller's
/// set is only ever intersected against the trusted side, an inflated
/// or fabricated set cannot widen the result.
fn attenuated_ceiling(launcher: &LauncherAuthority, params: &Value) -> Result<CapSet, String> {
    match params.get("parent_caps") {
        None | Some(Value::Null) => Ok(launcher.caps.clone()),
        Some(value) => {
            let declared: CapSet = serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid parent capabilities: {error}"))?;
            Ok(launcher.caps.intersect(&declared))
        }
    }
}

fn worker_floor(tier: Option<u8>) -> u8 {
    tier.unwrap_or_else(|| Role::Worker.credential_tier())
        .max(Role::Worker.credential_tier())
}

// ---------------------------------------------------------------------------
// Capability derivation
// ---------------------------------------------------------------------------

fn installed_app(app_id: &str) -> Result<App, String> {
    let apps_dir = std::path::PathBuf::from(
        std::env::var("COS_APPS_DIR").unwrap_or_else(|_| "/usr/lib/cos/apps".to_string()),
    );
    let app = crate::apps::find(&apps_dir, app_id)
        .ok_or_else(|| format!("App `{app_id}` is not installed"))?;
    if app.manifest.id != app_id {
        return Err(format!(
            "installed manifest declares id `{}`, not `{app_id}`",
            app.manifest.id
        ));
    }
    Ok(app)
}

/// Build the capability plan for one manifest operation.
fn operation_plan(
    app: &App,
    operation: &str,
    args: &[String],
    delegation: &Delegation,
) -> Result<LaunchPlan, BrokerError> {
    if operation == "__schema__" {
        return Err("App schema inspection does not run App code"
            .to_string()
            .into());
    }
    let declared = app
        .manifest
        .operations
        .get(operation)
        .ok_or_else(|| format!("App `{}` has no operation `{operation}`", app.manifest.id))?;
    let supplied = crate::caps::args::bind_supplied_cli_args(&declared.args, args)
        .map_err(|error| format!("App `{}` operation `{operation}`: {error}", app.manifest.id))?;
    let effective = app
        .manifest
        .resolve_operation_call(operation, &supplied, &delegation.paths)
        .map_err(|error| format!("resolve `{operation}` capabilities: {error}"))?;
    derive_plan(&declared.needs, &effective.needs, delegation)
}

fn session_tool_plan(
    app_id: &str,
    call: &Value,
    delegation: &Delegation,
) -> Result<(LaunchPlan, Option<McpCallAuthority>), BrokerError> {
    let app = installed_app(app_id)?;
    let tool_name = required_string(call, "tool")?;
    let args: BTreeMap<String, Value> = match call.get("args") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Some(_) => return Err("session tool args must be an object".to_string().into()),
    };
    let tool = app
        .manifest
        .mcp_service()
        .and_then(|session| session.tools.iter().find(|tool| tool.name == tool_name))
        .ok_or_else(|| format!("App `{app_id}` has no session tool `{tool_name}`"))?;
    let effective = app
        .manifest
        .resolve_session_tool_call(&tool_name, &args, &delegation.paths)
        .map_err(|error| format!("resolve `{tool_name}` capabilities: {error}"))?;
    let mut plan = derive_plan(&tool.needs, &effective.needs, delegation)?;
    let caller_authority = if app.manifest.mcp.is_some() {
        let invoke = crate::agent::tools::app_gateway::invoke_cap(app_id, &tool_name)
            .map_err(BrokerError::execution)?;
        let deadline_unix_ms = call
            .get("deadline_unix_ms")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                BrokerError::execution("MCP-first App call requires an absolute deadline")
            })?;
        let now = crate::agentd::grant::now_ms();
        if deadline_unix_ms <= now
            || deadline_unix_ms
                > now.saturating_add(crate::extension_host::protocol::MAX_REQUEST_TIMEOUT_MS)
        {
            return Err(BrokerError::execution(
                "MCP-first App call deadline is outside the allowed range",
            ));
        }
        plan.require(invoke.clone(), delegation);
        Some(McpCallAuthority {
            invoke,
            deadline_unix_ms,
        })
    } else {
        None
    };
    Ok((plan, caller_authority))
}

/// Turn manifest needs into a complete capability plan.
///
/// `resolved` is the canonical output of `Manifest::resolve_needs` /
/// `resolve_session_tool_needs`, positionally aligned with `needs`, so
/// an argument-bound scope is planned against the exact value derived
/// from the invocation rather than against the scope *kind*. A declared
/// wildcard inherits only what the launcher actually holds for that
/// verb, which keeps a wildcard need from widening authority. Nothing
/// is authorised here: capabilities the launcher cannot cover are
/// collected so the whole launch can be decided at once.
fn derive_plan(
    needs: &[Need],
    resolved: &[Vec<Cap>],
    delegation: &Delegation,
) -> Result<LaunchPlan, BrokerError> {
    if needs.len() != resolved.len() {
        return Err("manifest capability resolution is inconsistent"
            .to_string()
            .into());
    }
    let mut plan = LaunchPlan::default();
    for (need, caps) in needs.iter().zip(resolved) {
        if matches!(need.scope, ScopeBinding::Wild) {
            if !caps.is_empty() {
                plan.inherit(inherited_wild_caps(need.verb, delegation)?);
            }
            continue;
        }
        for cap in caps {
            plan.require(cap.clone(), delegation);
        }
    }
    Ok(plan)
}

/// Settle a complete plan against the approvals store.
///
/// Every capability the launcher cannot delegate must already be
/// approved for this exact launcher identity, verb and canonical scope,
/// and the whole set is retired together. A launch that is short even
/// one approval consumes nothing and files a deduplicated pending
/// request for each missing capability, returning their ids so the same
/// launcher process can wait for the decisions and retry.
fn authorize_plan(delegation: &Delegation, plan: LaunchPlan) -> Result<CapSet, BrokerError> {
    if plan.missing.is_empty() {
        return Ok(plan.caps);
    }
    match crate::approvals::consume_grant_set_once_for_owner(
        &delegation.grant_session,
        &plan.missing,
        Some(delegation.uid),
    ) {
        Ok(true) => Ok(plan.caps),
        Ok(false) => Err(request_approvals(delegation, &plan.missing)),
        Err(error) => Err(format!("could not settle approved permission grants: {error}").into()),
    }
}

/// File (or reuse) one pending request per missing capability and tell
/// the launcher which decisions it is waiting on.
///
/// The returned ids are not authority: a sibling that reads them still
/// cannot make the daemon derive this launcher's identity, and the
/// grants are matched on that identity plus the exact verb and scope.
fn request_approvals(delegation: &Delegation, missing: &[Cap]) -> BrokerError {
    let pending = crate::approvals::list_pending_for_owner(Some(delegation.uid));
    let mut ids = Vec::with_capacity(missing.len());
    let mut failures = Vec::new();
    for cap in missing {
        // A capability the user already approved is settled at retry
        // time; asking again would queue a second prompt for the same
        // decision.
        match crate::approvals::has_approved_grant_for_owner(
            &delegation.grant_session,
            cap,
            Some(delegation.uid),
        ) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                failures.push(error);
                continue;
            }
        }
        let existing = pending.iter().find(|request| {
            request.session == delegation.grant_session
                && request.verb == cap.verb.as_str()
                && request.scope == cap.scope
        });
        let submitted = match existing {
            Some(request) => Ok(request.id.clone()),
            None => crate::approvals::submit_owned(
                cap.verb,
                cap.scope.clone(),
                delegation.grant_session.clone(),
                format!("App launch requires {}:{}", cap.verb.as_str(), cap.scope),
                Some(delegation.requester.clone()),
                Some(delegation.uid),
            ),
        };
        match submitted {
            Ok(id) => ids.push(id),
            Err(error) => failures.push(error),
        }
    }

    let summary = missing
        .iter()
        .map(|cap| format!("{}:{}", cap.verb.as_str(), cap.scope))
        .collect::<Vec<_>>()
        .join(", ");
    if !failures.is_empty() {
        return format!(
            "launcher cannot delegate {summary}; could not create an approval request: {}",
            failures.join("; ")
        )
        .into();
    }
    BrokerError::authorization_required(
        format!("launcher cannot delegate {summary}; awaiting approval"),
        json!({
            "status": "approval_required",
            "approval_requests": ids,
        }),
    )
    .classified("approval_required")
}

/// Resolve a manifest wildcard need against the launcher's own scopes.
///
/// A `wild` need is a request to act on "everything of this kind", and
/// the only safe reading is "everything the launcher itself may reach".
/// Inheriting an untyped [`Scope::Wild`] would hand the App unbounded
/// authority over a real resource namespace, so that combination is
/// refused outright: the launching session has to hold a bounded scope
/// for the verb first. Verbs that carry no resource (`ui.notify`,
/// `time.delay`) and self-referential verbs have no narrower form, so
/// `Scope::Wild` remains their canonical scope.
fn inherited_wild_caps(verb: Verb, delegation: &Delegation) -> Result<Vec<Cap>, BrokerError> {
    let mut inherited = Vec::new();
    for cap in delegation.ceiling.iter().filter(|held| held.verb == verb) {
        if cap.scope.is_wildcard() && verb_addresses_a_resource(verb) {
            return Err(format!(
                "wildcard `{}` need cannot inherit unbounded authority; \
                 the launching session must hold a bounded {} scope",
                verb.as_str(),
                verb.as_str()
            )
            .into());
        }
        inherited.push(cap.clone());
    }
    if inherited.is_empty() {
        return Err(format!(
            "launcher holds no `{}` capability to delegate",
            verb.as_str()
        )
        .into());
    }
    Ok(inherited)
}

fn verb_addresses_a_resource(verb: Verb) -> bool {
    match crate::caps::lookup_meta(verb).map(|meta| meta.scope_kind) {
        Some(ScopeKind::Path) | Some(ScopeKind::Host) | Some(ScopeKind::Name) => true,
        Some(_) => false,
        // Unknown verbs are not in the catalog; fail closed.
        None => true,
    }
}

/// Capabilities for a desktop launch.
///
/// The launcher only gets to name the entry the manifest itself
/// declares — anything else is not a GUI launch and must go through the
/// operation path, where arguments are bound and authorized. A GUI
/// launch carries no operation arguments, so only manifest-fixed and
/// wildcard needs can be bound; argument-bound needs are left out.
fn gui_plan(app: &App, exec: &str, delegation: &Delegation) -> Result<LaunchPlan, BrokerError> {
    let desktop = app
        .manifest
        .desktop
        .as_ref()
        .ok_or_else(|| format!("App `{}` declares no desktop surface", app.manifest.id))?;
    if desktop.exec != exec {
        return Err(format!(
            "App `{}` declares desktop entrypoint `{}`, not `{exec}`",
            app.manifest.id, desktop.exec
        )
        .into());
    }

    let mut plan = LaunchPlan::default();
    for need in app
        .manifest
        .operations
        .values()
        .flat_map(|operation| operation.needs.iter())
    {
        match &need.scope {
            // A GUI launch is not a user-confirmed invocation of any one
            // operation, so anything the launcher cannot already delegate
            // is dropped rather than turned into an approval prompt.
            ScopeBinding::Fixed { scope } => {
                let requested = Cap::new(need.verb, scope.clone());
                if delegation.ceiling.covers(&requested) {
                    plan.inherit([requested]);
                }
            }
            ScopeBinding::Wild => {
                plan.inherit(inherited_wild_caps(need.verb, delegation)?);
            }
            ScopeBinding::FromArg { .. }
            | ScopeBinding::FromArgMap { .. }
            | ScopeBinding::FromArgOrWild { .. } => {}
        }
    }
    Ok(plan)
}

fn native_manifest_caps(manifest: &Manifest) -> Result<CapSet, String> {
    let mut caps = CapSet::new();
    for operation in manifest.operations.values() {
        for need in &operation.needs {
            let scope = match &need.scope {
                ScopeBinding::Fixed { scope } => scope.clone(),
                ScopeBinding::Wild => Scope::Wild,
                ScopeBinding::FromArg { .. }
                | ScopeBinding::FromArgMap { .. }
                | ScopeBinding::FromArgOrWild { .. } => {
                    return Err(format!(
                        "native host App `{}` has argument-bound capability {}",
                        manifest.id,
                        need.verb.as_str()
                    ));
                }
            };
            caps.insert(Cap::new(need.verb, scope));
        }
    }
    Ok(caps)
}

async fn install_session(
    uid: u32,
    home: std::path::PathBuf,
    info: SessionInfo,
) -> Result<std::path::PathBuf, String> {
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::register_session(info)?;
        Ok::<_, String>(crate::paths::proc_data_dir())
    })
    .await
}

// ---------------------------------------------------------------------------
// Launch grants
// ---------------------------------------------------------------------------

/// Mint the launch grant for a freshly registered session.
///
/// The grant is the authority; the handle returned here is only a
/// reference to it. It is bound to the exact launcher process — uid,
/// pid, start time and cgroup — so a same-uid sibling that obtained the
/// characters cannot bind, re-scope or drop the session, and a pid
/// recycled after the launcher exits resolves to nothing.
fn issue_launch_grant(
    session_id: &str,
    app_id: Option<&str>,
    uid: u32,
    launcher: &LauncherAuthority,
    caps: &CapSet,
) -> Result<String, String> {
    let principal = authority::Principal::of_process(uid, launcher.pid)
        .ok_or_else(|| "cannot bind an App launch to an unverifiable process".to_string())?;
    if principal.start_time_ticks != launcher.start_time_ticks {
        return Err("App launcher process identity changed during registration".to_string());
    }
    let (handle, view) = authority::authority()
        .issue(authority::Issuance {
            issuer: authority::Issuer::AppSessionAuthority,
            principal,
            binding: authority::Binding::Process,
            subject: authority::Subject::session(session_id)
                .with_app(app_id.map(ToOwned::to_owned)),
            // The launch grant is the parent of the session grant, so
            // it has to carry every audience that session will need;
            // `bind` narrows it to the provider audiences and drops
            // launch authority.
            audience: authority::AudienceSet::of(&[
                authority::Audience::AppLaunch,
                authority::Audience::SystemService,
                authority::Audience::Credential,
            ]),
            caps: caps.clone(),
            lifetime: LAUNCH_GRANT_TTL,
            uses: authority::Uses::Unbounded,
            index_session: false,
        })
        .map_err(|error| error.to_string())?;
    authority::audit::record_issued(&view, None);
    Ok(handle.into_wire())
}

/// Derive the session grant a bound App runs under.
///
/// Strictly narrower than the launch grant it comes from: launch
/// authority is dropped, the binding moves from the launcher to the App
/// process tree, and the expiry can only move earlier. It claims the
/// session index, and the store refuses a second claim, so `bind` is
/// one-shot even under two concurrent callers.
fn issue_session_grant(
    launch_handle: &str,
    session_id: &str,
    app_id: Option<&str>,
    uid: u32,
    child_pid: u32,
    caps: &CapSet,
) -> Result<(), String> {
    let principal = authority::Principal::of_process(uid, child_pid)
        .ok_or_else(|| format!("App process {child_pid} could not be identified"))?;
    let (_handle, view) = authority::authority()
        .attenuate(
            launch_handle,
            authority::Attenuation {
                issuer: authority::Issuer::AppSessionAuthority,
                principal,
                binding: authority::Binding::ProcessTree,
                subject: authority::Subject::session(session_id)
                    .with_app(app_id.map(ToOwned::to_owned)),
                audience: authority::AudienceSet::of(&[
                    authority::Audience::SystemService,
                    authority::Audience::Credential,
                ]),
                caps: caps.clone(),
                lifetime: SESSION_GRANT_TTL,
                uses: authority::Uses::Unbounded,
                index_session: true,
            },
        )
        .map_err(|error| error.to_string())?;
    authority::audit::record_issued(&view, None);
    Ok(())
}

/// Mint one root-authorized target grant for an MCP-first App call.
///
/// Caller invoke authority is checked separately and never appears here.
/// Target capabilities may come from an exact owner approval rather than the
/// caller's standing ceiling, so deriving this grant from the launch parent
/// would incorrectly reject the approved target authority.
fn issue_gateway_target_grant(
    session_id: &str,
    app_id: &str,
    uid: u32,
    child_pid: u32,
    caps: &CapSet,
    deadline_unix_ms: u64,
) -> Result<(), String> {
    authority::revoke_indexed_session(session_id);
    let remaining = deadline_unix_ms
        .checked_sub(crate::agentd::grant::now_ms())
        .filter(|remaining| *remaining > 0)
        .ok_or_else(|| "MCP App target grant deadline has expired".to_string())?;
    let principal = authority::Principal::of_process(uid, child_pid)
        .ok_or_else(|| format!("App process {child_pid} could not be identified"))?;
    let (_handle, view) = authority::authority()
        .issue(authority::Issuance {
            issuer: authority::Issuer::AppGateway,
            principal,
            binding: authority::Binding::ProcessTree,
            subject: authority::Subject::session(session_id).with_app(Some(app_id.to_string())),
            audience: authority::AudienceSet::of(&[
                authority::Audience::SystemService,
                authority::Audience::Credential,
            ]),
            caps: caps.clone(),
            lifetime: TARGET_CALL_GRANT_TTL.min(Duration::from_millis(remaining)),
            uses: authority::Uses::Unbounded,
            index_session: true,
        })
        .map_err(|error| error.to_string())?;
    authority::audit::record_issued(&view, None);
    Ok(())
}

/// Re-derive the session grant after a transient capability change.
///
/// An MCP session tool call widens what the App may do for exactly one
/// call, so the old session grant is revoked and a new one derived from
/// the same launch grant with the new set. Deriving rather than editing
/// keeps the attenuation check on the path: the transient set still has
/// to sit inside what the launcher could delegate.
///
/// `child_pid` is passed in rather than looked up, because the only
/// correct place to read the routed registry is under the owner's own
/// path view, and the caller already holds that row.
fn reissue_session_grant(
    launch_handle: &str,
    session_id: &str,
    app_id: Option<&str>,
    uid: u32,
    child_pid: u32,
    caps: &CapSet,
) -> Result<(), String> {
    authority::authority().revoke_indexed_session(session_id);
    issue_session_grant(launch_handle, session_id, app_id, uid, child_pid, caps)
}

// ---------------------------------------------------------------------------
// Request parsing and process inspection
// ---------------------------------------------------------------------------

fn launch_kind(params: &Value) -> Result<LaunchKind, String> {
    match params.get("kind").and_then(Value::as_str) {
        Some("operation") => Ok(LaunchKind::Operation),
        Some("gui") => Ok(LaunchKind::Gui),
        Some("mcp") => Ok(LaunchKind::Mcp),
        Some(other) => Err(format!("unknown App launch kind `{other}`")),
        None => Err("kind is required".to_string()),
    }
}

fn string_array(params: &Value, key: &str) -> Result<Vec<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| format!("{key} must contain only strings"))
            })
            .collect(),
        Some(_) => Err(format!("{key} must be an array")),
    }
}

fn require_trusted_native_launcher(client: &ClientIdentity) -> Result<(), String> {
    let pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    let launcher = process_executable(pid)
        .ok_or_else(|| "native App launcher executable is unavailable".to_string())?;
    if launcher != std::path::Path::new("/usr/lib/cos/claw-mail-ai-host")
        || !root_owned_file(&launcher)
    {
        return Err(format!(
            "native App launcher must be the root-owned mail-ai host, got `{}`",
            launcher.display()
        ));
    }
    let parent = process_parent_pid(pid)
        .and_then(process_executable)
        .ok_or_else(|| "native App launcher parent is unavailable".to_string())?;
    let trusted_parent = (parent == std::path::Path::new("/usr/bin/thunderbird")
        || parent.starts_with("/usr/lib/thunderbird")
        || parent.starts_with("/usr/lib/thunderbird-esr"))
        && root_owned_file(&parent);
    if !trusted_parent {
        return Err(format!(
            "native App launcher parent must be root-owned Thunderbird, got `{}`",
            parent.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_executable(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

#[cfg(not(target_os = "linux"))]
fn process_executable(_pid: u32) -> Option<std::path::PathBuf> {
    None
}

#[cfg(unix)]
fn root_owned_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    std::fs::metadata(path)
        .map(|metadata| {
            metadata.is_file() && metadata.uid() == 0 && metadata.permissions().mode() & 0o022 == 0
        })
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn root_owned_file(_path: &std::path::Path) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn process_parent_pid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix("PPid:")
            .and_then(|value| value.trim().parse::<u32>().ok())
    })
}

#[cfg(not(target_os = "linux"))]
fn process_parent_pid(_pid: u32) -> Option<u32> {
    None
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{key} is required"))
}

fn required_u32(params: &Value, key: &str) -> Result<u32, String> {
    params
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("{key} is required"))
}

#[cfg(target_os = "linux")]
fn process_no_new_privs(pid: u32) -> Option<bool> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("NoNewPrivs:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
            })
        })
        .map(|value| value != 0)
}

#[cfg(not(target_os = "linux"))]
fn process_no_new_privs(_pid: u32) -> Option<bool> {
    None
}

#[cfg(target_os = "linux")]
fn is_descendant_of(mut child: u32, ancestor: u32) -> bool {
    for _ in 0..64 {
        if child == ancestor {
            return true;
        }
        if child <= 1 {
            return false;
        }
        let Some(parent) = process_parent_pid(child) else {
            return false;
        };
        child = parent;
    }
    false
}

#[cfg(not(target_os = "linux"))]
fn is_descendant_of(_child: u32, _ancestor: u32) -> bool {
    false
}

#[cfg(target_os = "linux")]
fn process_uid(pid: u32) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("Uid:")
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse().ok())
        })
}

#[cfg(not(target_os = "linux"))]
fn process_uid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_sessions.rs"
    ));
}
