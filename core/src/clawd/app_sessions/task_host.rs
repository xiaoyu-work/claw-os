//! Private authority for a root-supervised App host.
//!
//! No route points here. The controller authenticates the task's signed
//! grant and live lease, selects its owner and parent session from root
//! state, and supplies the kernel-observed host identity. Neither the
//! isolated Agent nor the host may choose that context.

use super::{
    operation_call, register_with_launcher, required_string, App, BrokerError, ClientIdentity,
    Delegation, LaunchKind, LauncherAuthority, SessionInfo, Value,
};

mod stateful;
pub(crate) use stateful::{
    prepare_session_call_for_task_host, prepare_session_for_task_host,
    register_session_for_task_host, set_session_call_for_task_host, TaskHostSession,
    TaskHostSessionCall,
};

/// The original invocation admitted by the root task controller.
///
/// In particular, `args` must not be copied from registration parameters:
/// the host reports its canonical/default-expanded arguments separately.
/// Relative paths use the trusted parent's `workdir`, or the owner's passwd
/// home when none is set; changing the host's cwd must not change a target.
pub(crate) struct TaskHostInvocation<'a> {
    pub app_id: &'a str,
    pub operation: &'a str,
    pub args: &'a [String],
    pub package_digest: &'a str,
}

/// Prepare the assigned invocation for the ordinary unprivileged App bridge.
///
/// This verifies identity and metadata and resolves arguments, but authorizes
/// nothing: no approvals, grants, sessions, runtime records or App execution.
/// The controller must retain the original invocation unchanged and compare
/// later registration against it, not promote these returned arguments into a
/// replacement assignment. Expansion must fit registration's wire bounds and
/// round-trip without changing argument meanings or derived needs. Runtime
/// selectors must already be explicit.
pub(crate) fn prepare_for_task_host(
    client: &ClientIdentity,
    parent: &SessionInfo,
    invocation: &TaskHostInvocation<'_>,
) -> Result<Vec<String>, BrokerError> {
    let launcher = authenticate_task_host(client, parent)?;
    let uid = client.require_uid().map_err(BrokerError::authorization)?;
    let home = client
        .require_home_dir()
        .map_err(BrokerError::authorization)?;
    let delegation = task_host_delegation(&launcher, uid, &home, parent, &Value::Null)?;
    let app = super::installed_app(invocation.app_id)?;
    invocation.validate_package(&app)?;
    let (declared, effective) =
        operation_call(&app, invocation.operation, invocation.args, &delegation)?;
    require_explicit_resolvers(declared, &effective)?;
    let args = canonical_args(declared, &effective, invocation.args)?;
    invocation.validate_registration(&serde_json::json!({
        "app_id": invocation.app_id,
        "kind": "operation",
        "operation": invocation.operation,
        "args": args,
    }))?;
    invocation.validate_args(&app, &args, &delegation)?;
    Ok(args)
}

/// Register one assigned operation using the ordinary App policy path.
///
/// Root-controller lifecycle contract:
///
/// * Keep the verified package snapshot whose digest matches the invocation.
///   Before releasing registration, call `provenance::runtime::register` with
///   the authenticated owner uid and returned session id, not root's uid.
/// * Before bind, recheck the retained snapshot with `assert_current`. Bind
///   the child through the ordinary App-session handler, then record its
///   kernel-observed pid with `provenance::runtime::bind_process`. Both runtime
///   writers only warn on failure: read `instance_for` back and require the
///   exact `PackageRef`, App class, uid, pid and start time. A successful
///   `assert_live_instance_now` checks package trust, not process binding.
///   No relay may pass until all these checks succeed.
/// * On failure, exit or cancellation, stop admitting control calls and wait
///   for admitted mutations. Revoke only recorded, controller-owned sessions
///   with `authority::revoke_session_for_owner`. Stop any still-live bound
///   process group while its recorded identity is available (the runtime's
///   `mark_for_shutdown` / `terminate` path rechecks pid reuse), then remove
///   the owner-scoped proc and runtime records. Never retire the parent
///   approval identity or accept an arbitrary host-supplied session to clean up.
///
/// This function does not authenticate a task grant, supervise a process, or
/// install a runtime record. Runtime-selected arguments absent from the
/// original invocation fail closed; only equivalent canonicalization and
/// manifest defaults are accepted.
pub(crate) async fn register_for_task_host(
    params: Value,
    client: &ClientIdentity,
    parent: &SessionInfo,
    invocation: &TaskHostInvocation<'_>,
) -> Result<Value, BrokerError> {
    invocation.validate_registration(&params)?;
    let launcher = authenticate_task_host(client, parent)?;
    let uid = client.require_uid().map_err(BrokerError::authorization)?;
    let home = client
        .require_home_dir()
        .map_err(BrokerError::authorization)?;
    let delegation = task_host_delegation(&launcher, uid, &home, parent, &params)?;
    let app = super::installed_app(invocation.app_id)?;
    register_with_launcher(
        params,
        home,
        app,
        LaunchKind::Operation,
        launcher,
        delegation,
        Some(invocation),
    )
    .await
}

fn task_host_delegation(
    launcher: &LauncherAuthority,
    uid: u32,
    home: &std::path::Path,
    parent: &SessionInfo,
    params: &Value,
) -> Result<Delegation, BrokerError> {
    let cwd = parent
        .workdir
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.to_path_buf());
    if !cwd.is_absolute() {
        return Err(BrokerError::authorization(
            "task parent working directory must be absolute",
        ));
    }
    let mut delegation = Delegation::new(launcher, uid, home, params)?;
    delegation.paths.cwd = Some(cwd);
    Ok(delegation)
}

impl TaskHostInvocation<'_> {
    fn validate_registration(&self, params: &Value) -> Result<(), BrokerError> {
        // Reuse the closed, bounded body even for this non-socket entry point.
        serde_json::from_value::<crate::clawd::wire::requests::AppSessionRegister>(params.clone())
            .map_err(|_| BrokerError::execution("invalid task-host App registration"))?;
        if required_string(params, "kind")? != "operation" {
            return Err(BrokerError::authorization(
                "task hosts may register only one-shot App operations",
            ));
        }
        if required_string(params, "app_id")? != self.app_id
            || required_string(params, "operation")? != self.operation
        {
            return Err(BrokerError::authorization(
                "App registration does not match the assigned invocation",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_package(&self, app: &App) -> Result<(), BrokerError> {
        let package = app.require_verified().map_err(BrokerError::authorization)?;
        if package.id() != self.app_id
            || app.manifest.id != self.app_id
            || package.content_digest() != self.package_digest
        {
            return Err(BrokerError::authorization(
                "verified App package does not match the assigned invocation",
            ));
        }
        if app
            .manifest
            .desktop
            .as_ref()
            .is_some_and(|desktop| desktop.exec == self.operation)
        {
            return Err(BrokerError::authorization(
                "task hosts may not register an App desktop entrypoint",
            ));
        }
        Ok(())
    }

    pub(super) fn validate_args(
        &self,
        app: &App,
        reported: &[String],
        delegation: &Delegation,
    ) -> Result<(), BrokerError> {
        let (declared, assigned) = operation_call(app, self.operation, self.args, delegation)?;
        require_explicit_resolvers(declared, &assigned)?;
        let (_, reported) = operation_call(app, self.operation, reported, delegation)?;
        if assigned.needs != reported.needs
            || assigned.values.len() != reported.values.len()
            || declared.args.iter().any(|argument| {
                !equivalent_argument(
                    argument,
                    assigned.values.get(&argument.name),
                    reported.values.get(&argument.name),
                )
            })
        {
            return Err(BrokerError::authorization(
                "App arguments do not match the assigned invocation",
            ));
        }
        Ok(())
    }
}

fn equivalent_argument(
    argument: &crate::caps::manifest::Arg,
    assigned: Option<&Value>,
    reported: Option<&Value>,
) -> bool {
    if assigned == reported {
        return true;
    }
    if argument.kind != crate::caps::manifest::ArgKind::Number {
        return false;
    }
    // Number CLI tokens bind as f64, while a literal JSON default may
    // be an integer. Normalize only that declared kind, never integers,
    // text or scopes; the caller also compares the derived needs.
    let same_number = |assigned: &Value, reported: &Value| {
        assigned
            .as_f64()
            .zip(reported.as_f64())
            .is_some_and(|(assigned, reported)| assigned == reported)
    };
    match (assigned, reported) {
        (Some(Value::Array(assigned)), Some(Value::Array(reported))) if argument.repeatable => {
            assigned.len() == reported.len()
                && assigned
                    .iter()
                    .zip(reported)
                    .all(|(assigned, reported)| same_number(assigned, reported))
        }
        (Some(assigned), Some(reported)) if !argument.repeatable => same_number(assigned, reported),
        _ => false,
    }
}

fn require_explicit_resolvers(
    operation: &crate::caps::manifest::Operation,
    effective: &crate::caps::manifest::EffectiveCall,
) -> Result<(), BrokerError> {
    if let Some(argument) = operation.args.iter().find(|argument| {
        argument.trusted_resolver.is_some() && !effective.values.contains_key(&argument.name)
    }) {
        return Err(BrokerError::execution(format!(
            "runtime-selected argument `{}` must be explicit in the original task invocation",
            argument.name
        )));
    }
    Ok(())
}

fn canonical_args(
    operation: &crate::caps::manifest::Operation,
    effective: &crate::caps::manifest::EffectiveCall,
    raw: &[String],
) -> Result<Vec<String>, BrokerError> {
    use crate::caps::manifest::{ArgBinding, ArgKind};

    let supplied = crate::caps::args::bind_supplied_cli_args(&operation.args, raw)?;
    let mut positionals = Vec::new();
    let mut flags = Vec::new();
    for argument in &operation.args {
        let Some(value) = effective.values.get(&argument.name) else {
            continue;
        };
        if argument.effective_binding() == ArgBinding::Positional {
            if supplied.contains_key(&argument.name) || effective.defaulted.contains(&argument.name)
            {
                positionals.extend(argument_tokens(argument, value)?);
            }
            continue;
        }
        let flag = format!("--{}", crate::caps::args::flag_name(argument));
        if argument.kind == ArgKind::Bool {
            match value.as_bool() {
                Some(true) => flags.push(flag),
                Some(false) if supplied.contains_key(&argument.name) => {
                    flags.push(format!("{flag}=false"));
                }
                Some(false) => {}
                None => return Err(BrokerError::execution("bound boolean argument is invalid")),
            }
        } else {
            for token in argument_tokens(argument, value)? {
                if token.starts_with("--") {
                    flags.push(format!("{flag}={token}"));
                } else {
                    flags.push(flag.clone());
                    flags.push(token);
                }
            }
        }
    }
    // Keep option-looking App data out of the bridge's option parser,
    // including single-dash aliases declared by the manifest.
    if positionals.iter().any(|value| value.starts_with('-')) {
        flags.push("--".to_string());
        flags.extend(positionals);
        Ok(flags)
    } else {
        positionals.extend(flags);
        Ok(positionals)
    }
}

fn argument_tokens(
    argument: &crate::caps::manifest::Arg,
    value: &Value,
) -> Result<Vec<String>, BrokerError> {
    let values = if argument.repeatable {
        value
            .as_array()
            .map(Vec::as_slice)
            .ok_or_else(|| BrokerError::execution("bound repeatable argument is invalid"))?
    } else {
        std::slice::from_ref(value)
    };
    values
        .iter()
        .map(|value| match value {
            Value::String(value) => Ok(value.clone()),
            Value::Number(value) => Ok(value.to_string()),
            Value::Bool(value) => Ok(value.to_string()),
            _ => Err(BrokerError::execution("bound argument is not a scalar")),
        })
        .collect()
}

fn authenticate_task_host(
    client: &ClientIdentity,
    parent: &SessionInfo,
) -> Result<LauncherAuthority, BrokerError> {
    use crate::clawd::transport::{peer, Credentials};
    use crate::clawd::wire::bounded::Token;

    let uid = client.require_uid().map_err(BrokerError::authorization)?;
    if uid == 0 {
        return Err(BrokerError::authorization(
            "a task App host must run as a non-root owner",
        ));
    }
    let pid = client
        .pid
        .filter(|pid| *pid > 1)
        .ok_or_else(|| BrokerError::authorization("task App host pid is unavailable"))?;
    let gid = client
        .gid
        .ok_or_else(|| BrokerError::authorization("task App host gid is unavailable"))?;
    let expected_start = client
        .start_time_ticks
        .ok_or_else(|| BrokerError::authorization("task App host start time is unavailable"))?;
    let process = peer::verify(Credentials { pid, uid, gid })
        .ok_or_else(|| BrokerError::authorization("task App host identity no longer matches"))?;
    if process.start_time_ticks != expected_start
        || super::process_no_new_privs(pid) != Some(true)
        || !crate::provenance::runtime::ProcessIdentity::of_process(uid, pid).is_some_and(
            |identity| {
                identity.start_time_ticks == Some(expected_start) && identity.still_matches()
            },
        )
    {
        return Err(BrokerError::authorization(
            "task App host must be live, identity-bound, and NoNewPrivs",
        ));
    }

    let session_id = Token::<128>::parse(&parent.session_id)
        .map_err(|_| BrokerError::authorization("task parent session id is unusable"))?;
    if session_id.as_str().is_empty()
        || session_id.as_str() != parent.session_id
        || parent.app_id.is_some()
        || parent.pending_bind
        || matches!(parent.group.as_deref(), Some("app" | "mcp"))
        || parent.ended_at.is_some()
        || parent.exit_code.is_some()
    {
        return Err(BrokerError::authorization(
            "task parent must be an active non-App session",
        ));
    }
    let caps = parent
        .caps
        .as_ref()
        .filter(|caps| !caps.is_empty())
        .ok_or_else(|| BrokerError::authorization("task parent has no capabilities"))?;
    Ok(LauncherAuthority {
        pid,
        start_time_ticks: Some(expected_start),
        parent: Some(parent.session_id.clone()),
        caps: caps.clone(),
        tier: parent.tier,
        scope: parent.scope.clone(),
        priority: parent.priority.clone(),
        role: parent.role.clone(),
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/app_sessions/task_host.rs"
    ));
}
