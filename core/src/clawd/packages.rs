use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::proc::SessionInfo;
use crate::session::{Mutation, MutationRecord, SessionId};

use super::client_identity::ClientIdentity;

const PACKAGE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
static APT_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageState {
    pub package: String,
    pub installed: bool,
    pub version: Option<String>,
    pub held: bool,
}

pub async fn install(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    let mut params = params;
    params["action"] = Value::String("install".to_string());
    control(params, client).await
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("system package control requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("system package control requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let package = optional_string(&params, "package")?;
        let version = optional_string(&params, "version")?;
        validate_action(&action, package.as_deref(), version.as_deref())?;

        let requested_scope = if is_global_action(&action) {
            Scope::Wild
        } else {
            Scope::name(package.as_deref().unwrap_or_default())
        };
        let session = crate::paths::with_user_override(uid, home.clone(), async {
            authorize_package_session(&session_id, peer_pid, requested_scope, true)
        })
        .await?;

        let _guard = APT_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
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
        let output = run_package_action(&action, package.as_deref(), version.as_deref()).await?;
        let after = match package.as_deref() {
            Some(package) => match query_package_state(package).await {
                Ok(state) => Some(state),
                Err(error) => {
                    return Ok(json!({
                        "action": action,
                        "package": package,
                        "version": version,
                        "changed": Value::Null,
                        "action_applied": true,
                        "before": before,
                        "stdout_tail": output.stdout_tail,
                        "stderr_tail": output.stderr_tail,
                        "post_state_error": error,
                        "rollback": rollback,
                    }));
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

pub async fn restore(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("system package restore requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("system package restore requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let mutation_session = required_string(&params, "mutation_session")?;
        let mutation_seq = required_u64(&params, "mutation_seq")?;
        let package = required_string(&params, "package")?;
        validate_package_name(&package)?;
        let previous_version = optional_string(&params, "previous_version")?;
        if let Some(version) = previous_version.as_deref() {
            validate_version(version)?;
        }
        let was_held = required_bool(&params, "was_held")?;

        crate::paths::with_user_override(uid, home, async {
            authorize_package_session(&session_id, peer_pid, Scope::name(&package), false)?;
            validate_restore_record(
                &mutation_session,
                mutation_seq,
                &package,
                previous_version.as_deref(),
                was_held,
            )
        })
        .await?;

        let _guard = APT_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        restore_package_state_async(&package, previous_version.as_deref(), was_held).await?;
        Ok(json!({
            "package": package,
            "restored": true,
            "state": query_package_state(&package).await?,
        }))
    }
}

pub fn restore_package_state(
    mutation_session: &SessionId,
    mutation_seq: u64,
    package: &str,
    previous_version: Option<&str>,
    was_held: bool,
) -> Result<(), String> {
    validate_package_name(package)?;
    if let Some(version) = previous_version {
        validate_version(version)?;
    }
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } == 0 {
            return restore_package_state_sync(package, previous_version, was_held);
        }
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
                }),
            ),
        )?;
        if response.ok {
            Ok(())
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

fn authorize_package_session(
    session_id: &str,
    peer_pid: u32,
    scope: Scope,
    require_pkg_app: bool,
) -> Result<SessionInfo, String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("package session not found: {session_id}"))?;
    if require_pkg_app && session.app_id.as_deref() != Some("pkg") {
        return Err("system package control is restricted to the pkg App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("package session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "package session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("package session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("package request did not originate from the authorized session".to_string());
    }

    let mut caps = session.caps.clone().unwrap_or_else(CapSet::new);
    if let Some(transient) = &session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    let requested = Cap::new(Verb::SYS_PACKAGE, scope);
    if !caps.covers(&requested) {
        return Err(format!(
            "session lacks sys.package permission for {}",
            requested.scope
        ));
    }
    Ok(session)
}

async fn query_package_state(package: &str) -> Result<PackageState, String> {
    let query = run_command_allow_failure(
        dpkg_query_path(),
        &["-W", "-f=${db:Status-Abbrev}\t${Version}", "--", package],
    )
    .await?;
    let installed = query.status == 0 && query.stdout.starts_with("ii ");
    let version = installed
        .then(|| {
            query
                .stdout
                .split_once('\t')
                .map(|(_, value)| value.trim().to_string())
        })
        .flatten();
    let holds = run_command(apt_mark_path(), &["showhold"]).await?;
    let held = holds.stdout.lines().any(|line| line.trim() == package);
    Ok(PackageState {
        package: package.to_string(),
        installed,
        version,
        held,
    })
}

async fn run_package_action(
    action: &str,
    package: Option<&str>,
    version: Option<&str>,
) -> Result<CommandOutput, String> {
    match action {
        "install" => {
            run_command(
                apt_get_path(),
                &[
                    "install",
                    "-y",
                    "--no-install-recommends",
                    "--",
                    package.unwrap(),
                ],
            )
            .await
        }
        "install-version" => {
            let spec = format!("{}={}", package.unwrap(), version.unwrap());
            run_command_owned(
                apt_get_path(),
                vec![
                    "install".into(),
                    "-y".into(),
                    "--allow-downgrades".into(),
                    "--no-install-recommends".into(),
                    "--".into(),
                    spec,
                ],
            )
            .await
        }
        "remove" => run_command(apt_get_path(), &["remove", "-y", "--", package.unwrap()]).await,
        "purge" => run_command(apt_get_path(), &["purge", "-y", "--", package.unwrap()]).await,
        "upgrade" => {
            run_command(
                apt_get_path(),
                &["install", "--only-upgrade", "-y", "--", package.unwrap()],
            )
            .await
        }
        "update-index" => run_command(apt_get_path(), &["update"]).await,
        "upgrade-all" => run_command(apt_get_path(), &["upgrade", "-y"]).await,
        "hold" => run_command(apt_mark_path(), &["hold", "--", package.unwrap()]).await,
        "unhold" => run_command(apt_mark_path(), &["unhold", "--", package.unwrap()]).await,
        _ => Err(format!("unsupported package action: {action}")),
    }
}

async fn restore_package_state_async(
    package: &str,
    previous_version: Option<&str>,
    was_held: bool,
) -> Result<(), String> {
    match previous_version {
        Some(version) => {
            let spec = format!("{package}={version}");
            run_command_owned(
                apt_get_path(),
                vec![
                    "install".into(),
                    "-y".into(),
                    "--allow-downgrades".into(),
                    "--no-install-recommends".into(),
                    "--".into(),
                    spec,
                ],
            )
            .await?;
        }
        None => {
            run_command(apt_get_path(), &["remove", "-y", "--", package]).await?;
        }
    }
    run_command(
        apt_mark_path(),
        &[if was_held { "hold" } else { "unhold" }, "--", package],
    )
    .await?;
    Ok(())
}

fn restore_package_state_sync(
    package: &str,
    previous_version: Option<&str>,
    was_held: bool,
) -> Result<(), String> {
    match previous_version {
        Some(version) => {
            let spec = format!("{package}={version}");
            run_command_sync_owned(
                apt_get_path(),
                vec![
                    "install".into(),
                    "-y".into(),
                    "--allow-downgrades".into(),
                    "--no-install-recommends".into(),
                    "--".into(),
                    spec,
                ],
            )?;
        }
        None => {
            run_command_sync(apt_get_path(), &["remove", "-y", "--", package])?;
        }
    }
    run_command_sync(
        apt_mark_path(),
        &[if was_held { "hold" } else { "unhold" }, "--", package],
    )?;
    Ok(())
}

fn prepare_rollback_record(
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
struct RollbackRecord {
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

fn run_command_sync(path: &str, args: &[&str]) -> Result<CommandOutput, String> {
    run_command_sync_owned(
        path,
        args.iter().map(|value| (*value).to_string()).collect(),
    )
}

fn run_command_sync_owned(path: &str, args: Vec<String>) -> Result<CommandOutput, String> {
    let timeout = PACKAGE_TIMEOUT.as_secs().to_string();
    let output = std::process::Command::new(timeout_path())
        .args(["--signal=KILL", &timeout, path])
        .args(&args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("DEBIAN_FRONTEND", "noninteractive")
        .env("APT_LISTCHANGES_FRONTEND", "none")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to launch timeout wrapper for {path}: {error}"))?;
    if matches!(output.status.code(), Some(124) | Some(137)) {
        return Err(format!(
            "{path} timed out after {}s",
            PACKAGE_TIMEOUT.as_secs()
        ));
    }
    command_output(path, &args, output, false)
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

fn timeout_path() -> &'static str {
    if std::path::Path::new("/usr/bin/timeout").is_file() {
        "/usr/bin/timeout"
    } else {
        "/bin/timeout"
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/packages.rs"
    ));
}
