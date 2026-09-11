//! Private authority for verified App-owned stdio sessions.
//!
//! The task controller owns call IDs, serialization of the App transport,
//! handle aliases, and process retirement. Only freshly authorized call caps
//! enter a rotated launch envelope; generic attenuation is never widened.
//! Control grants never outlive the prior control grant. Active App/relay
//! grants additionally expire within 75 seconds, even if the worker withholds
//! call-end. Clearing restores only base identity under the remaining control
//! lifetime; it never restores the expired call's effect capabilities.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use super::super::{
    self as sessions, authority, App, BrokerError, Cap, CapSet, Ceiling, ClientIdentity,
    Delegation, LaunchKind, LauncherAuthority, Scope, SessionInfo, Verb,
};
use super::{authenticate_task_host, equivalent_argument, task_host_delegation};
use crate::provenance::runtime::{Instance, InstanceClass, PackageRef, ProcessIdentity};

/// The ordinary 60-second call timeout plus bounded teardown headroom.
const ACTIVE_CALL_WINDOW: Duration = Duration::from_secs(75);

pub(crate) struct TaskHostSession<'a> {
    pub app_id: &'a str,
    pub package_digest: &'a str,
}

pub(crate) struct TaskHostSessionCall<'a> {
    pub tool: &'a str,
    pub args: &'a BTreeMap<String, Value>,
}

/// Authenticate a stdio declaration and signed entrypoint without granting
/// anything or executing package code.
pub(crate) fn prepare_session_for_task_host(
    client: &ClientIdentity,
    parent: &SessionInfo,
    session: &TaskHostSession<'_>,
) -> Result<(), BrokerError> {
    authenticate_task_host(client, parent)?;
    client
        .require_home_dir()
        .map_err(BrokerError::authorization)?;
    verified_session(session)?;
    Ok(())
}

pub(crate) async fn register_session_for_task_host(
    params: Value,
    client: &ClientIdentity,
    parent: &SessionInfo,
    session: &TaskHostSession<'_>,
) -> Result<Value, BrokerError> {
    let request: crate::clawd::wire::requests::AppSessionRegister =
        serde_json::from_value(params.clone())
            .map_err(|_| BrokerError::execution("invalid task-host session registration"))?;
    if request.app_id.as_str() != session.app_id
        || request.kind.as_ref().map(|kind| kind.as_str()) != Some("mcp")
        || request.operation.is_some()
        || request
            .args
            .as_ref()
            .is_some_and(|args| !args.as_slice().is_empty())
    {
        return Err(BrokerError::authorization(
            "registration must name only the assigned App-owned session",
        ));
    }
    let launcher = authenticate_task_host(client, parent)?;
    let uid = client.require_uid().map_err(BrokerError::authorization)?;
    let home = client
        .require_home_dir()
        .map_err(BrokerError::authorization)?;
    let delegation = task_host_delegation(&launcher, uid, &home, parent, &params)?;
    let app = verified_session(session)?;
    sessions::register_with_launcher(
        params,
        home,
        app,
        LaunchKind::Mcp,
        launcher,
        delegation,
        None,
    )
    .await
}

pub(crate) fn prepare_session_call_for_task_host(
    client: &ClientIdentity,
    parent: &SessionInfo,
    session: &TaskHostSession<'_>,
    call: &TaskHostSessionCall<'_>,
) -> Result<BTreeMap<String, Value>, BrokerError> {
    let launcher = authenticate_task_host(client, parent)?;
    let uid = client.require_uid().map_err(BrokerError::authorization)?;
    let home = client
        .require_home_dir()
        .map_err(BrokerError::authorization)?;
    let delegation = task_host_delegation(&launcher, uid, &home, parent, &Value::Null)?;
    let app = verified_session(session)?;
    let original = call_value(call)?;
    let (_, effective) = sessions::session_tool_call(&app, &original, &delegation)?;
    let canonical = json!({"tool":call.tool, "args":effective.values});
    validate_call(&app, call, &canonical, &delegation)?;
    Ok(effective.values)
}

/// Start or clear one call under the controller's retained original request.
///
/// The controller must supply current real handles and retire the session
/// after a failed clear or a non-approval transition failure. Successful
/// rotation invalidates all old handles and returns replacement launch/relay
/// handles. A revoked/unverifiable package is cleared and revoked, never
/// reissued authority. A live control grant can clear an expired call grant
/// back to base-only authority. Runtime process records remain controller-owned.
pub(crate) async fn set_session_call_for_task_host(
    params: Value,
    client: &ClientIdentity,
    parent: &SessionInfo,
    session: &TaskHostSession<'_>,
    call: Option<&TaskHostSessionCall<'_>>,
) -> Result<Value, BrokerError> {
    let launcher = authenticate_task_host(client, parent)?;
    let uid = client.require_uid().map_err(BrokerError::authorization)?;
    let home = client
        .require_home_dir()
        .map_err(BrokerError::authorization)?;
    serde_json::from_value::<crate::clawd::wire::requests::AppSessionSetTransient>(params.clone())
        .map_err(|_| BrokerError::execution("invalid task-host session call"))?;
    let session_id = sessions::required_string(&params, "session_id")?;
    let handle = sessions::required_string(&params, "handle")?;
    let offered = params.get("call").filter(|value| !value.is_null());
    if offered.is_some() != call.is_some() {
        return Err(BrokerError::authorization(
            "call control does not match the retained request",
        ));
    }

    let serializer = sessions::session_lock(&session_id);
    let _transition = serializer.lock().await;
    let sampled = Instant::now();
    let launch = sessions::require_launch_grant(client, &handle, &session_id, uid)
        .map_err(BrokerError::authorization)?;
    let bound = crate::paths::with_user_override(uid, home.clone(), async {
        crate::proc::session_info_by_id(&session_id)
    })
    .await
    .ok_or_else(|| BrokerError::authorization("App session is not registered"))?;
    validate_assignment(&launch, &bound, parent, session, uid)?;
    let instance = match crate::provenance::runtime::instance_for(uid, &session_id) {
        Ok(Some(instance)) => instance,
        result => {
            revoke_launch(launch.id, &session_id);
            sessions::rollback_transient_caps(uid, home, &session_id, None).await;
            return Err(BrokerError::unavailable(match result {
                Err(error) => error,
                _ => "App session has no root runtime record".to_string(),
            }));
        }
    };
    validate_package_record(&instance, session)?;

    let ready = live_session(&launch, &bound, &instance, uid, call.is_none()).and_then(|process| {
        let app = verified_session(session)?;
        if instance.package.as_ref() != Some(&PackageRef::of(app.require_verified()?)) {
            return Err(BrokerError::authorization("App runtime package changed"));
        }
        crate::provenance::runtime::assert_live_instance_now(uid, &session_id)
            .map_err(BrokerError::authorization)?;
        Ok((process, app))
    });
    let (process, app) = match ready {
        Ok(ready) => ready,
        Err(error) => {
            revoke_launch(launch.id, &session_id);
            sessions::rollback_transient_caps(uid, home, &session_id, None).await;
            return Err(error);
        }
    };
    let ceiling = sessions::app_ceiling(&app)?;
    let caps = match (call, offered) {
        (Some(original), Some(offered)) => {
            if bound.transient_caps.is_some() {
                return Err(BrokerError::authorization(
                    "App session already has an active call",
                ));
            }
            let delegation = task_host_delegation(&launcher, uid, &home, parent, &params)?;
            validate_call(&app, original, offered, &delegation)?;
            let plan =
                sessions::session_tool_plan(&app, &call_value(original)?, &delegation, &ceiling)?;
            Some(sessions::authorize_plan(
                &delegation,
                plan,
                &ceiling,
                session.app_id,
            )?)
        }
        (None, None) => None,
        _ => return Err(BrokerError::authorization("invalid retained session call")),
    };
    let mut effective = base_caps(session.app_id);
    if let Some(caps) = &caps {
        effective.extend(caps.iter().cloned());
    }
    let effective =
        sessions::clamp_to_ceiling(&ceiling, session.app_id, &effective, "task_session_call");
    // The control envelope owns the lifetime. Short call expiry must not
    // prevent narrowing back to base or become a new long-lived effect grant.
    let deadline = sampled + launch.expires_in;
    sessions::transient_transaction(
        uid,
        home,
        &session_id,
        caps,
        sessions::TransientRollback::Clear,
        || {
            rotate(
                &session_id,
                session,
                &launcher,
                &process,
                &instance,
                launch.id,
                &effective,
                &ceiling,
                deadline,
                call.is_some(),
            )
        },
        || revoke_launch(launch.id, &session_id),
    )
    .await
}

fn verified_session(session: &TaskHostSession<'_>) -> Result<App, BrokerError> {
    let app = sessions::installed_app(session.app_id)?;
    let package = app.require_verified()?;
    if package.id() != session.app_id
        || package.content_digest() != session.package_digest
        || package.kind() != crate::provenance::PackageKind::App
    {
        return Err(BrokerError::authorization(
            "verified package does not match the App session",
        ));
    }
    let declared = app
        .manifest
        .session
        .as_ref()
        .ok_or_else(|| BrokerError::authorization("App declares no stateful session"))?;
    if declared.transport != crate::caps::manifest::SessionTransport::Stdio
        || !package.ceiling().allows_mcp_attach()
        || !package.ceiling().allows_relay()
    {
        return Err(BrokerError::authorization(
            "App provenance does not allow a stdio session",
        ));
    }
    let entry = declared
        .entry
        .as_deref()
        .unwrap_or_else(|| app.manifest.runtime.default_session_entry());
    #[cfg(unix)]
    package
        .open_entrypoint(entry)
        .map_err(|error| BrokerError::authorization(format!("session entrypoint: {error}")))?;
    #[cfg(not(unix))]
    {
        let _ = entry;
        return Err(BrokerError::unavailable("App sessions require Unix"));
    }
    Ok(app)
}

fn base_caps(app_id: &str) -> CapSet {
    CapSet::from_caps([Cap::new(Verb::AGENT_INVOKE, Scope::name(app_id))])
}

fn call_value(call: &TaskHostSessionCall<'_>) -> Result<Value, BrokerError> {
    let value = json!({"tool":call.tool, "args":call.args});
    checked_call(&value)?;
    Ok(value)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OfferedCall {
    tool: String,
    #[serde(default)]
    args: BTreeMap<String, Value>,
}

fn checked_call(value: &Value) -> Result<OfferedCall, BrokerError> {
    serde_json::from_value::<crate::clawd::wire::bounded::Structured>(value.clone())
        .map_err(|_| BrokerError::execution("session call exceeds broker bounds"))?;
    serde_json::from_value(value.clone())
        .map_err(|_| BrokerError::execution("invalid session tool/arguments"))
}

fn validate_call(
    app: &App,
    original: &TaskHostSessionCall<'_>,
    offered: &Value,
    delegation: &Delegation,
) -> Result<(), BrokerError> {
    let offered_call = checked_call(offered)?;
    if offered_call.tool != original.tool {
        return Err(BrokerError::authorization(
            "session tool does not match the original call",
        ));
    }
    let (tool, expected) = sessions::session_tool_call(app, &call_value(original)?, delegation)?;
    let (_, actual) = sessions::session_tool_call(
        app,
        &json!({"tool":offered_call.tool, "args":offered_call.args}),
        delegation,
    )?;
    if expected.needs != actual.needs
        || expected.values.len() != actual.values.len()
        || tool.args.iter().any(|arg| {
            !equivalent_argument(
                arg,
                expected.values.get(&arg.name),
                actual.values.get(&arg.name),
            )
        })
    {
        return Err(BrokerError::authorization(
            "session arguments do not match the original call",
        ));
    }
    Ok(())
}

fn validate_assignment(
    launch: &authority::GrantView,
    bound: &SessionInfo,
    parent: &SessionInfo,
    session: &TaskHostSession<'_>,
    uid: u32,
) -> Result<(), BrokerError> {
    if launch.owner_uid != uid
        || launch.issuer != authority::Issuer::AppSessionAuthority
        || launch.parent.is_some()
        || launch.depth != 0
        || launch.subject.app_id.as_deref() != Some(session.app_id)
        || bound.app_id.as_deref() != Some(session.app_id)
        || bound.parent.as_deref() != Some(parent.session_id.as_str())
        || bound.command != vec![format!("cos app {} session", session.app_id)]
        || bound.caps.as_ref() != Some(&base_caps(session.app_id))
    {
        return Err(BrokerError::authorization(
            "App session does not match the trusted task context",
        ));
    }
    Ok(())
}

fn validate_package_record(
    instance: &Instance,
    session: &TaskHostSession<'_>,
) -> Result<(), BrokerError> {
    let package = instance
        .package
        .as_ref()
        .ok_or_else(|| BrokerError::authorization("session is not package-backed"))?;
    if instance.class != InstanceClass::App
        || package.kind != crate::provenance::PackageKind::App
        || package.id != session.app_id
        || package.content_digest != session.package_digest
    {
        return Err(BrokerError::authorization(
            "App session package does not match the task",
        ));
    }
    Ok(())
}

fn live_session(
    launch: &authority::GrantView,
    bound: &SessionInfo,
    instance: &Instance,
    uid: u32,
    clearing: bool,
) -> Result<ProcessIdentity, BrokerError> {
    let process = ProcessIdentity::of_process(uid, bound.pid)
        .ok_or_else(|| BrokerError::authorization("App child cannot be identified"))?;
    if bound.pending_bind
        || bound.pid <= 1
        || bound.start_time_ticks != process.start_time_ticks
        || instance.process.as_ref() != Some(&process)
        || !process.still_matches()
        || sessions::process_no_new_privs(bound.pid) != Some(true)
    {
        return Err(BrokerError::authorization(
            "App child no longer matches its root binding",
        ));
    }
    let mut presentation = authority::Presentation::new(
        uid,
        bound.pid,
        bound.start_time_ticks,
        authority::Audience::SystemService,
        "task_host.session_call",
    );
    presentation.session_id = Some(bound.session_id.clone());
    let mut effective = bound.caps.clone().unwrap_or_default();
    if let Some(caps) = &bound.transient_caps {
        effective.extend(caps.iter().cloned());
    }
    if launch.caps != effective {
        return Err(BrokerError::authorization(
            "App registry and control authority disagree",
        ));
    }
    let grant = match authority::authority().resolve_session(&bound.session_id, &presentation) {
        Ok(grant) => grant,
        // Possession was already rechecked against the exact live control
        // principal, task, package and process. This exception permits only
        // dropping all effect caps; it cannot start or renew a call.
        Err(authority::AuthorityError::UnknownGrant | authority::AuthorityError::Expired)
            if clearing =>
        {
            return Ok(process);
        }
        Err(error) => return Err(BrokerError::authorization(error.to_string())),
    };
    if grant.parent != Some(launch.id)
        || grant.issuer != authority::Issuer::AppSessionAuthority
        || grant.subject != launch.subject
        || grant.bound_pid != bound.pid
        || grant.owner_uid != uid
        || grant.caps != effective
    {
        return Err(BrokerError::authorization(
            "App registry and grant authority disagree",
        ));
    }
    Ok(process)
}

fn revoke_launch(id: authority::GrantId, session_id: &str) {
    let count = authority::authority().revoke(id);
    authority::audit::record_revoked("task-session-rotation", Some(session_id), count);
}

/// No replacement grant can survive a partial rotation, including unwinding.
struct RotationGuard {
    old: authority::GrantId,
    new: Option<authority::GrantId>,
    session_id: String,
    committed: bool,
}

impl Drop for RotationGuard {
    fn drop(&mut self) {
        if !self.committed {
            revoke_launch(self.old, &self.session_id);
            if let Some(id) = self.new {
                revoke_launch(id, &self.session_id);
            }
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration, BrokerError> {
    deadline
        .checked_duration_since(Instant::now())
        .and_then(|remaining| remaining.checked_sub(Duration::from_secs(1)))
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| BrokerError::authorization("App session authority is expiring"))
}

fn check_expiry(view: &authority::GrantView, deadline: Instant) -> Result<(), BrokerError> {
    if Instant::now() + view.expires_in > deadline {
        return Err(BrokerError::authorization(
            "rotated grant would extend its authorized lifetime",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rotate(
    session_id: &str,
    session: &TaskHostSession<'_>,
    launcher: &LauncherAuthority,
    process: &ProcessIdentity,
    instance: &Instance,
    old: authority::GrantId,
    effective: &CapSet,
    ceiling: &Ceiling,
    deadline: Instant,
    active: bool,
) -> Result<Value, BrokerError> {
    let call_deadline = Instant::now() + ACTIVE_CALL_WINDOW;
    let store = authority::authority();
    let mut guard = RotationGuard {
        old,
        new: None,
        session_id: session_id.to_string(),
        committed: false,
    };
    revoke_launch(old, session_id);
    let request = sessions::launch_issuance(
        session_id,
        Some(session.app_id),
        process.uid,
        launcher,
        effective,
        Some(ceiling),
        remaining(deadline)?,
    )?;
    let before_issue = Instant::now();
    let (handle, view) = store
        .issue(request)
        .map_err(|error| BrokerError::unavailable(error.to_string()))?;
    guard.new = Some(view.id);
    // Issuance accepts a relative lifetime. Check a conservative *upper*
    // bound afterwards, before exposing or deriving any replacement.
    check_expiry(&view, deadline)?;
    authority::audit::record_issued(&view, None);
    let control_deadline = before_issue + view.expires_in;
    let deadline = if active {
        control_deadline.min(call_deadline)
    } else {
        control_deadline
    };
    let handle = handle.into_wire();
    let request = sessions::session_attenuation(
        session_id,
        Some(session.app_id),
        process.uid,
        process.pid,
        effective,
        Some(ceiling),
        remaining(deadline)?,
    )?;
    let (_, child) = store
        .attenuate(&handle, request)
        .map_err(|error| BrokerError::unavailable(error.to_string()))?;
    check_expiry(&child, deadline)?;
    authority::audit::record_issued(&child, None);
    #[cfg(test)]
    fail_rotation_if_armed()?;
    let request = sessions::relay_attenuation(
        session_id,
        Some(session.app_id),
        process.uid,
        launcher.pid,
        remaining(deadline)?,
    )?;
    let (relay, relay_view) = store
        .attenuate(&handle, request)
        .map_err(|error| BrokerError::unavailable(error.to_string()))?;
    check_expiry(&relay_view, deadline)?;
    authority::audit::record_issued(&relay_view, None);
    if !process.still_matches()
        || sessions::process_uid(launcher.pid) != Some(process.uid)
        || crate::proc::read_start_time_ticks_pub(launcher.pid) != launcher.start_time_ticks
    {
        return Err(BrokerError::authorization(
            "App process identity changed during rotation",
        ));
    }
    let app = verified_session(session)?;
    if instance.package.as_ref() != Some(&PackageRef::of(app.require_verified()?))
        || crate::provenance::runtime::instance_for(process.uid, session_id)
            .map_err(BrokerError::unavailable)?
            .as_ref()
            != Some(instance)
    {
        return Err(BrokerError::authorization(
            "App runtime identity changed during rotation",
        ));
    }
    crate::provenance::runtime::assert_live_instance_now(process.uid, session_id)
        .map_err(BrokerError::authorization)?;
    guard.committed = true;
    Ok(json!({"updated":true, "handle":handle, "relay_handle":relay.into_wire()}))
}

#[cfg(test)]
thread_local! {
    static FAIL_ROTATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_rotation_if_armed() -> Result<(), BrokerError> {
    if FAIL_ROTATION.with(|flag| flag.replace(false)) {
        Err(BrokerError::unavailable(
            "injected session rotation failure",
        ))
    } else {
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_sessions/task_host/stateful.rs"
    ));
}
