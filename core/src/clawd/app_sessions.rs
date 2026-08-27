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
//! Registration returns an opaque launch handle. It is bound to the
//! connecting launcher process (pid + start time), single-use for the
//! pid bind, short-lived, and never handed to the launched App — so an
//! App cannot bind, re-scope, or drop sessions, and an unrelated local
//! process cannot drive somebody else's launch.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::apps::App;
use crate::caps::{Cap, CapSet, Manifest, Need, Role, Scope, ScopeBinding, ScopeKind, Verb};
use crate::clawd::protocol::BrokerError;
use crate::proc::SessionInfo;

use super::client_identity::ClientIdentity;

/// How long an issued handle may be used to bind a child process.
/// Long enough for a slow interpreter start, short enough that a
/// forgotten handle is not standing authority.
const BIND_WINDOW: Duration = Duration::from_secs(120);

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

pub async fn register(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let app_id = required_string(&params, "app_id")?;
    let kind = launch_kind(&params)?;
    let launcher = authenticate_launcher(client, uid, home.clone()).await?;
    let delegation = Delegation::new(&launcher, uid, &home, &params)?;
    let app = installed_app(&app_id)?;

    let (command, mut plan) = match kind {
        LaunchKind::Operation => {
            let operation = required_string(&params, "operation")?;
            let args = string_array(&params, "args")?;
            let plan = operation_plan(&app, &operation, &args, &delegation)?;
            (format!("cos app {app_id} {operation}"), plan)
        }
        LaunchKind::Gui => {
            let exec = required_string(&params, "operation")?;
            let plan = gui_plan(&app, &exec, &delegation)?;
            (format!("cos app {app_id} {exec}"), plan)
        }
        LaunchKind::Mcp => (format!("cos app {app_id} session"), LaunchPlan::default()),
    };
    plan.require(
        Cap::new(Verb::AGENT_INVOKE, Scope::name(&app_id)),
        &delegation,
    );
    let caps = authorize_plan(&delegation, plan)?;

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
    };

    let proc_dir = install_session(uid, home, info).await?;
    let handle = issue_handle(&session_id, uid, &launcher);
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
        caps: Some(caps),
        transient_caps: None,
        role: Some(role.name().to_string()),
        app_id: Some(app_id.clone()),
        pending_bind: true,
        start_time_ticks: None,
    };
    let proc_dir = install_session(uid, home, info).await?;
    let handle = issue_handle(&session_id, uid, &launcher);
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
        caps: Some(caps),
        transient_caps: None,
        role: launcher.role.clone(),
        app_id: None,
        pending_bind: true,
        start_time_ticks: None,
    };
    let proc_dir = install_session(uid, home, info).await?;
    let handle = issue_handle(&session_id, uid, &launcher);
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
    authorize_handle(&handle, &session_id, uid, launcher_pid, true)?;
    let child_pid = required_u32(&params, "pid")?;
    if child_pid == launcher_pid {
        return Err("App session must bind a child process".to_string());
    }
    if !is_descendant_of(child_pid, launcher_pid) {
        return Err(format!(
            "process {child_pid} is not descended from launcher {launcher_pid}"
        ));
    }
    if process_uid(child_pid) != Some(uid) {
        return Err(format!("App process {child_pid} is not owned by uid {uid}"));
    }
    let bind_id = session_id.clone();
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::bind_session_process(&bind_id, child_pid)
    })
    .await?;
    mark_handle_bound(&handle);
    Ok(json!({"bound": true}))
}

pub async fn set_transient(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let session_id = required_string(&params, "session_id")?;
    let handle = required_string(&params, "handle")?;
    let launcher_pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    authorize_handle(&handle, &session_id, uid, launcher_pid, false)?;

    let lookup_id = session_id.clone();
    let app_id = crate::paths::with_user_override(uid, home.clone(), async move {
        crate::proc::session_info_by_id(&lookup_id)
            .and_then(|session| session.app_id)
            .ok_or_else(|| "App session not found".to_string())
    })
    .await?;

    let caps = match params.get("call") {
        None | Some(Value::Null) => None,
        Some(call) => {
            let launcher = authenticate_launcher(client, uid, home.clone()).await?;
            // A serialized MCP call is answered in the caller's own
            // process, so a denial is retried under the same
            // pid/start identity.
            let delegation = Delegation::new(&launcher, uid, &home, &params)?;
            let plan = session_tool_plan(&app_id, call, &delegation)
                .map_err(|error| error.message)?;
            Some(authorize_plan(&delegation, plan).map_err(|error| error.message)?)
        }
    };
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::set_app_session_transient_caps(&session_id, caps)
    })
    .await?;
    Ok(json!({"updated": true}))
}

pub async fn deregister(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let session_id = required_string(&params, "session_id")?;
    let handle = required_string(&params, "handle")?;
    let launcher_pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    authorize_handle(&handle, &session_id, uid, launcher_pid, false)?;
    let remove_id = session_id.clone();
    crate::paths::with_user_override(uid, home, async move {
        crate::proc::deregister_session(&remove_id);
    })
    .await;
    release_handle(&handle);
    Ok(json!({"removed": true}))
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
    if process_uid(pid) != Some(uid) {
        return Err(format!("clawd peer {pid} is not owned by uid {uid}"));
    }
    if process_no_new_privs(pid) != Some(false) {
        return Err("App processes cannot manage App sessions".to_string());
    }
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid);
    let sessions =
        crate::paths::with_user_override(uid, home.clone(), async { crate::proc::registry_sessions() })
            .await;
    launcher_authority(&sessions, pid, start_time_ticks, &home)
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
) -> Result<LaunchPlan, BrokerError> {
    let app = installed_app(app_id)?;
    let tool_name = required_string(call, "tool")?;
    let args: BTreeMap<String, Value> = match call.get("args") {
        None | Some(Value::Null) => BTreeMap::new(),
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Some(_) => return Err("session tool args must be an object".to_string().into()),
    };
    let tool = app
        .manifest
        .session
        .as_ref()
        .and_then(|session| session.tools.iter().find(|tool| tool.name == tool_name))
        .ok_or_else(|| format!("App `{app_id}` has no session tool `{tool_name}`"))?;
    let effective = app
        .manifest
        .resolve_session_tool_call(&tool_name, &args, &delegation.paths)
        .map_err(|error| format!("resolve `{tool_name}` capabilities: {error}"))?;
    derive_plan(&tool.needs, &effective.needs, delegation)
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
// Launch handles
// ---------------------------------------------------------------------------

struct LaunchHandle {
    session_id: String,
    uid: u32,
    launcher_pid: u32,
    launcher_start_time_ticks: Option<u64>,
    bind_deadline: Instant,
    bound: bool,
}

fn handles() -> MutexGuard<'static, HashMap<String, LaunchHandle>> {
    static HANDLES: OnceLock<Mutex<HashMap<String, LaunchHandle>>> = OnceLock::new();
    HANDLES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn issue_handle(session_id: &str, uid: u32, launcher: &LauncherAuthority) -> String {
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut store = handles();
    prune_handles(&mut store);
    store.insert(
        token.clone(),
        LaunchHandle {
            session_id: session_id.to_string(),
            uid,
            launcher_pid: launcher.pid,
            launcher_start_time_ticks: launcher.start_time_ticks,
            bind_deadline: Instant::now() + BIND_WINDOW,
            bound: false,
        },
    );
    token
}

fn authorize_handle(
    token: &str,
    session_id: &str,
    uid: u32,
    launcher_pid: u32,
    for_bind: bool,
) -> Result<(), String> {
    let mut store = handles();
    prune_handles(&mut store);
    let handle = store
        .get(token)
        .ok_or_else(|| "App launch handle is unknown or expired".to_string())?;
    if handle.session_id != session_id || handle.uid != uid {
        return Err("App launch handle does not cover this session".to_string());
    }
    if handle.launcher_pid != launcher_pid {
        return Err("App launch handle belongs to a different launcher".to_string());
    }
    if !crate::proc::is_alive_with_start_time(handle.launcher_pid, handle.launcher_start_time_ticks)
    {
        return Err("App launch handle belongs to an exited launcher".to_string());
    }
    if for_bind {
        if handle.bound {
            return Err("App launch handle has already bound a process".to_string());
        }
        if Instant::now() > handle.bind_deadline {
            return Err("App launch handle expired before binding a process".to_string());
        }
    }
    Ok(())
}

fn mark_handle_bound(token: &str) {
    if let Some(handle) = handles().get_mut(token) {
        handle.bound = true;
    }
}

fn release_handle(token: &str) {
    handles().remove(token);
}

/// Drop handles whose launcher is gone and unbound handles past their
/// bind window, so a long-lived daemon never keeps authority for a
/// process that no longer exists.
fn prune_handles(store: &mut HashMap<String, LaunchHandle>) {
    let now = Instant::now();
    store.retain(|_, handle| {
        if !handle.bound && now > handle.bind_deadline {
            return false;
        }
        crate::proc::is_alive_with_start_time(handle.launcher_pid, handle.launcher_start_time_ticks)
    });
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
