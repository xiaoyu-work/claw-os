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
use crate::provenance::Ceiling;

use super::authority;
use super::client_identity::ClientIdentity;
use super::routes::{Access, Command, Route, RouteCall};
use super::state::DaemonState;

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

fn required_package_ref(params: &Value) -> Result<crate::provenance::runtime::PackageRef, String> {
    let value = params
        .get("package")
        .cloned()
        .ok_or_else(|| "App session omitted its verified package".to_string())?;
    serde_json::from_value(value).map_err(|error| format!("invalid App package identity: {error}"))
}

fn require_expected_package(
    app: &App,
    expected: &crate::provenance::runtime::PackageRef,
) -> Result<Arc<crate::provenance::VerifiedPackage>, String> {
    let package = Arc::clone(app.require_verified()?);
    if crate::provenance::runtime::PackageRef::of(&package) != *expected {
        return Err(format!(
            "App `{}` package changed before session authorization",
            app.manifest.id
        ));
    }
    Ok(package)
}

fn target_session_caps(authorized: CapSet, caller_invoke: &Cap) -> CapSet {
    CapSet::from_caps(
        authorized
            .iter()
            .filter(|cap| *cap != caller_invoke)
            .cloned(),
    )
}

async fn remove_session_row(owner: u32, home: std::path::PathBuf, session_id: &str) {
    let remove_id = session_id.to_string();
    crate::paths::with_user_override(owner, home, async move {
        crate::proc::deregister_session(&remove_id);
    })
    .await;
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
    "app_session.relay",
];

pub async fn register(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let app_id = required_string(&params, "app_id")?;
    let expected_package = required_package_ref(&params)?;
    let kind = launch_kind(&params)?;
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    let delegation = Delegation::new(&launcher, uid, &home, &params)?;
    let app = installed_app(&app_id)?;
    let package = require_expected_package(&app, &expected_package)?;
    let package_ref = crate::provenance::runtime::PackageRef::of(&package);
    // Resolved before any plan is built: the ceiling decides whether
    // this launch kind is available at all, and every capability and
    // audience below is filtered through it.
    let ceiling = app_ceiling(&app)?;
    if matches!(kind, LaunchKind::Mcp) && !ceiling.allows_mcp_attach() {
        return Err(format!(
            "App `{app_id}` is {}-trusted and may not run as an MCP server; \
             sign and install it to attach a session",
            ceiling.label()
        )
        .into());
    }

    let (command, mut plan, caller_invoke) = match kind {
        LaunchKind::Operation => {
            let operation = required_string(&params, "operation")?;
            let args = string_array(&params, "args")?;
            let plan = operation_plan(&app, &operation, &args, &delegation, &ceiling)?;
            (
                format!("cos app {app_id} {operation}"),
                plan,
                Cap::new(Verb::AGENT_INVOKE, Scope::name(&app_id)),
            )
        }
        LaunchKind::Gui => {
            let exec = required_string(&params, "operation")?;
            let plan = gui_plan(&app, &exec, &delegation, &ceiling)?;
            (
                format!("cos app {app_id} {exec}"),
                plan,
                Cap::new(Verb::AGENT_INVOKE, Scope::name(&app_id)),
            )
        }
        LaunchKind::Mcp => {
            let tool = required_string(&params, "tool")?;
            app.manifest
                .mcp
                .as_ref()
                .and_then(|service| service.tools.iter().find(|declared| declared.name == tool))
                .ok_or_else(|| format!("App `{app_id}` has no MCP tool `{tool}`"))?;
            let invoke = crate::agent::tools::app_gateway::invoke_cap(&app_id, &tool)?;
            (
                format!("cos app {app_id} mcp {tool}"),
                LaunchPlan::default(),
                invoke,
            )
        }
    };
    plan.require(caller_invoke.clone(), &delegation);
    let grant_caps = authorize_plan(&delegation, plan, &ceiling, &app_id)?;
    let caps = target_session_caps(grant_caps.clone(), &caller_invoke);

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
        group: Some(
            if kind == LaunchKind::Mcp {
                "app-mcp"
            } else {
                "app"
            }
            .to_string(),
        ),
        parent: launcher.parent.clone(),
        workdir: Some(app.dir.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: Some(worker_floor(launcher.tier)),
        scope: launcher.scope.clone(),
        priority: launcher.priority.clone(),
        caps: Some(caps.clone()),
        transient_caps: None,
        role: launcher.role.clone(),
        app_id: Some(app_id.clone()),
        pending_bind: true,
        start_time_ticks: None,
        client: crate::session::SessionClient::new(crate::session::SessionSource::App, false, true),
    };

    let proc_dir = install_session(uid, home.clone(), info).await?;
    let handle = match issue_launch_grant(
        &session_id,
        Some(&app_id),
        uid,
        &launcher,
        &grant_caps,
        Some(&ceiling),
    ) {
        Ok(handle) => handle,
        Err(error) => {
            remove_session_row(uid, home, &session_id).await;
            return Err(BrokerError::execution(error));
        }
    };
    crate::provenance::runtime::register(uid, &session_id, &package);
    Ok(json!({
        "session_id": session_id,
        "proc_data_dir": proc_dir,
        "app_id": app_id,
        "handle": handle,
        // What clawd actually granted, so the launcher builds its
        // sandbox from the daemon's decision instead of its own
        // resolution of the same manifest. A launcher that finds a
        // wider set than this refuses to launch.
        "caps": caps,
        "package": package_ref,
        "trust_tier": ceiling.label(),
    }))
}

pub async fn register_native(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let app_id = required_string(&params, "app_id")?;
    require_trusted_native_launcher(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    let app = installed_app(&app_id)?;
    let package = Arc::clone(app.require_verified()?);
    let package_ref = crate::provenance::runtime::PackageRef::of(&package);
    // The native host is not an unauthenticated local launcher: its
    // executable and parent are pinned root-owned binaries, so its
    // authority is the installed manifest itself. `native_manifest_caps`
    // still refuses argument-bound needs, which have no invocation to
    // bind to here.
    let ceiling = app_ceiling(&app)?;
    let caps = native_manifest_caps(&app.manifest)?;
    let caps = clamp_to_ceiling(&ceiling, &app_id, &caps, "register_native");
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
    let proc_dir = install_session(uid, home.clone(), info).await?;
    let handle = match issue_launch_grant(
        &session_id,
        Some(&app_id),
        uid,
        &launcher,
        &caps,
        Some(&ceiling),
    ) {
        Ok(handle) => handle,
        Err(error) => {
            remove_session_row(uid, home, &session_id).await;
            return Err(error);
        }
    };
    crate::provenance::runtime::register(uid, &session_id, &package);
    Ok(json!({
        "session_id": session_id,
        "proc_data_dir": proc_dir,
        "app_id": app_id,
        "handle": handle,
        "caps": caps,
        "package": package_ref,
        "trust_tier": ceiling.label(),
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
    let handle = issue_launch_grant(&session_id, None, uid, &launcher, &caps, None)?;
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
    let execution_uid = client.execution_uid.unwrap_or(uid);
    if process_uid(child_pid) != Some(execution_uid) {
        return Err(format!(
            "App process {child_pid} is not owned by execution uid {execution_uid}"
        ));
    }
    // Re-resolved from the installed package rather than remembered
    // from `register`, so a revocation landing between the two is seen
    // here and the session grant is never derived from a stale tier.
    let ceiling = match launch.subject.app_id.as_deref() {
        Some(app_id) => Some(installed_app_for_session(uid, &session_id, app_id)?.1),
        None => None,
    };
    let bind_id = session_id.clone();
    let bound_caps = crate::paths::with_user_override(uid, home, async move {
        crate::proc::bind_session_process(&bind_id, child_pid)?;
        crate::proc::session_info_by_id(&bind_id)
            .and_then(|session| session.caps)
            .ok_or_else(|| "App session lost its capabilities during bind".to_string())
    })
    .await?;
    // The registry row was written by `register` under this same
    // ceiling, but it is re-clamped before it becomes live authority:
    // the row is a file, and a grant derived from a file nobody
    // re-checked is a grant derived from whatever last wrote it.
    let bound_caps = match ceiling.as_ref() {
        Some(ceiling) => clamp_to_ceiling(
            ceiling,
            launch.subject.app_id.as_deref().unwrap_or_default(),
            &bound_caps,
            "bind",
        ),
        None => bound_caps,
    };
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
        ceiling.as_ref(),
    )?;
    if launch.subject.app_id.is_some() {
        crate::provenance::runtime::bind_process(uid, &session_id, child_pid);
    }
    // The worker runs inside a mount and pid namespace and cannot
    // present its own session grant: the only process outside it that
    // legitimately speaks for the session is the launcher that built
    // the sandbox. It gets a relay grant — no capabilities of its own,
    // bound `Process`-tight to this launcher, naming this one session —
    // and the worker never sees it.
    //
    // Developer-trusted content is refused one outright rather than
    // handed an empty one: an unminted handle leaves the launcher's
    // relay slot empty, and `worker::broker::relay` fails closed on an
    // empty slot, so no privileged route is even addressable.
    let relay_handle = match ceiling.as_ref() {
        Some(ceiling) if !ceiling.allows_relay() => {
            crate::provenance::audit(
                "provenance.relay_refused",
                json!({
                    "package_kind": "app",
                    "package_id": launch.subject.app_id.as_deref().unwrap_or_default(),
                    "trust_tier": ceiling.label(),
                    "session_id": session_id,
                }),
            );
            Value::Null
        }
        _ => Value::String(issue_relay_grant(
            &handle,
            &session_id,
            launch.subject.app_id.as_deref(),
            uid,
            launcher_pid,
        )?),
    };
    Ok(json!({"bound": true, "relay_handle": relay_handle}))
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

pub(crate) async fn prepare_app_service_call(
    params: Value,
    client: &ClientIdentity,
) -> Result<super::app_services::PreparedAppServiceCall, BrokerError> {
    let extension = client
        .extension_host
        .as_ref()
        .ok_or_else(|| BrokerError::authorization("App service calls require a private task host"))?
        .clone();
    if extension.purpose != crate::extension_host::protocol::HostPurpose::Task
        || extension.owner_uid != client.require_uid()?
        || client.execution_uid != Some(extension.extension_uid)
        || client.pid != Some(extension.host_pid)
        || client.start_time_ticks != extension.host_start_time_ticks
        || crate::proc::read_start_time_ticks_pub(extension.host_pid)
            != extension.host_start_time_ticks
    {
        return Err(BrokerError::authorization(
            "App service call did not come from its live task host",
        ));
    }

    let app_id = required_string(&params, "app_id")?;
    let tool = required_string(&params, "tool")?;
    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| "App service call arguments must be an object".to_string())?;
    let audit: crate::extension_host::protocol::AppInvocationAudit = params
        .get("audit")
        .cloned()
        .ok_or_else(|| "App service call omitted its audit binding".to_string())
        .and_then(|value| {
            serde_json::from_value(value)
                .map_err(|error| format!("invalid App service audit binding: {error}"))
        })?;
    audit.validate_shape()?;
    if audit.app_id != app_id
        || audit.tool != tool
        || audit.capability_generation != extension.capability_generation
        || audit.context.task_id.as_deref() != Some(extension.lease_id.as_str())
        || audit.context.session_id.as_deref() != extension.authority_session_id.as_deref()
    {
        return Err(BrokerError::authorization(
            "App service audit does not match its authenticated task",
        ));
    }

    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    if launcher.parent.as_deref() != extension.host_session_id.as_deref() {
        return Err(BrokerError::authorization(
            "App service call did not come from the authenticated task host session",
        ));
    }
    let delegation = Delegation::new(&launcher, uid, &home, &params)?;
    validate_mcp_call_context(&audit.context, &launcher, &delegation, uid, &home).await?;

    let app = installed_app(&app_id)?;
    let generation = crate::agent::tools::exposure::capability_generation(&delegation.ceiling);
    if generation != audit.capability_generation {
        return Err(BrokerError::authorization(
            "MCP App caller capabilities changed before daemon authorization",
        ));
    }
    let supplied: BTreeMap<String, Value> = arguments
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    finalize_prepared_app_service_call(
        &app,
        &app_id,
        &tool,
        &supplied,
        &delegation,
        audit.context,
        uid,
    )
}

/// Derive a fully authorized [`PreparedAppServiceCall`] from an
/// already-authenticated invocation.
///
/// This is the shared tail of both the private-task-host and the local
/// CLI paths: given the verified App, the exact tool and arguments, the
/// authenticated delegation, and a daemon-derived call context, it
/// resolves the manifest tool, derives and ceiling-clamps the target
/// capabilities, requires (and separates out) the caller's exact
/// `agent.invoke:<app>/<tool>` authority, and packages the result. The
/// App only ever receives the target capabilities; the invoke authority
/// stays with the caller and never reaches App code.
fn finalize_prepared_app_service_call(
    app: &App,
    app_id: &str,
    tool: &str,
    supplied: &BTreeMap<String, Value>,
    delegation: &Delegation,
    context: crate::agent::tools::app_gateway::McpCallContext,
    uid: u32,
) -> Result<super::app_services::PreparedAppServiceCall, BrokerError> {
    crate::agent::tools::app_gateway::authorize_manifest(&app.manifest, &context.caller)?;
    let generation = crate::agent::tools::exposure::capability_generation(&delegation.ceiling);
    let service = app
        .manifest
        .mcp
        .as_ref()
        .ok_or_else(|| format!("App `{app_id}` does not expose an MCP service"))?;
    let declared_tool = service
        .tools
        .iter()
        .find(|candidate| candidate.name == tool)
        .ok_or_else(|| format!("App `{app_id}` has no mcp tool `{tool}`"))?;
    let effective = app
        .manifest
        .resolve_mcp_tool_call(tool, supplied, &delegation.paths)
        .map_err(|error| format!("resolve `{tool}` capabilities: {error}"))?;
    let placement_caps = effective
        .needs
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let placement = crate::agent::tools::cos_apps_session::classify_call(&placement_caps);
    if let crate::agent::tools::cos_apps_session::CallPlacement::Unsupported(reason) = &placement {
        return Err(BrokerError::authorization(format!(
            "App `{app_id}` tool `{tool}` cannot be authorized: {reason}"
        )));
    }
    let authorized_mounts =
        crate::worker::derive::authorize_granted_path_mounts(&CapSet::from_caps(placement_caps))
            .map_err(BrokerError::authorization)?;
    let ceiling = app_ceiling(app)?;
    let mut plan = derive_plan(
        &declared_tool.needs,
        &effective.needs,
        delegation,
        &ceiling,
        app_id,
    )?;
    let invoke = crate::agent::tools::app_gateway::invoke_cap(app_id, tool)?;
    plan.require(invoke.clone(), delegation);
    let authorized = authorize_plan(delegation, plan, &ceiling, app_id)?;
    let package = crate::provenance::runtime::PackageRef::of(app.require_verified()?);
    let deadline_ms = context
        .deadline_unix_ms
        .ok_or_else(|| "App service call omitted its deadline".to_string())?;
    Ok(super::app_services::PreparedAppServiceCall {
        owner_uid: uid,
        app_id: app_id.to_string(),
        tool: tool.to_string(),
        arguments: Value::Object(effective.values.into_iter().collect()),
        context,
        capability_generation: generation,
        package,
        caps: target_session_caps(authorized, &invoke),
        placement,
        authorized_mounts,
        lifecycle: service.lifecycle,
        deadline_ms,
    })
}

/// The daemon-derived workload id for an authenticated local CLI
/// principal.
///
/// Encodes only kernel-reported facts about the exact peer — its uid,
/// pid and process start time — in a form that satisfies the call
/// context's workload-id grammar. It is never anything the request
/// supplied and is never shared with a sibling launcher.
fn cli_principal_id(uid: u32, pid: u32, start_time_ticks: Option<u64>) -> String {
    match start_time_ticks {
        Some(ticks) => format!("cli.uid{uid}.pid{pid}.start{ticks}"),
        None => format!("cli.uid{uid}.pid{pid}"),
    }
}

/// Authenticated local CLI App invocation preparation.
///
/// The request body carries only the exact App id, tool and arguments.
/// Everything with authority — the caller principal, call context,
/// capability ceiling, target capabilities, verified package, owner uid
/// and deadline — is derived here from the peer's [`ClientIdentity`],
/// its process ancestry / registered launcher session, the verified
/// package and the installed manifest. The caller principal is a fresh
/// [`McpPrincipalKind::Cli`] the daemon mints; a private task host can
/// never reach this route, and this route can never mint a task host
/// principal.
pub(crate) async fn prepare_cli_app_service_call(
    params: Value,
    client: &ClientIdentity,
) -> Result<super::app_services::PreparedAppServiceCall, BrokerError> {
    use crate::agent::tools::app_gateway::{
        McpCallContext, McpPrincipal, McpPrincipalKind, CALL_CONTEXT_WIRE_VERSION,
    };

    // The CLI principal is derived from the peer itself. A daemon-owned
    // Extension Host (task host or App service host) is not a human CLI
    // process and must never drive this route, so a task host can neither
    // reach it nor forge a CLI identity through it.
    if client.extension_host.is_some() {
        return Err(BrokerError::authorization(
            "App service CLI calls do not come from an Extension Host",
        ));
    }

    let app_id = required_string(&params, "app_id")?;
    let tool = required_string(&params, "tool")?;
    let arguments = params
        .get("arguments")
        .and_then(Value::as_object)
        .ok_or_else(|| "App service CLI call arguments must be an object".to_string())?;

    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    // Identity, ceiling and path context come from the authenticated peer
    // and its process ancestry / registered launcher session. `Value::Null`
    // is passed as the params for ceiling derivation so no caller-supplied
    // `parent_caps` (or any other field) can influence the launcher ceiling.
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    let delegation = Delegation::new(&launcher, uid, &home, &Value::Null)?;

    let now = crate::agentd::grant::now_ms();
    let deadline_ms = now.saturating_add(crate::extension_host::protocol::MAX_REQUEST_TIMEOUT_MS);
    let call_id = format!("cli-{}", uuid::Uuid::new_v4().simple());
    let context = McpCallContext {
        wire_version: CALL_CONTEXT_WIRE_VERSION,
        trace_id: call_id.clone(),
        call_id,
        parent_call_id: None,
        depth: 0,
        deadline_unix_ms: Some(deadline_ms),
        session_id: None,
        task_id: None,
        caller: McpPrincipal {
            kind: McpPrincipalKind::Cli,
            id: cli_principal_id(uid, launcher.pid, launcher.start_time_ticks),
            owner_uid: uid,
            app_id: None,
        },
    };
    context.validate().map_err(BrokerError::authorization)?;

    let app = installed_app(&app_id)?;
    let supplied: BTreeMap<String, Value> = arguments
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    finalize_prepared_app_service_call(&app, &app_id, &tool, &supplied, &delegation, context, uid)
}

pub async fn set_transient(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, String> {
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
    if bound.group.as_deref() != Some("app-mcp") {
        return Err("only MCP App sessions accept tool-call authority".to_string());
    }
    let child_pid = bound.pid;

    let authorization = match params.get("authorization") {
        None | Some(Value::Null) => None,
        Some(Value::String(token)) => Some(token.as_str()),
        Some(_) => return Err("App session authorization must be a token".to_string()),
    };
    let action_digest = match params.get("action_digest") {
        None | Some(Value::Null) => None,
        Some(Value::String(digest))
            if digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) =>
        {
            Some(digest.as_str())
        }
        Some(_) => return Err("App session action digest is invalid".to_string()),
    };
    if authorization.is_some() != action_digest.is_some() {
        return Err(
            "App session authorization and action digest must be supplied together".to_string(),
        );
    }
    let widening = authorization.is_some();

    // The same authoritative ceiling as `register`, resolved again from
    // the installed package. A transient re-scope is the obvious way to
    // widen an App after launch, so it is clamped by the identical
    // rule: no MCP call, approval or parent capability can move a
    // package above its tier once it is running.
    //
    // A package that can no longer be verified — uninstalled, revoked,
    // replaced — may not widen anything. Clearing a transient set is
    // still allowed, because refusing to *narrow* would strand exactly
    // the grant the caller was trying to give up.
    let verified = match installed_app_for_session(uid, &session_id, &app_id) {
        Ok(verified) => Some(verified),
        Err(error) if widening => return Err(error),
        Err(error) => {
            tracing::warn!(
                app = %app_id,
                error = %error,
                "narrowing an App session whose package no longer verifies"
            );
            None
        }
    };

    // Derive and authorize the requested capabilities *before* anything
    // is written. A launch that cannot settle its approvals leaves both
    // the registry and the authority untouched.
    let (caps, call_context) = match authorization {
        None => (None, None),
        Some(token) => {
            let supplied_action_digest =
                action_digest.ok_or_else(|| "App session action digest is missing".to_string())?;
            let (app, _) = verified
                .as_ref()
                .ok_or_else(|| "App session tool call needs a verified package".to_string())?;
            let expected_package =
                crate::provenance::runtime::PackageRef::of(app.require_verified()?);
            let pending = state.consume_app_authorization(token, supplied_action_digest)?;
            let extension = client.extension_host.as_ref().ok_or_else(|| {
                "App call authorization requires a private App service host".to_string()
            })?;
            if extension.purpose != crate::extension_host::protocol::HostPurpose::AppService
                || pending.owner_uid != uid
                || pending.app_id != app_id
                || pending.package != expected_package
                || pending.service_host_session_id
                    != extension.host_session_id.as_deref().unwrap_or_default()
                || pending.service_host_pid != extension.host_pid
                || pending.service_host_start_time_ticks != extension.host_start_time_ticks
                || pending.service_extension_uid != extension.extension_uid
                || client.pid != Some(extension.host_pid)
                || bound.parent.as_deref() != Some(pending.service_host_session_id.as_str())
                || !crate::proc::process_descends_from(child_pid, extension.host_pid)
            {
                return Err(
                    "App call authorization does not match this service host and child session"
                        .to_string(),
                );
            }
            pending.context.validate()?;
            if pending.capability_generation.is_empty()
                || pending.tool.is_empty()
                || pending.expires_at_ms > pending.context.deadline_unix_ms.unwrap_or_default()
            {
                return Err("App call authorization is internally inconsistent".to_string());
            }
            (Some(pending.caps), Some(pending.context))
        }
    };

    let mut effective = bound.caps.clone().unwrap_or_else(CapSet::new);
    if let Some(transient) = caps.as_ref() {
        effective.extend(transient.iter().cloned());
    }
    // Clamped once more over the union: the persisted row and the
    // transient set are each within the ceiling, but the merge is what
    // becomes the live grant, and that is the value that has to be
    // provably inside it.
    let ceiling = verified.as_ref().map(|(_, ceiling)| ceiling);
    let effective = match ceiling {
        Some(ceiling) => clamp_to_ceiling(ceiling, &app_id, &effective, "set_transient"),
        None => effective,
    };

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

    let grant = match call_context.as_ref() {
        Some(context) => issue_gateway_target_grant(
            &session_id,
            &app_id,
            uid,
            child_pid,
            &effective,
            context,
            ceiling,
        ),
        None => reissue_session_grant(
            &handle,
            &session_id,
            Some(&app_id),
            uid,
            child_pid,
            &effective,
            ceiling,
        ),
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
    authority::revoke_session(session_id);
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
    // A per-call App Gateway grant is daemon-rooted rather than derived
    // from the launch grant, so revoke the indexed session authority
    // explicitly before retiring the launch lineage.
    authority::revoke_indexed_session(&session_id);
    authority::authority().revoke(launch.id);
    crate::provenance::runtime::deregister(uid, &session_id);
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
            &authority::Presentation::new(
                uid,
                pid,
                client.start_time_ticks,
                authority::Audience::AppLaunch,
                "app_session",
            ),
        )
        .map_err(|error| error.to_string())?;
    if view.subject.session_id.as_deref() != Some(session_id) {
        return Err("App launch handle does not cover this session".to_string());
    }
    Ok(view)
}

async fn validate_mcp_call_context(
    context: &crate::agent::tools::app_gateway::McpCallContext,
    launcher: &LauncherAuthority,
    delegation: &Delegation,
    uid: u32,
    home: &std::path::Path,
) -> Result<(), String> {
    use crate::agent::tools::app_gateway::McpPrincipalKind;

    context.validate()?;
    let caller_session = context
        .session_id
        .as_deref()
        .ok_or_else(|| "MCP App call omitted its caller session".to_string())?;
    if context.caller.owner_uid != uid
        || context.caller.id != caller_session
        || delegation.uid != uid
    {
        return Err("MCP App call context does not match its owner session".to_string());
    }
    let now = crate::agentd::grant::now_ms();
    let deadline = context
        .deadline_unix_ms
        .filter(|deadline| {
            *deadline > now
                && *deadline
                    <= now.saturating_add(crate::extension_host::protocol::MAX_REQUEST_TIMEOUT_MS)
        })
        .ok_or_else(|| "MCP App call deadline is outside the allowed range".to_string())?;
    let _ = deadline;

    let transport_session_id = launcher
        .parent
        .as_deref()
        .ok_or_else(|| "MCP App calls require a registered launcher session".to_string())?;
    let transport_id = transport_session_id.to_string();
    let transport = crate::paths::with_user_override(uid, home.to_path_buf(), async move {
        crate::proc::session_info_by_id(&transport_id)
    })
    .await
    .ok_or_else(|| "MCP App launcher session no longer exists".to_string())?;
    if transport.pid != launcher.pid
        || transport.start_time_ticks != launcher.start_time_ticks
        || transport.pending_bind
    {
        return Err("MCP App call did not come from the registered launcher process".to_string());
    }

    let caller = if transport.session_id == caller_session {
        transport
    } else {
        if transport.group.as_deref() != Some(crate::extension_host::protocol::EXTENSION_HOST_GROUP)
            || transport.parent.as_deref() != Some(caller_session)
            || transport.command.first().map(String::as_str) != Some("claw-extension-host")
            || context.task_id.as_deref() != transport.command.get(1).map(String::as_str)
        {
            return Err(
                "MCP App call context does not match the authenticated Extension Host".to_string(),
            );
        }
        let caller_id = caller_session.to_string();
        crate::paths::with_user_override(uid, home.to_path_buf(), async move {
            crate::proc::session_info_by_id(&caller_id)
        })
        .await
        .ok_or_else(|| "MCP App caller session no longer exists".to_string())?
    };

    let principal_matches = match context.caller.kind {
        McpPrincipalKind::SystemAgent => {
            context.caller.app_id.is_none()
                && caller.app_id.is_none()
                && matches!(
                    caller.client.source,
                    crate::session::SessionSource::LocalCli
                        | crate::session::SessionSource::LocalWeb
                        | crate::session::SessionSource::BrokerTask
                        | crate::session::SessionSource::ScheduledTrigger
                        | crate::session::SessionSource::System
                )
        }
        McpPrincipalKind::AppAgent => {
            context.caller.app_id.as_deref() == caller.app_id.as_deref() && caller.app_id.is_some()
        }
        McpPrincipalKind::App => {
            context.caller.app_id.as_deref() == caller.app_id.as_deref()
                && caller.app_id.is_some()
                && caller.client.source == crate::session::SessionSource::App
        }
        McpPrincipalKind::ExternalAgent => {
            context.caller.app_id.is_none()
                && caller.app_id.is_none()
                && matches!(
                    caller.client.source,
                    crate::session::SessionSource::ExternalMcp
                        | crate::session::SessionSource::DelegatedAgent
                )
        }
        McpPrincipalKind::Cli => false,
    };
    if !principal_matches {
        return Err("MCP App caller principal does not match the registered session".to_string());
    }
    Ok(())
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
    let execution_uid = client.execution_uid.unwrap_or(uid);
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
        matches!(
            session.group.as_deref(),
            Some(
                crate::extension_host::protocol::EXTENSION_HOST_GROUP
                    | crate::extension_host::protocol::APP_SERVICE_HOST_GROUP
            )
        ) && session.app_id.is_none()
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
    // Authority path: a quarantined install may never reach capability
    // derivation or session binding, so the verified lookup is used
    // rather than the listing-oriented one.
    let app = crate::apps::find_verified_fresh(&apps_dir, app_id)?;
    if app.manifest.id != app_id {
        return Err(format!(
            "installed manifest declares id `{}`, not `{app_id}`",
            app.manifest.id
        ));
    }
    let pkg = app.require_verified()?;
    pkg.assert_current(&crate::provenance::trust_store())
        .map_err(|e| format!("App `{app_id}` failed its pre-launch provenance check: {e}"))?;
    Ok(app)
}

fn installed_app_for_session(
    owner: u32,
    session_id: &str,
    app_id: &str,
) -> Result<(App, Ceiling), String> {
    let expected = crate::provenance::runtime::package_for(owner, session_id)?
        .ok_or_else(|| "App session has no bound package identity".to_string())?;
    let app = installed_app(app_id)?;
    let current = crate::provenance::runtime::PackageRef::of(app.require_verified()?);
    if current != expected {
        return Err(format!(
            "App `{app_id}` package changed after session authorization"
        ));
    }
    let ceiling = app_ceiling(&app)?;
    Ok((app, ceiling))
}

/// The authoritative provenance ceiling for an installed App.
///
/// Derived here, inside the daemon, from the daemon's own verified
/// package — never from anything the launcher sent. The launcher
/// applies the same ceiling before it builds a sandbox, but that copy
/// is defence in depth: a launcher is unprivileged local code and its
/// view of the manifest, of the trust tier, or of what it "already
/// dropped" is not evidence. Every grant this module mints is clamped
/// against *this* value.
fn app_ceiling(app: &App) -> Result<Ceiling, String> {
    Ok(app.require_verified()?.ceiling())
}

/// Clamp a resolved capability set to a package's ceiling, auditing
/// what the daemon refused.
///
/// This is the only place an App's capability set becomes authority, so
/// it is also the only honest place to record the restriction: the
/// audit line means "clawd did not grant these", not "a launcher says
/// it did not ask for these".
fn clamp_to_ceiling(ceiling: &Ceiling, app_id: &str, caps: &CapSet, stage: &str) -> CapSet {
    let (kept, dropped) = ceiling.clamp(caps);
    record_ceiling_drop(ceiling, app_id, stage, &dropped);
    kept
}

fn record_ceiling_drop(ceiling: &Ceiling, app_id: &str, stage: &str, dropped: &[Cap]) {
    if dropped.is_empty() {
        return;
    }
    crate::provenance::audit(
        "provenance.ceiling_applied",
        json!({
            "package_kind": "app",
            "package_id": app_id,
            "stage": stage,
            "enforced_by": "clawd",
            "trust_tier": ceiling.label(),
            "dropped": dropped
                .iter()
                .map(|cap| json!({"verb": cap.verb.as_str(), "scope": cap.scope}))
                .collect::<Vec<_>>(),
        }),
    );
}

/// The audiences a grant for this package may carry.
///
/// Structural rather than advisory: the requested set is filtered
/// through the ceiling, so developer-trusted content receives
/// `AppLaunch` at most and can never be handed the `AppRelay`,
/// `SystemService` or `Credential` audiences that address a privileged
/// broker route.
fn permitted_audiences(
    ceiling: Option<&Ceiling>,
    requested: &[authority::Audience],
) -> authority::AudienceSet {
    match ceiling {
        None => authority::AudienceSet::of(requested),
        Some(ceiling) => {
            let allowed: Vec<authority::Audience> = requested
                .iter()
                .copied()
                .filter(|audience| ceiling.allows_audience(audience_facet(*audience)))
                .collect();
            authority::AudienceSet::of(&allowed)
        }
    }
}

/// Map a clawd audience onto the provenance vocabulary.
///
/// The two enums are deliberately separate — `provenance` must not
/// depend on the daemon's authority types — so the crossing is made
/// once, exhaustively, and a new audience will not compile until it is
/// classified.
fn audience_facet(audience: authority::Audience) -> crate::provenance::ceiling::Audience {
    use crate::provenance::ceiling::Audience as Facet;
    match audience {
        authority::Audience::AgentWorker => Facet::AgentWorker,
        authority::Audience::AppLaunch => Facet::AppLaunch,
        authority::Audience::AppRelay => Facet::AppRelay,
        authority::Audience::SystemService => Facet::SystemService,
        authority::Audience::Credential => Facet::Credential,
        authority::Audience::Scheduler => Facet::Scheduler,
        authority::Audience::Permission => Facet::Permission,
        authority::Audience::Transaction => Facet::Transaction,
        authority::Audience::Context => Facet::Context,
        authority::Audience::Notification => Facet::Notification,
        authority::Audience::Task => Facet::Task,
        authority::Audience::Daemon => Facet::Daemon,
    }
}

/// Build the capability plan for one manifest operation.
fn operation_plan(
    app: &App,
    operation: &str,
    args: &[String],
    delegation: &Delegation,
    ceiling: &Ceiling,
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
    derive_plan(
        &declared.needs,
        &effective.needs,
        delegation,
        ceiling,
        &app.manifest.id,
    )
}

fn mcp_tool_plan(
    app: &App,
    call: &Value,
    delegation: &Delegation,
    ceiling: &Ceiling,
) -> Result<LaunchPlan, BrokerError> {
    let app_id = app.manifest.id.as_str();
    let tool_name = required_string(call, "tool")?;
    let args: BTreeMap<String, Value> = match call.get("args") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Some(_) => return Err("mcp tool args must be an object".to_string().into()),
    };
    let tool = app
        .manifest
        .mcp
        .as_ref()
        .and_then(|service| service.tools.iter().find(|tool| tool.name == tool_name))
        .ok_or_else(|| format!("App `{app_id}` has no mcp tool `{tool_name}`"))?;
    let effective = app
        .manifest
        .resolve_mcp_tool_call(&tool_name, &args, &delegation.paths)
        .map_err(|error| format!("resolve `{tool_name}` capabilities: {error}"))?;
    derive_plan(&tool.needs, &effective.needs, delegation, ceiling, app_id)
}

/// Turn manifest needs into a complete capability plan.
///
/// `resolved` is the canonical output of `Manifest::resolve_needs` /
/// `resolve_mcp_tool_needs`, positionally aligned with `needs`, so
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
    ceiling: &Ceiling,
    app_id: &str,
) -> Result<LaunchPlan, BrokerError> {
    if needs.len() != resolved.len() {
        return Err("manifest capability resolution is inconsistent"
            .to_string()
            .into());
    }
    let mut plan = LaunchPlan::default();
    let mut refused_wild = Vec::new();
    for (need, caps) in needs.iter().zip(resolved) {
        if matches!(need.scope, ScopeBinding::Wild) {
            // A `wild` binding is a request to borrow the launcher's
            // reach. Unsigned content does not get to: the need is
            // dropped rather than expanded, so nothing downstream ever
            // sees the inherited scopes.
            if !ceiling.allows_wild_binding(need.verb) {
                refused_wild.push(Cap::new(need.verb, Scope::Wild));
                continue;
            }
            if !caps.is_empty() {
                plan.inherit(inherited_wild_caps(need.verb, delegation)?);
            }
            continue;
        }
        for cap in caps {
            plan.require(cap.clone(), delegation);
        }
    }
    record_ceiling_drop(ceiling, app_id, "wild_binding", &refused_wild);
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
fn authorize_plan(
    delegation: &Delegation,
    plan: LaunchPlan,
    ceiling: &Ceiling,
    app_id: &str,
) -> Result<CapSet, BrokerError> {
    // Clamp before the approvals store is touched. A capability outside
    // the package's ceiling can never be granted, so asking the user to
    // approve it — or worse, *consuming* an approval they granted for
    // some other launch — would be a prompt for nothing. Both halves of
    // the plan are filtered, so `missing` cannot resurrect a dropped
    // capability through consent.
    let (caps, dropped_caps) = ceiling.clamp(&plan.caps);
    let (missing, dropped_missing) = ceiling.clamp_vec(&plan.missing);
    let mut dropped = dropped_caps;
    dropped.extend(dropped_missing);
    record_ceiling_drop(ceiling, app_id, "authorize_plan", &dropped);

    if missing.is_empty() {
        return Ok(caps);
    }
    match crate::approvals::consume_grant_set_once_for_owner(
        &delegation.grant_session,
        &missing,
        Some(delegation.uid),
    ) {
        Ok(true) => Ok(caps),
        Ok(false) => Err(request_approvals(delegation, &missing)),
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
fn gui_plan(
    app: &App,
    exec: &str,
    delegation: &Delegation,
    ceiling: &Ceiling,
) -> Result<LaunchPlan, BrokerError> {
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
    let mut refused_wild = Vec::new();
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
            // Same rule as the operation path: unsigned content never
            // borrows the launcher's reach over a resource namespace,
            // whichever surface asked for it.
            ScopeBinding::Wild if !ceiling.allows_wild_binding(need.verb) => {
                refused_wild.push(Cap::new(need.verb, Scope::Wild));
            }
            ScopeBinding::Wild => {
                plan.inherit(inherited_wild_caps(need.verb, delegation)?);
            }
            ScopeBinding::FromArg { .. }
            | ScopeBinding::FromArgMap { .. }
            | ScopeBinding::FromArgOrWild { .. } => {}
        }
    }
    record_ceiling_drop(ceiling, &app.manifest.id, "wild_binding", &refused_wild);
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
    ceiling: Option<&Ceiling>,
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
            // The launch grant is the parent of the session grant and
            // of the launcher's relay grant, so it has to carry every
            // audience either will need; `bind` narrows the session
            // grant to the provider audiences, the relay grant to
            // relay authority alone, and both drop launch authority.
            //
            // Filtered through the package's ceiling, so a
            // developer-trusted App's launch grant carries `AppLaunch`
            // alone and no attenuation of it can produce a relay,
            // system-service or credential grant — attenuation may only
            // narrow.
            audience: permitted_audiences(
                ceiling,
                &[
                    authority::Audience::AppLaunch,
                    authority::Audience::SystemService,
                    authority::Audience::Credential,
                    authority::Audience::AppRelay,
                ],
            ),
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
    ceiling: Option<&Ceiling>,
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
                audience: permitted_audiences(
                    ceiling,
                    &[
                        authority::Audience::SystemService,
                        authority::Audience::Credential,
                    ],
                ),
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

/// Mint the launcher's right to relay for one App session.
///
/// The grant carries an empty capability set on purpose: it authorizes
/// *presenting* the session grant, never any effect. It is bound
/// `Process`-tight to the launcher, so a same-uid sibling, a process
/// that received the handle over a socket, or the sandboxed worker
/// itself cannot use it; it is derived from the launch grant, so
/// `deregister` revokes it with everything else; and its audience is
/// only [`authority::Audience::AppRelay`], so it can reach nothing
/// directly.
/// Relay one App-session system-service call for a sandboxed worker.
///
/// A worker runs inside a mount and pid namespace with no route to the
/// real broker socket. Its launcher holds the relay grant this route is
/// addressed by, and forwards exactly one inner call at a time.
///
/// The relay is plumbing, not policy. It decides only *which* routes
/// may be relayed at all; every question about whether this session may
/// take this action against this resource is answered by the inner
/// route's own typed decode, its authority decision and the exact
/// capability its provider spends — all against the live session grant,
/// so a transient capability set for one MCP call is honoured while it
/// is set and gone the moment it is cleared.
pub async fn relay(
    state: &DaemonState,
    params: Value,
    client: &ClientIdentity,
    relay_grant: &authority::Decision,
) -> Result<Value, BrokerError> {
    let session_id = required_string(&params, "session_id")?;
    let handle = required_string(&params, "handle")?;
    // The middleware resolved the handle against this process. What is
    // left is the route's own contract: the grant names *this* session.
    if relay_grant.session_id() != Some(session_id.as_str()) {
        return Err(BrokerError::authorization(
            "relay handle does not cover this App session",
        ));
    }
    // Before the grant is resolved, before the body is decoded and
    // before any provider spends a capability: is the package behind
    // this session still trusted?
    //
    // A relay is the one route where a sandboxed worker reaches a
    // privileged provider, and the session grant it rides on lives for
    // `SESSION_GRANT_TTL`. Waiting for that to expire would leave a
    // revoked package driving `system.*` routes for minutes. The
    // package reference comes from the owner's root-owned runtime
    // record, not from the request.
    assert_relay_package_live(client, &session_id).await?;

    let command = required_string(&params, "command")?;
    let inner =
        Command::parse(&command).ok_or_else(|| format!("unknown relay route `{command}`"))?;
    let route = relayable_route(inner)?;

    // The session id is not taken from the worker: it is overwritten
    // with the one the relay grant names, before the inner body is
    // decoded, so the typed decode validates the value that will
    // actually be authorized.
    let mut inner_params = params
        .get("params")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let Some(object) = inner_params.as_object_mut() else {
        return Err(BrokerError::execution(
            "relayed parameters must be an object",
        ));
    };
    object.insert("session".to_string(), Value::String(session_id.clone()));
    let inner_params = (route.decode)(inner_params)
        .map_err(|_| format!("relayed route `{}` refused its parameters", route.name))?;

    let decision = authority::authorize_relayed(
        &handle,
        &session_id,
        route.name,
        &route.authority,
        &inner_params,
        client,
    )
    .await
    .map_err(|_| {
        BrokerError::authorization(format!("relayed route `{}` was not authorized", route.name))
    })?;

    let call = RouteCall {
        state,
        client,
        params: inner_params,
        authority: decision.as_ref(),
    };
    let outcome = (route.handler)(call).await;
    // The same obligation the server enforces for a direct call: a
    // route that derives its own capability and answered without
    // spending one has authorized nothing, so nothing is released.
    if !authority::obligation_met(decision.as_ref()) {
        tracing::error!(
            route = route.name,
            "relayed route answered without exercising its capability requirement"
        );
        return Err(BrokerError::execution(
            "relayed route did not exercise its authority",
        ));
    }
    let result = outcome?;
    Ok(json!({ "command": route.name, "result": result }))
}

/// Re-check the package behind an App session before relaying for it.
///
/// The runtime record is read under the *owner's* path view, which
/// resolves to `/run/cos/caps/<uid>` — root-owned, so the session
/// cannot rewrite its own provenance. The trust store is re-resolved
/// rather than reused: the resolver re-stats the durable generation and
/// rebuilds when it moved, so a revocation written by another process
/// is visible on this very call with no notification or restart.
///
/// A failure is terminal for the session, not just for the call. Both
/// the session grant and every grant derived from the launch are
/// revoked, so the next relay finds nothing to present and the App's
/// own `caps::require` denies too; the instance is marked so the
/// lifecycle pass stops the process itself.
async fn assert_relay_package_live(
    client: &ClientIdentity,
    session_id: &str,
) -> Result<(), String> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let _ = home;
    // The owner uid comes from the peer's kernel credentials, and the
    // record is addressed by it directly — not by whatever
    // `proc_data_dir()` would resolve to inside a path override. A
    // relay grant names an App session, so a missing or unreadable
    // record is a denial rather than "not an extension".
    let verdict = crate::provenance::runtime::assert_live_instance_now(uid, session_id);
    let Err(reason) = verdict else {
        return Ok(());
    };
    authority::authority().revoke_session(session_id);
    super::authority::audit::record_revoked("app-session-revoked-package", Some(session_id), 1);
    crate::provenance::audit(
        "provenance.revoked_instance_denied",
        json!({
            "session": session_id,
            "surface": "app-session-relay",
            "reason": reason,
        }),
    );
    Err(format!(
        "the package backing this App session is no longer trusted: {reason}"
    ))
}

/// Which routes a relay may reach.
///
/// Only a `Session`-subject system-service route: the exact shape whose
/// authority *is* the App session grant. Everything else — root access,
/// peer-scoped, peer-session, handle-addressed, the consent surface,
/// session and identity control, the scheduler, the journal, and the
/// relay route itself — is refused here and refused again by
/// [`authority::authorize_relayed`].
fn relayable_route(command: Command) -> Result<&'static Route, String> {
    let route = command.route();
    let refuse = |reason: &str| Err(format!("route `{}` {reason}", route.name));
    if command == Command::AppSessionRelay {
        return refuse("cannot be relayed through itself");
    }
    if route.access != Access::User {
        return refuse("is not reachable by an unprivileged session");
    }
    if route.authority.subject != authority::SubjectSource::Session {
        return refuse("is not addressed by an App session");
    }
    if route.authority.audience != authority::Audience::SystemService {
        return refuse("is outside the system-service audience a relay may reach");
    }
    Ok(route)
}

fn issue_relay_grant(
    launch_handle: &str,
    session_id: &str,
    app_id: Option<&str>,
    uid: u32,
    launcher_pid: u32,
) -> Result<String, String> {
    let principal = authority::Principal::of_process(uid, launcher_pid)
        .ok_or_else(|| format!("App launcher {launcher_pid} could not be identified"))?;
    let (handle, view) = authority::authority()
        .attenuate(
            launch_handle,
            authority::Attenuation {
                issuer: authority::Issuer::AppSessionAuthority,
                principal,
                binding: authority::Binding::Process,
                subject: authority::Subject::session(session_id)
                    .with_app(app_id.map(ToOwned::to_owned)),
                audience: authority::AudienceSet::of(&[authority::Audience::AppRelay]),
                caps: CapSet::new(),
                lifetime: SESSION_GRANT_TTL,
                uses: authority::Uses::Unbounded,
                index_session: false,
            },
        )
        .map_err(|error| error.to_string())?;
    authority::audit::record_issued(&view, None);
    Ok(handle.into_wire())
}

/// Mint target authority for one authenticated App MCP call.
///
/// The caller's exact `agent.invoke:<app>/<tool>` capability was checked in
/// the call plan and is deliberately absent from `caps`. The grant is rooted
/// at the daemon because target capabilities may have been approved for this
/// exact call rather than delegated as standing caller authority.
fn issue_gateway_target_grant(
    session_id: &str,
    app_id: &str,
    uid: u32,
    child_pid: u32,
    caps: &CapSet,
    context: &crate::agent::tools::app_gateway::McpCallContext,
    ceiling: Option<&Ceiling>,
) -> Result<(), String> {
    authority::revoke_indexed_session(session_id);
    let remaining = context.remaining(TARGET_CALL_GRANT_TTL)?;
    let principal = authority::Principal::of_process(uid, child_pid)
        .ok_or_else(|| format!("App process {child_pid} could not be identified"))?;
    let (_handle, view) = authority::authority()
        .issue(authority::Issuance {
            issuer: authority::Issuer::AppGateway,
            principal,
            binding: authority::Binding::ProcessTree,
            subject: authority::Subject::session(session_id)
                .with_app(Some(app_id.to_string()))
                .with_task(context.task_id.clone()),
            audience: permitted_audiences(
                ceiling,
                &[
                    authority::Audience::SystemService,
                    authority::Audience::Credential,
                ],
            ),
            caps: caps.clone(),
            lifetime: remaining,
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
    ceiling: Option<&Ceiling>,
) -> Result<(), String> {
    authority::revoke_indexed_session(session_id);
    issue_session_grant(
        launch_handle,
        session_id,
        app_id,
        uid,
        child_pid,
        caps,
        ceiling,
    )
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
