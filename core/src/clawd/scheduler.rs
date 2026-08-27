//! Authority for the proactive-scheduler operations `clawd` brokers.
//!
//! `cos cron` and `cos triggers` act on the root-owned job store the
//! daemon's heartbeat drives, so a user CLI reaches it through
//! `scheduler.run` instead of writing into `/var/lib/cos` itself. Every
//! request therefore forces the daemon to answer two questions: what
//! authority does this peer hold, and what authority may the job it
//! creates carry once the call returns?
//!
//! Neither answer may come from the request, and neither may come from
//! how the peer happens to be running. An interactive terminal, an
//! unconfined `NoNewPrivs` process, a particular executable path or a
//! socket group are all things a local process can arrange for itself;
//! none of them is authority. `clawd` resolves the peer from the
//! connection (uid, pid, process start time), finds the nearest session
//! it registered itself in the root-owned routed registry, and derives
//! every capability from that session alone.
//!
//! A peer with no registered session holds nothing it can delegate.
//! Creating a job, re-arming one, or executing one now needs a one-shot
//! grant approved through the privileged approval helper and bound to
//! that exact peer identity, verb and scope. Listing, inspecting or
//! retiring rows the authenticated owner already owns neither creates
//! nor widens authority, so those are answered with the single
//! capability the subsystem's own gate requires and nothing else.
//!
//! What a job may do is bounded twice: by the authority its creator
//! could prove, and by the same scheduled-execution ceiling
//! `cron::execute_job` and `triggers::execution_owner` apply before a
//! stored snapshot runs.

use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use crate::caps::{Cap, CapSet, Role, Scope, Verb};
use crate::proc::SessionInfo;

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

/// Bounds on a scheduler argument list. Arguments are forwarded to the
/// subsystem parsers, so their shape is checked before anything
/// authorizes or dispatches the call.
const MAX_ARGS: usize = 64;
const MAX_ARG_BYTES: usize = 8 * 1024;

/// Longest process ancestry walked while looking for the session that
/// owns the peer.
const MAX_ANCESTRY_DEPTH: usize = 64;

pub async fn run(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    let uid = client.require_uid()?;
    // Canonical, ownership-checked passwd home. The scheduled ceiling
    // and the ceiling `cron`/`triggers` re-apply at execution are both
    // derived from it, so a home the daemon cannot verify authorises
    // nothing rather than falling back to a raw passwd string.
    let home = super::system_caps::verified_owner_home(uid)?;
    let request = SchedulerCommand::parse(&params)?;
    let authority = authenticate_caller(client, uid, &home).await?;
    let caps = authorize(&request, &authority)?;
    let session = trusted_session(&request, &authority, caps, &home);

    let permit = scheduler_slots()
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| "scheduler executor is unavailable".to_string())?;
    let subsystem = request.subsystem;
    let command = request.command;
    let args = request.args;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| format!("create scheduler runtime: {error}"))?;
        runtime.block_on(crate::paths::with_user_override(
            uid,
            home,
            crate::proc::with_trusted_session_override(session, async move {
                match subsystem {
                    Subsystem::Cron => crate::cron::run(&command, &args),
                    Subsystem::Triggers => crate::triggers::run(&command, &args),
                }
            }),
        ))
    })
    .await
    .map_err(|error| format!("scheduler executor failed: {error}"))?
    .map_err(BrokerError::from)
}

fn scheduler_slots() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)))
}

// ---------------------------------------------------------------------------
// Request validation
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Subsystem {
    Cron,
    Triggers,
}

impl Subsystem {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "cron" => Ok(Subsystem::Cron),
            "triggers" => Ok(Subsystem::Triggers),
            other => Err(format!("unsupported scheduler subsystem: {other}")),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Subsystem::Cron => "cron",
            Subsystem::Triggers => "triggers",
        }
    }

    /// Verb a job of this subsystem spends when it actually runs.
    fn executor_verb(self) -> Verb {
        match self {
            Subsystem::Cron => Verb::PROC_SPAWN,
            Subsystem::Triggers => Verb::AGENT_SPAWN,
        }
    }
}

/// A scheduler request the daemon has already validated: known
/// subsystem, allow-listed command, bounded arguments, and the
/// well-formed identifier of the job or rule the command addresses.
#[derive(Debug)]
struct SchedulerCommand {
    subsystem: Subsystem,
    command: String,
    args: Vec<String>,
    /// Job or rule the command names, when it names one. Resolved the
    /// same way the subsystem resolves it, so authorization and
    /// dispatch can never disagree about the target.
    target: Option<String>,
    /// Credentials a new cron job asks to have injected. Each one
    /// becomes an exact `secret.read` scope that must be authorized.
    credentials: Vec<String>,
}

impl SchedulerCommand {
    fn parse(params: &Value) -> Result<Self, String> {
        let subsystem = Subsystem::parse(&required_string(params, "subsystem")?)?;
        let command = required_string(params, "command")?;
        let args = parse_args(params)?;

        if command == "tick" {
            return Err("scheduler tick is reserved for the kernel heartbeat".to_string());
        }
        let supported = match subsystem {
            Subsystem::Cron => matches!(
                command.as_str(),
                "add" | "remove" | "list" | "status" | "enable" | "disable" | "logs" | "run"
            ),
            Subsystem::Triggers => matches!(
                command.as_str(),
                "add" | "list" | "remove" | "rm" | "enable" | "disable" | "run"
            ),
        };
        if !supported {
            return Err(format!(
                "unsupported {} command: {command}",
                subsystem.as_str()
            ));
        }

        let target = target_identifier(subsystem, &command, &args)?;
        let credentials = if subsystem == Subsystem::Cron && command == "add" {
            requested_credentials(&args)?
        } else {
            Vec::new()
        };

        Ok(Self {
            subsystem,
            command,
            args,
            target,
            credentials,
        })
    }

    /// Capability the subsystem's own gate requires for work that only
    /// reads or retires rows the authenticated owner already owns.
    ///
    /// These commands cannot create a job, re-stamp a capability
    /// snapshot or start one, and the subsystems match every row
    /// against the owner uid the daemon derived from the peer — so
    /// answering them costs no delegated authority.
    fn owned_caps(&self) -> Vec<Cap> {
        match self.command.as_str() {
            "run" => Vec::new(),
            "logs" => vec![Cap::new(Verb::DATA_LOG_READ, Scope::Wild)],
            _ => vec![Cap::new(Verb::TIME_CRON, Scope::Wild)],
        }
    }

    /// Capability this command delegates into a persisted job or spends
    /// to execute one now. Every entry must be authority the peer can
    /// prove it holds, or an approved one-shot grant bound to it.
    fn delegated_caps(&self) -> Vec<Cap> {
        let mut caps = Vec::new();
        if matches!(self.command.as_str(), "add" | "enable" | "run") {
            caps.push(Cap::new(self.subsystem.executor_verb(), Scope::Wild));
        }
        for credential in &self.credentials {
            caps.push(Cap::new(
                Verb::SECRET_READ,
                Scope::name(format!("default/{credential}")),
            ));
        }
        caps
    }

    /// True when the command stamps a fresh owner capability snapshot
    /// onto a persisted job or rule, so the authority it binds outlives
    /// this call.
    fn binds_owner_snapshot(&self) -> bool {
        matches!(self.command.as_str(), "add" | "enable")
    }

    fn label(&self) -> String {
        match &self.target {
            Some(target) => format!("cos {} {} {target}", self.subsystem.as_str(), self.command),
            None => format!("cos {} {}", self.subsystem.as_str(), self.command),
        }
    }
}

fn parse_args(params: &Value) -> Result<Vec<String>, String> {
    let args = match params.get("args") {
        None | Some(Value::Null) => Vec::new(),
        Some(value) => serde_json::from_value::<Vec<String>>(value.clone())
            .map_err(|error| format!("invalid scheduler args: {error}"))?,
    };
    if args.len() > MAX_ARGS {
        return Err(format!("scheduler accepts at most {MAX_ARGS} arguments"));
    }
    for arg in &args {
        if arg.len() > MAX_ARG_BYTES {
            return Err(format!(
                "scheduler arguments are limited to {MAX_ARG_BYTES} bytes"
            ));
        }
        if arg.contains('\0') {
            return Err("scheduler arguments cannot contain NUL".to_string());
        }
    }
    Ok(args)
}

/// Resolve the job or rule a command addresses exactly the way the
/// subsystem resolves it, then validate it against that subsystem's
/// identifier rules before anything authorizes or dispatches the call.
fn target_identifier(
    subsystem: Subsystem,
    command: &str,
    args: &[String],
) -> Result<Option<String>, String> {
    if command == "list" {
        return Ok(None);
    }
    match subsystem {
        Subsystem::Cron => {
            let id = args
                .first()
                .ok_or_else(|| format!("cos cron {command} requires a job id"))?;
            validate_cron_id(id)?;
            Ok(Some(id.clone()))
        }
        Subsystem::Triggers => {
            // `triggers add` reads `--id`; the other commands take the
            // first positional and fall back to `--id`.
            let id = if command == "add" {
                flag(args, "id")
            } else {
                args.iter()
                    .find(|arg| !arg.starts_with("--"))
                    .cloned()
                    .or_else(|| flag(args, "id"))
            }
            .ok_or_else(|| format!("cos triggers {command} requires a rule id"))?;
            validate_trigger_id(&id)?;
            Ok(Some(id))
        }
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    let key = format!("--{name}");
    args.iter()
        .position(|arg| arg == &key)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

/// Mirrors `cron::validate_id`.
fn validate_cron_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("cron job id cannot be empty".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("cron job id must be alphanumeric (hyphens/underscores allowed)".to_string());
    }
    Ok(())
}

/// Mirrors `triggers::sanitize_id`.
fn validate_trigger_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.starts_with('.') {
        return Err("trigger id cannot be empty or start with `.`".to_string());
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(
            "trigger id must be alphanumeric (hyphens/underscores/dots allowed)".to_string(),
        );
    }
    Ok(())
}

/// Credential names a `cron add` asks to have injected, parsed the way
/// `cron::cmd_add` parses them so the scope authorized here is the
/// scope `cron::execute_job` later checks.
fn requested_credentials(args: &[String]) -> Result<Vec<String>, String> {
    let Some(raw) = flag(args, "credentials") else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = Vec::new();
    for name in raw
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        if name.len() > 128
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(format!("invalid credential name: {name}"));
        }
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_string());
        }
    }
    Ok(names)
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

// ---------------------------------------------------------------------------
// Peer authority
// ---------------------------------------------------------------------------

/// Authority of the process on the other end of the socket, resolved by
/// `clawd` and never described by the caller.
#[derive(Debug)]
struct CallerAuthority {
    uid: u32,
    /// Session `clawd` itself registered for this peer, when it runs
    /// inside one. Recorded as the scheduler session's parent.
    parent: Option<String>,
    /// Identity an approved grant is bound to: the authenticated parent
    /// session, or a value derived purely from the peer's uid, pid and
    /// process start time. Never a value the request supplied and never
    /// one a sibling process shares.
    grant_session: String,
    /// Requester label for the approval UI and audit trail. Carries
    /// authenticated facts only and is never used for matching.
    requester: String,
    /// What the peer already holds and may delegate to scheduled work
    /// without a new decision. Empty for a peer the daemon could not
    /// tie to a registered session.
    delegable: CapSet,
    /// Standing, home-bounded ceiling a job created by this peer may
    /// inherit. Daemon policy, never asserted by the caller.
    ceiling: CapSet,
    tier: Option<u8>,
    role: Option<String>,
}

async fn authenticate_caller(
    client: &ClientIdentity,
    uid: u32,
    home: &Path,
) -> Result<CallerAuthority, String> {
    let pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    if pid <= 1 {
        return Err("clawd peer pid is not a schedulable process".to_string());
    }
    if process_uid(pid) != Some(uid) {
        return Err(format!("clawd peer {pid} is not owned by uid {uid}"));
    }
    let start_time_ticks = crate::proc::read_start_time_ticks_pub(pid).ok_or_else(|| {
        format!("cannot establish a scheduler identity for process {pid} without its start time")
    })?;
    let owner_home = home.to_path_buf();
    let sessions = crate::paths::with_user_override(uid, owner_home, async {
        crate::proc::registry_sessions()
    })
    .await;
    caller_authority(&sessions, uid, pid, start_time_ticks, home)
}

/// Derive the peer's authority from the root-owned routed registry.
///
/// A peer running inside a session `clawd` registered itself inherits
/// that session's capabilities, attenuated to the scheduled-execution
/// ceiling. A peer running inside an App session may not manage
/// proactive jobs at all — an App holds delegated authority, not a
/// place new authority is minted from. Anything else is unregistered:
/// it can delegate nothing, and only an approved grant moves it on.
fn caller_authority(
    sessions: &[SessionInfo],
    uid: u32,
    pid: u32,
    start_time_ticks: u64,
    home: &Path,
) -> Result<CallerAuthority, String> {
    let scheduled = scheduled_ceiling(home);
    let requester = format!("uid:{uid} pid:{pid} start:{start_time_ticks}");
    match nearest_registered_session(sessions, pid)? {
        Some(session) if session.app_id.is_some() => Err(format!(
            "App session `{}` cannot manage proactive jobs",
            session.session_id
        )),
        Some(session) => {
            let held = session.caps.clone().unwrap_or_default();
            let delegable = held.intersect(&scheduled);
            Ok(CallerAuthority {
                uid,
                parent: Some(session.session_id.clone()),
                grant_session: session.session_id.clone(),
                requester,
                ceiling: delegable.clone(),
                delegable,
                tier: session.tier,
                role: session.role.clone(),
            })
        }
        // Root already holds every capability on this machine, so
        // deriving the scheduled ceiling for it grants nothing new.
        None if uid == 0 => Ok(CallerAuthority {
            uid,
            parent: None,
            grant_session: unregistered_grant_identity(uid, pid, start_time_ticks),
            requester,
            delegable: scheduled.clone(),
            ceiling: scheduled,
            tier: Some(Role::Admin.credential_tier()),
            role: Some(Role::Admin.name().to_string()),
        }),
        None => Ok(CallerAuthority {
            uid,
            parent: None,
            grant_session: unregistered_grant_identity(uid, pid, start_time_ticks),
            requester,
            // Being able to open the socket is not authority.
            delegable: CapSet::new(),
            ceiling: scheduled.intersect(&super::system_caps::local_launcher_ceiling(home)),
            tier: Some(Role::Worker.credential_tier()),
            role: Some(Role::Worker.name().to_string()),
        }),
    }
}

/// Identity an approval grant is bound to when the peer belongs to no
/// registered session. Derived only from kernel-reported facts about
/// this exact process, so a sibling can read a pending request but can
/// never make the daemon derive another peer's identity for its own
/// connection.
fn unregistered_grant_identity(uid: u32, pid: u32, start_time_ticks: u64) -> String {
    format!("scheduler:uid={uid}:pid={pid}:start={start_time_ticks}")
}

/// Walk the peer's ancestry looking for a session `clawd` registered
/// itself.
///
/// `Ok(None)` means the walk completed and found nothing. An unreadable
/// `/proc` entry, an ancestor that exited mid-walk, or a chain longer
/// than the budget is not the same thing: the daemon cannot prove where
/// the peer sits, so it reports an error and the caller fails closed.
fn nearest_registered_session(
    sessions: &[SessionInfo],
    pid: u32,
) -> Result<Option<&SessionInfo>, String> {
    let mut current = pid;
    for _ in 0..MAX_ANCESTRY_DEPTH {
        if let Some(session) = sessions.iter().find(|session| {
            !session.pending_bind && session.pid == current && session_process_is_current(session)
        }) {
            return Ok(Some(session));
        }
        if current <= 1 {
            return Ok(None);
        }
        current = process_parent_pid(current).ok_or_else(|| {
            format!("could not resolve the scheduler caller's ancestry above process {current}")
        })?;
    }
    Err(format!(
        "scheduler caller ancestry for process {pid} is longer than the supported depth"
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

/// Ceiling every scheduled job runs under. Mirrors the intersection
/// `cron::execute_job` and `triggers::execution_owner` apply before a
/// stored snapshot is used, so what the daemon persists is exactly what
/// it will later execute.
fn scheduled_ceiling(home: &Path) -> CapSet {
    Role::AgentHost.caps_with_scopes(
        Some(Scope::path(format!("{}/**", home.display()))),
        Some(Scope::Wild),
        Some(Scope::Wild),
    )
}

// ---------------------------------------------------------------------------
// Authorization
// ---------------------------------------------------------------------------

/// Settle one scheduler operation against the peer's authority.
///
/// Owner-scoped work is answered with the exact capability its gate
/// needs. Anything that delegates authority into a persisted job or
/// spends it to run one must be covered by capabilities the peer holds,
/// or by approved one-shot grants bound to this peer, verb and scope; a
/// set that is short even one grant consumes nothing and files the
/// missing decisions.
fn authorize(
    request: &SchedulerCommand,
    authority: &CallerAuthority,
) -> Result<CapSet, BrokerError> {
    let mut caps = CapSet::new();
    caps.extend(request.owned_caps());

    let delegated = request.delegated_caps();
    let missing: Vec<Cap> = delegated
        .iter()
        .filter(|cap| !authority.delegable.covers(cap))
        .cloned()
        .collect();
    if !missing.is_empty() {
        match crate::approvals::consume_grant_set_once_for_owner(
            &authority.grant_session,
            &missing,
            Some(authority.uid),
        ) {
            Ok(true) => {}
            Ok(false) => return Err(request_approvals(request, authority, &missing)),
            Err(error) => {
                return Err(format!("could not settle approved permission grants: {error}").into())
            }
        }
    }
    caps.extend(delegated);

    if request.binds_owner_snapshot() {
        // The snapshot outlives this call, so the job inherits the
        // peer's standing, home-bounded ceiling — never more.
        caps.extend(authority.ceiling.iter().cloned());
    }
    Ok(caps)
}

/// File (or reuse) one pending request per capability the peer cannot
/// delegate, and tell it which decisions it is waiting on.
///
/// The ids are not authority: grants are matched on the daemon-derived
/// peer identity plus the exact verb and scope, so a sibling that reads
/// an id still cannot spend the decision.
fn request_approvals(
    request: &SchedulerCommand,
    authority: &CallerAuthority,
    missing: &[Cap],
) -> BrokerError {
    let pending = crate::approvals::list_pending_for_owner(Some(authority.uid));
    let mut ids = Vec::with_capacity(missing.len());
    let mut failures = Vec::new();
    for cap in missing {
        // A capability the user already approved is settled on retry;
        // asking again would queue a second prompt for one decision.
        match crate::approvals::has_approved_grant_for_owner(
            &authority.grant_session,
            cap,
            Some(authority.uid),
        ) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                failures.push(error);
                continue;
            }
        }
        let existing = pending.iter().find(|pending| {
            pending.session == authority.grant_session
                && pending.verb == cap.verb.as_str()
                && pending.scope == cap.scope
        });
        let submitted = match existing {
            Some(pending) => Ok(pending.id.clone()),
            None => crate::approvals::submit_owned(
                cap.verb,
                cap.scope.clone(),
                authority.grant_session.clone(),
                format!(
                    "{} requires {}:{}",
                    request.label(),
                    cap.verb.as_str(),
                    cap.scope
                ),
                Some(authority.requester.clone()),
                Some(authority.uid),
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
            "scheduler caller cannot delegate {summary}; \
             could not create an approval request: {}",
            failures.join("; ")
        )
        .into();
    }
    BrokerError::with_data(
        format!("scheduler caller cannot delegate {summary}; awaiting approval"),
        json!({
            "status": "approval_required",
            "approval_requests": ids,
        }),
    )
    .classified("approval_required")
}

/// Session installed for exactly one scheduler operation.
///
/// It carries the authorized capabilities and nothing else, is bound to
/// this daemon process, and lives only for the single `cron` /
/// `triggers` call below — it never becomes ambient authority for a
/// later request.
fn trusted_session(
    request: &SchedulerCommand,
    authority: &CallerAuthority,
    caps: CapSet,
    home: &Path,
) -> SessionInfo {
    let pid = std::process::id();
    SessionInfo {
        session_id: format!("scheduler-client-{}", uuid::Uuid::new_v4().simple()),
        pid,
        command: vec![format!(
            "{}.{}",
            request.subsystem.as_str(),
            request.command
        )],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("scheduler-client".to_string()),
        parent: authority.parent.clone(),
        workdir: Some(home.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: authority.tier,
        scope: Some("scheduler-client".to_string()),
        priority: None,
        caps: Some(caps),
        transient_caps: None,
        role: authority.role.clone(),
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(pid),
    }
}

// ---------------------------------------------------------------------------
// Peer process facts
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
fn process_uid(pid: u32) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("Uid:")
                    .and_then(|value| value.split_whitespace().next())
                    .and_then(|value| value.parse::<u32>().ok())
            })
        })
}

#[cfg(not(target_os = "linux"))]
fn process_uid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(target_os = "linux")]
fn process_parent_pid(pid: u32) -> Option<u32> {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("PPid:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
            })
        })
}

#[cfg(not(target_os = "linux"))]
fn process_parent_pid(_pid: u32) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/scheduler.rs"
    ));
}
