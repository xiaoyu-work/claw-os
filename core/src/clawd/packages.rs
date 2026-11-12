use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;

use crate::caps::{Cap, Scope, Verb};
use crate::proc::SessionInfo;
use crate::session::{Mutation, MutationRecord, SessionId};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const PACKAGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

pub(crate) mod intent;
pub(crate) mod query;
#[cfg(target_os = "linux")]
pub(crate) mod gui;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageState {
    pub package: String,
    pub installed: bool,
    pub version: Option<String>,
    pub held: bool,
}

pub async fn install(
    state: &super::state::DaemonState,
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    let mut params = params;
    params["action"] = Value::String("install".to_string());
    control(state, params, client, authority).await
}

pub async fn control(
    state: &super::state::DaemonState,
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, params, authority);
        return Err("system package control requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("system package control requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let action = required_string(&params, "action")?;
        let package = optional_string(&params, "package")?;
        let version = optional_string(&params, "version")?;
        if action == "install-packages" {
            if package.is_some() || version.is_some() {
                return Err("install-packages does not accept a single package or version".into());
            }
            let packages: Vec<String> =
                serde_json::from_value(params.get("packages").cloned().unwrap_or(Value::Null))
                    .map_err(|_| "install-packages requires a bounded package list")?;
            let operation = super::package_transactions::Operation::install_packages(packages)?;
            let invocation = optional_string(&params, "invocation_id")?;
            return super::package_transactions::accept(
                state,
                client,
                authority,
                operation,
                invocation.as_deref(),
            )
            .await;
        }
        if params
            .get("packages")
            .is_some_and(|packages| !packages.is_null())
        {
            return Err("Only install-packages accepts a package list".into());
        }
        validate_action(&action, package.as_deref(), version.as_deref())?;

        if !matches!(action.as_str(), "update-index" | "hold" | "unhold") {
            let operation = super::package_transactions::Operation::from_action(
                &action,
                package.as_deref(),
                version.as_deref(),
            )?;
            let invocation = optional_string(&params, "invocation_id")?;
            return super::package_transactions::accept(
                state,
                client,
                authority,
                operation,
                invocation.as_deref(),
            )
            .await;
        }
        let requested_scope = if is_global_action(&action) {
            Scope::Wild
        } else {
            Scope::name(package.as_deref().unwrap_or_default())
        };
        let (session, authorized) = authorize_package_session(authority, requested_scope, uid)?;

        let _guard = super::app_store::apt::TRANSACTION.lock().await;
        let before = match package.as_deref() {
            Some(package) => Some(query_package_state(package).await?),
            None => None,
        };
        let rollback = if is_reversible_action(&action) {
            crate::paths::with_user_override(uid, home, async {
                prepare_rollback_record(
                    &session,
                    before
                        .as_ref()
                        .expect("reversible package action must have state"),
                )
            })
            .await?
        } else if action == "purge" {
            RollbackRecord::not_available(
                "Purge removes package configuration and requires a system snapshot for complete rollback.",
            )
        } else {
            RollbackRecord::not_available(
                "Global apt index and upgrade operations require a system snapshot for rollback.",
            )
        };
        let output = run_metadata_action(&authorized, &action, package.as_deref()).await?;
        let after = match package.as_deref() {
            Some(package) => match query_package_state(package).await {
                Ok(state) => Some(state),
                Err(error) => {
                    return Err(format!("Package metadata outcome is unconfirmed: {error}; do not replay automatically"));
                }
            },
            None => None,
        };

        Ok(json!({
            "action": action,
            "package": package,
            "version": version,
            "changed": before != after,
            "before": before,
            "after": after,
            "stdout_tail": output.stdout_tail,
            "stderr_tail": output.stderr_tail,
            "rollback": rollback,
        }))
    }
}

pub async fn restore(
    state: &super::state::DaemonState,
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (state, params, client, authority);
        return Err("system package restore requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("system package restore requires root clawd".to_string());
        }
        client.require_uid()?;
        let mutation_session = required_string(&params, "mutation_session")?;
        let mutation_seq = required_u64(&params, "mutation_seq")?;
        let package = required_string(&params, "package")?;
        validate_package_name(&package)?;
        let previous_version = optional_string(&params, "previous_version")?;
        if let Some(version) = previous_version.as_deref() {
            validate_version(version)?;
        }
        let was_held = required_bool(&params, "was_held")?;

        validate_restore_record(
            &mutation_session,
            mutation_seq,
            &package,
            previous_version.as_deref(),
            was_held,
        )?;

        let invocation = optional_string(&params, "invocation_id")?;
        super::package_transactions::accept(
            state,
            client,
            authority,
            super::package_transactions::Operation::Restore {
                package,
                previous_version,
                was_held,
                mutation_session,
                mutation_seq,
            },
            invocation.as_deref(),
        )
        .await
    }
}

pub fn restore_package_state(
    mutation_session: &SessionId,
    mutation_seq: u64,
    package: &str,
    previous_version: Option<&str>,
    was_held: bool,
) -> Result<Value, String> {
    validate_package_name(package)?;
    if let Some(version) = previous_version {
        validate_version(version)?;
    }
    #[cfg(target_os = "linux")]
    {
        let session = std::env::var("COS_SESSION")
            .map_err(|_| "package rollback requires COS_SESSION".to_string())?;
        let response = crate::clawd::client::request_blocking(
            crate::paths::clawd_socket_path(),
            crate::clawd::protocol::Request::build(
                crate::clawd::routes::Command::SystemPackageRestore,
                json!({
                    "session": session,
                    "mutation_session": mutation_session.as_str(),
                    "mutation_seq": mutation_seq,
                    "package": package,
                    "previous_version": previous_version,
                    "was_held": was_held,
                    "invocation_id": crate::crypto::sha256_hex(
                        format!("package-inverse:{}:{mutation_seq}", mutation_session.as_str()).as_bytes()
                    )[..32].to_string(),
                }),
            ),
        )?;
        if response.ok {
            let receipt = response
                .result
                .ok_or("Root package acceptance receipt is missing")?;
            require_acceptance(&receipt)?;
            Ok(receipt)
        } else {
            Err(response
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "clawd package restore failed".to_string()))
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (mutation_session, mutation_seq, previous_version, was_held);
        Err("system package restore requires Linux".to_string())
    }
}

pub(crate) fn inverse_completion(
    session: &SessionId,
    seq: u64,
    package: &str,
    version: Option<&str>,
    held: bool,
) -> Result<Option<bool>, String> {
    let invocation =
        crate::crypto::sha256_hex(format!("package-inverse:{}:{seq}", session.as_str()).as_bytes());
    let response = super::client::request_blocking(
        crate::paths::clawd_socket_path(),
        super::protocol::Request::build(
            super::routes::Command::SystemPackageStatus,
            json!({"invocation_id": &invocation[..32]}),
        ),
    )?;
    if !response.ok {
        return Err("Root package inverse completion could not be authenticated".into());
    }
    let result = response.result.ok_or("Missing Root package status")?;
    let expected = json!({
        "action": "restore", "package": package, "previous_version": version,
        "was_held": held, "mutation_session": session.as_str(), "mutation_seq": seq,
    });
    let records = result
        .get("transactions")
        .and_then(Value::as_array)
        .ok_or("Malformed Root package status")?;
    let matching = records
        .iter()
        .filter(|record| record.get("operation") == Some(&expected))
        .collect::<Vec<_>>();
    if matching.iter().any(|record| {
        record.get("phase").and_then(Value::as_str) == Some("applied")
            && record
                .get("completion")
                .and_then(|value| value.get("confirmed"))
                .and_then(Value::as_bool)
                == Some(true)
    }) {
        return Ok(Some(true));
    }
    if !matching.is_empty()
        && matching
            .iter()
            .all(|record| record.get("phase").and_then(Value::as_str) == Some("not_applied"))
    {
        return Ok(Some(false));
    }
    Ok(None)
}

pub(crate) fn inverse_completion_in_process(
    client: &ClientIdentity,
    session: &SessionId,
    seq: u64,
    package: &str,
    version: Option<&str>,
    held: bool,
) -> Result<Option<bool>, String> {
    #[cfg(target_os = "linux")]
    {
        super::package_transactions::inverse_completion(
            client, session, seq, package, version, held,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (client, session, seq, package, version, held);
        Err("Package rollback requires Linux".into())
    }
}

fn require_acceptance(receipt: &Value) -> Result<(), String> {
    let id = receipt
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if receipt.get("status").and_then(Value::as_str) != Some("accepted")
        || receipt.get("accepted").and_then(Value::as_bool) != Some(true)
        || id.len() != 32
        || !id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("The Root package handoff protocol was not confirmed; do not replay".into());
    }
    Ok(())
}

pub(crate) fn request_control(mut params: Value) -> Result<Value, String> {
    let payload = !matches!(
        params.get("action").and_then(Value::as_str),
        Some("update-index" | "hold" | "unhold")
    );
    let invocation = uuid::Uuid::new_v4().simple().to_string();
    if payload {
        params["invocation_id"] = json!(invocation);
    }
    let response = super::client::request_blocking(
        crate::paths::clawd_socket_path(),
        super::protocol::Request::build(super::routes::Command::SystemPackageControl, params),
    ).map_err(|error| {
        if payload && error.may_have_dispatched() {
            format!("Root package acceptance is unconfirmed: {error}. Query `cos package-transactions --invocation {invocation}`; do not resubmit effects")
        } else { error.to_string() }
    })?;
    if !response.ok {
        return Err(response
            .error
            .map(|error| error.message)
            .unwrap_or_else(|| "Root package request failed".into()));
    }
    let result = response.result.ok_or("Root package response is missing")?;
    if payload {
        require_acceptance(&result)?;
    }
    Ok(result)
}

pub(crate) async fn rollback_authority(client: &ClientIdentity) -> Result<Decision, String> {
    let route = super::routes::Command::SystemPackageRestore.route();
    super::authority::authorize(route.name, &route.authority, &json!({}), client)
        .await
        .map_err(|fault| fault.message().to_string())?
        .ok_or_else(|| "Package rollback requires the authenticated caller's live session".into())
}

pub(crate) fn restore_in_process(
    state: &super::state::DaemonState,
    client: &ClientIdentity,
    authority: &Decision,
    mutation_session: &SessionId,
    mutation_seq: u64,
    package: &str,
    previous_version: Option<&str>,
    was_held: bool,
) -> Result<Value, String> {
    #[cfg(target_os = "linux")]
    {
        validate_restore_record(
            mutation_session.as_str(),
            mutation_seq,
            package,
            previous_version,
            was_held,
        )?;
        super::package_transactions::accept_inverse(
            state,
            client,
            authority,
            super::package_transactions::Operation::Restore {
                package: package.into(),
                previous_version: previous_version.map(str::to_string),
                was_held,
                mutation_session: mutation_session.as_str().into(),
                mutation_seq,
            },
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            state,
            client,
            authority,
            mutation_session,
            mutation_seq,
            package,
            previous_version,
            was_held,
        );
        Err("Package rollback requires Linux".into())
    }
}

pub async fn transaction_status(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, super::protocol::BrokerError> {
    #[cfg(target_os = "linux")]
    {
        super::package_transactions::status(params, client).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        Err(super::protocol::BrokerError::unavailable(
            "Package transactions require Linux",
        ))
    }
}

pub async fn transaction_recover(
    params: Value,
    client: &ClientIdentity,
) -> Result<Value, super::protocol::BrokerError> {
    #[cfg(target_os = "linux")]
    {
        super::package_transactions::recover(params, client).await
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        Err(super::protocol::BrokerError::unavailable(
            "Package transactions require Linux",
        ))
    }
}

/// Final provider check, taken against the decision the broker already
/// made.
///
/// The session, its App identity, the process allowed to act under it,
/// and the capabilities it holds all come from the grant `clawd`
/// issued and the middleware resolved. Nothing here re-reads the
/// process registry or re-derives policy, so the two can no longer
/// disagree; the check still runs, because a privileged mutation
/// should be refused twice.
fn authorize_package_session(
    authority: &Decision,
    scope: Scope,
    owner_uid: u32,
) -> Result<(SessionInfo, Authorized), String> {
    if authority.owner_uid() != owner_uid {
        return Err("package authority belongs to another owner".into());
    }
    let session = authority.session()?.clone();
    let proof = authority.require(Cap::new(Verb::SYS_PACKAGE, scope))?;
    Ok((session, proof))
}

pub(crate) async fn query_package_state(package: &str) -> Result<PackageState, String> {
    let status = query_package_status(package).await?;
    let installed = status
        .as_ref()
        .is_some_and(|(status, _, _)| status.ends_with(" ok installed"));
    let version = status
        .as_ref()
        .filter(|_| installed)
        .map(|(_, version, _)| version.clone());
    let holds = run_command(apt_mark_path(), &["showhold"]).await?;
    let native = native_architecture().await?;
    let base = package.split(':').next().unwrap_or(package);
    let arch = status
        .as_ref()
        .map(|(_, _, arch)| arch.as_str())
        .or_else(|| package.split_once(':').map(|(_, arch)| arch))
        .unwrap_or(&native);
    let held = holds.stdout.lines().any(|line| {
        let line = line.trim();
        line == package
            || line == format!("{base}:{arch}")
            || (arch == "all" || arch == native) && line == base
    });
    Ok(PackageState {
        package: package.to_string(),
        installed,
        version,
        held,
    })
}

pub(crate) async fn native_architecture() -> Result<String, String> {
    let output = run_command("/usr/bin/dpkg", &["--print-architecture"]).await?;
    let architecture = output.stdout.trim();
    validate_package_name(&format!("fixture:{architecture}"))?;
    Ok(architecture.into())
}

fn package_absent(output: &CommandOutput) -> bool {
    output.status == 1
        && output.stdout.is_empty()
        && output
            .stderr
            .starts_with("dpkg-query: no packages found matching ")
}

pub(crate) async fn query_package_status(
    package: &str,
) -> Result<Option<(String, String, String)>, String> {
    validate_package_name(package)?;
    let base = package.split(':').next().unwrap_or(package);
    let output = run_command_allow_failure(
        dpkg_query_path(),
        &[
            "--show",
            "--showformat=${Status}\t${Version}\t${Architecture}\n",
            "--",
            base,
        ],
    )
    .await?;
    if package_absent(&output) {
        return Ok(None);
    }
    if output.status != 0 || output.stdout.len() > 64 * 1024 {
        return Err("The concrete installed package state could not be read".into());
    }
    let preferred = match package.split_once(':') {
        Some((_, "native")) => native_architecture().await?,
        Some((_, arch)) => arch.to_string(),
        None => native_architecture().await?,
    };
    select_package_status(&output.stdout, &preferred)
}

fn select_package_status(
    text: &str,
    preferred: &str,
) -> Result<Option<(String, String, String)>, String> {
    if text.is_empty() {
        return Err("The concrete installed package state is empty, not confirmed absent".into());
    }
    let mut selected = None;
    for line in text.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 3 {
            return Err("The concrete installed package state is malformed".into());
        }
        validate_package_name(&format!("fixture:{}", fields[2]))?;
        if fields[2] == preferred || fields[2] == "all" {
            if selected.is_some() {
                return Err("The concrete installed package architecture is ambiguous".into());
            }
            selected = Some((fields[0].into(), fields[1].into(), fields[2].into()));
        }
    }
    Ok(selected)
}

pub(crate) async fn query_package_owners(path: &std::path::Path) -> Result<Vec<String>, String> {
    let path = path.to_str().ok_or("Package-owned path must be UTF-8")?;
    let output = run_command_allow_failure(dpkg_query_path(), &["--search", "--", path]).await?;
    if output.status == 1
        && output.stdout.is_empty()
        && output
            .stderr
            .starts_with("dpkg-query: no path found matching pattern ")
    {
        return Ok(Vec::new());
    }
    if output.status != 0 || output.stdout.len() > 64 * 1024 {
        return Err("Installed App package ownership could not be read".into());
    }
    let mut owners = Vec::new();
    for line in output.stdout.lines() {
        let (names, _) = line
            .rsplit_once(": ")
            .ok_or("Invalid installed App ownership result")?;
        for owner in names.split(", ") {
            validate_package_name(owner)?;
            owners.push(owner.into());
        }
    }
    Ok(owners)
}

async fn run_metadata_action(
    _authorized: &Authorized,
    action: &str,
    package: Option<&str>,
) -> Result<CommandOutput, String> {
    match action {
        "update-index" => run_command(apt_get_path(), &["update"]).await,
        "hold" => run_command(apt_mark_path(), &["hold", "--", package.unwrap()]).await,
        "unhold" => run_command(apt_mark_path(), &["unhold", "--", package.unwrap()]).await,
        _ => Err(format!("unsupported package action: {action}")),
    }
}

pub(crate) fn prepare_rollback_record(
    session: &SessionInfo,
    before: &PackageState,
) -> Result<RollbackRecord, String> {
    let Some(parent) = session.parent.as_deref() else {
        return Ok(RollbackRecord::not_available(
            "App session has no durable parent task.",
        ));
    };
    let Ok(parent_id) = parent.parse::<SessionId>() else {
        return Ok(RollbackRecord::not_available(
            "Parent session is not a durable task id.",
        ));
    };
    if !crate::session::session_dir(&parent_id).is_dir() {
        return Ok(RollbackRecord::not_available(
            "Parent task has no mutation store.",
        ));
    }
    let seq = crate::session::record_mutation(
        &parent_id,
        MutationRecord::new(Mutation::SystemPackage {
            package: before.package.clone(),
            previous_version: before.version.clone(),
            was_held: before.held,
        })
        .with_runtime("clawd-packages"),
    )
    .map_err(|error| format!("record package rollback metadata: {error}"))?;
    Ok(RollbackRecord {
        available: true,
        mutation_seq: Some(seq),
        note: "Previous installed version and hold state recorded on the parent task.".to_string(),
    })
}

fn validate_restore_record(
    mutation_session: &str,
    mutation_seq: u64,
    package: &str,
    previous_version: Option<&str>,
    was_held: bool,
) -> Result<(), String> {
    let session_id = mutation_session
        .parse::<SessionId>()
        .map_err(|error| format!("invalid rollback session id: {error}"))?;
    let mutations = crate::session::iter_mutations(&session_id)
        .map_err(|error| format!("read rollback mutation: {error}"))?;
    let mutation = mutations
        .into_iter()
        .find(|record| record.seq == mutation_seq)
        .ok_or_else(|| {
            format!(
                "rollback mutation seq {mutation_seq} not found in {}",
                session_id.as_str()
            )
        })?;
    match mutation.mutation {
        Mutation::SystemPackage {
            package: recorded_package,
            previous_version: recorded_version,
            was_held: recorded_held,
        } if recorded_package == package
            && recorded_version.as_deref() == previous_version
            && recorded_held == was_held =>
        {
            Ok(())
        }
        Mutation::SystemPackage { .. } => {
            Err("requested package restore does not match the recorded inverse state".to_string())
        }
        _ => Err("rollback mutation is not a system package change".to_string()),
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RollbackRecord {
    available: bool,
    mutation_seq: Option<u64>,
    note: String,
}

impl RollbackRecord {
    fn not_available(note: &str) -> Self {
        Self {
            available: false,
            mutation_seq: None,
            note: note.to_string(),
        }
    }
}

struct CommandOutput {
    status: i32,
    stdout: String,
    stderr: String,
    stdout_tail: String,
    stderr_tail: String,
}

async fn run_command(path: &str, args: &[&str]) -> Result<CommandOutput, String> {
    run_command_owned(
        path,
        args.iter().map(|value| (*value).to_string()).collect(),
    )
    .await
}

async fn run_command_owned(path: &str, args: Vec<String>) -> Result<CommandOutput, String> {
    let mut process = command(path, &args);
    let output = tokio::time::timeout(PACKAGE_TIMEOUT, process.output())
        .await
        .map_err(|_| format!("{} timed out after {}s", path, PACKAGE_TIMEOUT.as_secs()))?
        .map_err(|error| format!("failed to launch {}: {error}", path))?;
    command_output(path, &args, output, false)
}

async fn run_command_allow_failure(path: &str, args: &[&str]) -> Result<CommandOutput, String> {
    let args = args
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    let mut process = command(path, &args);
    let output = tokio::time::timeout(PACKAGE_TIMEOUT, process.output())
        .await
        .map_err(|_| format!("{} timed out after {}s", path, PACKAGE_TIMEOUT.as_secs()))?
        .map_err(|error| format!("failed to launch {}: {error}", path))?;
    command_output(path, &args, output, true)
}

fn command(path: &str, args: &[String]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(path);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("APT_LISTCHANGES_FRONTEND", "none")
        .env("LC_ALL", "C.UTF-8")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn command_output(
    path: &str,
    args: &[String],
    output: std::process::Output,
    allow_failure: bool,
) -> Result<CommandOutput, String> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let status = output.status.code().unwrap_or(-1);
    if !allow_failure && !output.status.success() {
        return Err(format!(
            "{} {} exited {status}: {}",
            path,
            args.join(" "),
            tail(&stderr)
        ));
    }
    Ok(CommandOutput {
        status,
        stdout_tail: tail(&stdout),
        stderr_tail: tail(&stderr),
        stdout,
        stderr,
    })
}

fn is_global_action(action: &str) -> bool {
    matches!(action, "update-index" | "upgrade-all")
}

fn is_reversible_action(action: &str) -> bool {
    matches!(
        action,
        "install" | "install-version" | "remove" | "upgrade" | "hold" | "unhold"
    )
}

fn validate_action(
    action: &str,
    package: Option<&str>,
    version: Option<&str>,
) -> Result<(), String> {
    if !matches!(
        action,
        "install"
            | "install-version"
            | "remove"
            | "purge"
            | "upgrade"
            | "update-index"
            | "upgrade-all"
            | "hold"
            | "unhold"
    ) {
        return Err(format!("unsupported package action: {action}"));
    }
    if is_global_action(action) {
        if package.is_some() || version.is_some() {
            return Err(format!("{action} does not accept a package or version"));
        }
        return Ok(());
    }
    let package = package.ok_or_else(|| format!("{action} requires a package"))?;
    validate_package_name(package)?;
    if action == "install-version" {
        validate_version(version.ok_or_else(|| "install-version requires a version".to_string())?)?;
    } else if version.is_some() {
        return Err(format!("{action} does not accept a version"));
    }
    Ok(())
}

pub(crate) fn validate_package_name(package: &str) -> Result<(), String> {
    if package.is_empty() || package.len() > 255 || package.starts_with('-') {
        return Err(format!("invalid Debian package name: {package:?}"));
    }
    let (name, architecture) = package
        .split_once(':')
        .map(|(name, arch)| (name, Some(arch)))
        .unwrap_or((package, None));
    if !valid_name_component(name, true)
        || architecture.is_some_and(|arch| !valid_name_component(arch, false))
    {
        return Err(format!("invalid Debian package name: {package:?}"));
    }
    Ok(())
}

pub(crate) fn validate_version(version: &str) -> Result<(), String> {
    if version.is_empty()
        || version.len() > 255
        || version.starts_with('-')
        || !version.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b':' | b'+' | b'~' | b'-')
        })
    {
        return Err(format!("invalid Debian package version: {version:?}"));
    }
    Ok(())
}

fn valid_name_component(value: &str, allow_plus_dot: bool) -> bool {
    !value.is_empty()
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && (byte == b'-' || (allow_plus_dot && matches!(byte, b'+' | b'.'))))
        })
}

fn tail(text: &str) -> String {
    const MAX: usize = 8 * 1024;
    let start = text.len().saturating_sub(MAX);
    text.get(start..).unwrap_or(text).trim().to_string()
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key)?.ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            let value = value.trim();
            if value.is_empty() {
                Ok(None)
            } else {
                Ok(Some(value.to_string()))
            }
        }
        Some(_) => Err(format!("parameter `{key}` must be a string or null")),
    }
}

fn required_bool(params: &Value, key: &str) -> Result<bool, String> {
    params
        .get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("missing required boolean parameter: {key}"))
}

fn required_u64(params: &Value, key: &str) -> Result<u64, String> {
    params
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("missing required unsigned integer parameter: {key}"))
}

fn apt_get_path() -> &'static str {
    "/usr/bin/apt-get"
}

fn apt_mark_path() -> &'static str {
    "/usr/bin/apt-mark"
}

fn dpkg_query_path() -> &'static str {
    "/usr/bin/dpkg-query"
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/packages.rs"
    ));
}
