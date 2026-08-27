use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use crate::caps::{Cap, CapSet, Scope, Verb};
use crate::proc::SessionInfo;
use crate::session::{Mutation, MutationRecord, SessionId};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(120);
static SYSTEMD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitState {
    pub unit: String,
    pub description: String,
    pub load_state: String,
    pub active_state: String,
    pub sub_state: String,
    pub unit_file_state: String,
    pub active: bool,
    pub enabled: Option<bool>,
}

fn validate_restore_record(
    mutation_session: &str,
    mutation_seq: u64,
    unit: &str,
    active: bool,
    enabled: Option<bool>,
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
        Mutation::SystemService {
            unit: recorded_unit,
            was_active,
            was_enabled,
        } if recorded_unit == unit && was_active == active && was_enabled == enabled => Ok(()),
        Mutation::SystemService { .. } => {
            Err("requested systemd restore does not match the recorded inverse state".to_string())
        }
        _ => Err("rollback mutation is not a native systemd service change".to_string()),
    }
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err(BrokerError::unavailable(
            "native systemd control requires Linux",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err(BrokerError::unavailable(
                "native systemd control requires root clawd",
            ));
        }

        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let action = required_string(&params, "action")?;
        let session_id = required_string(&params, "session")?;
        let unit = required_string(&params, "unit")?;
        validate_unit_name(&unit)?;

        let verb = if action == "status" {
            Verb::SYS_OBSERVE
        } else {
            Verb::SYS_SERVICE
        };
        let session = crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, &unit, verb, true)
        })
        .await?;
        systemctl_path().map_err(backend_unavailable)?;

        if action == "status" {
            return Ok(json!({
                "action": action,
                "unit": unit,
                "state": read_unit_state(&unit).await?,
                "changed": false,
            }));
        }
        if !matches!(
            action.as_str(),
            "start" | "stop" | "restart" | "reload" | "enable" | "disable"
        ) {
            return Err(BrokerError::execution(format!(
                "unsupported systemd action `{action}`; expected status, start, stop, restart, reload, enable, or disable"
            )));
        }

        let _guard = SYSTEMD_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let before = read_unit_state(&unit).await?;
        let rollback = if matches!(action.as_str(), "start" | "stop" | "enable" | "disable") {
            crate::paths::with_user_override(uid, home.clone(), async {
                prepare_rollback_record(&session, &before)
            })
            .await?
        } else {
            RollbackRecord::not_applicable()
        };
        let output = run_systemctl_action(&action, &unit).await?;
        let after = match read_unit_state(&unit).await {
            Ok(after) => after,
            Err(error) => {
                return Ok(json!({
                    "action": action,
                    "unit": unit,
                    "changed": Value::Null,
                    "action_applied": true,
                    "before": before,
                    "stdout_tail": output.stdout_tail,
                    "stderr_tail": output.stderr_tail,
                    "post_state_error": error,
                    "rollback": rollback,
                }));
            }
        };

        Ok(json!({
            "action": action,
            "unit": unit,
            "changed": before != after,
            "before": before,
            "after": after,
            "stdout_tail": output.stdout_tail,
            "stderr_tail": output.stderr_tail,
            "rollback": rollback,
        }))
    }
}

pub async fn restore(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err(BrokerError::unavailable(
            "native systemd restore requires Linux",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err(BrokerError::unavailable(
                "native systemd restore requires root clawd",
            ));
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let unit = required_string(&params, "unit")?;
        validate_unit_name(&unit)?;
        let active = required_bool(&params, "active")?;
        let enabled = optional_bool(&params, "enabled")?;
        let mutation_session = required_string(&params, "mutation_session")?;
        let mutation_seq = required_u64(&params, "mutation_seq")?;

        crate::paths::with_user_override(uid, home, async {
            authorize_session(&session_id, peer_pid, &unit, Verb::SYS_SERVICE, false)?;
            validate_restore_record(&mutation_session, mutation_seq, &unit, active, enabled)
                .map_err(BrokerError::execution)
        })
        .await?;
        systemctl_path().map_err(backend_unavailable)?;

        let _guard = SYSTEMD_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        restore_state_async(&unit, active, enabled).await?;
        Ok(json!({
            "unit": unit,
            "restored": true,
            "state": read_unit_state(&unit).await?,
        }))
    }
}

fn backend_unavailable(message: String) -> BrokerError {
    BrokerError::unavailable(message)
}

pub fn restore_unit_state(
    mutation_session: &SessionId,
    mutation_seq: u64,
    unit: &str,
    active: bool,
    enabled: Option<bool>,
) -> Result<(), String> {
    validate_unit_name(unit)?;
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } == 0 {
            return restore_state_sync(unit, active, enabled);
        }
        let session = std::env::var("COS_SESSION")
            .map_err(|_| "systemd rollback requires COS_SESSION".to_string())?;
        let response = crate::clawd::client::request_blocking(
            crate::paths::clawd_socket_path(),
            crate::clawd::protocol::Request::build(
                crate::clawd::routes::Command::SystemServiceRestore,
                json!({
                    "session": session,
                    "mutation_session": mutation_session.as_str(),
                    "mutation_seq": mutation_seq,
                    "unit": unit,
                    "active": active,
                    "enabled": enabled,
                }),
            ),
        )?;
        if response.ok {
            Ok(())
        } else {
            Err(response
                .error
                .map(|error| error.message)
                .unwrap_or_else(|| "clawd systemd restore failed".to_string()))
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (active, enabled);
        Err("native systemd restore requires Linux".to_string())
    }
}

fn authorize_session(
    session_id: &str,
    peer_pid: u32,
    unit: &str,
    verb: Verb,
    require_systemd_app: bool,
) -> Result<SessionInfo, BrokerError> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| {
            BrokerError::authorization(format!("systemd session not found: {session_id}"))
        })?;
    if require_systemd_app && session.app_id.as_deref() != Some("systemd") {
        return Err(BrokerError::authorization(
            "native systemd control is restricted to the systemd App",
        ));
    }
    if session.pending_bind || session.pid == 0 {
        return Err(BrokerError::authorization(
            "systemd session is not bound to a process",
        ));
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| BrokerError::authorization("systemd session has no process identity"))?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err(BrokerError::authorization(
            "systemd session process identity is stale",
        ));
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err(BrokerError::authorization(
            "systemd request did not originate from the authorized session",
        ));
    }

    let mut caps = session.caps.clone().unwrap_or_else(CapSet::new);
    if let Some(transient) = &session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    let requested = Cap::new(verb, Scope::name(unit));
    if !caps.covers(&requested) {
        return Err(BrokerError::authorization(format!(
            "session lacks {} permission for `{unit}`",
            verb.as_str()
        )));
    }
    Ok(session)
}

async fn read_unit_state(unit: &str) -> Result<UnitState, String> {
    let output = run_systemctl(&[
        "show",
        "--no-pager",
        "--property=Id",
        "--property=Description",
        "--property=LoadState",
        "--property=ActiveState",
        "--property=SubState",
        "--property=UnitFileState",
        "--",
        unit,
    ])
    .await?;
    let values = parse_properties(&output.stdout);
    if values
        .get("LoadState")
        .is_some_and(|state| state == "not-found")
    {
        return Err(format!("systemd unit not found: {unit}"));
    }
    Ok(state_from_properties(unit, &values))
}

fn read_unit_state_sync(unit: &str) -> Result<UnitState, String> {
    let output = run_systemctl_sync(&[
        "show",
        "--no-pager",
        "--property=Id",
        "--property=Description",
        "--property=LoadState",
        "--property=ActiveState",
        "--property=SubState",
        "--property=UnitFileState",
        "--",
        unit,
    ])?;
    let values = parse_properties(&output.stdout);
    if values
        .get("LoadState")
        .is_some_and(|state| state == "not-found")
    {
        return Err(format!("systemd unit not found: {unit}"));
    }
    Ok(state_from_properties(unit, &values))
}

fn state_from_properties(unit: &str, values: &BTreeMap<String, String>) -> UnitState {
    let active_state = values.get("ActiveState").cloned().unwrap_or_default();
    let unit_file_state = values.get("UnitFileState").cloned().unwrap_or_default();
    UnitState {
        unit: values
            .get("Id")
            .filter(|value| !value.is_empty())
            .cloned()
            .unwrap_or_else(|| unit.to_string()),
        description: values.get("Description").cloned().unwrap_or_default(),
        load_state: values.get("LoadState").cloned().unwrap_or_default(),
        sub_state: values.get("SubState").cloned().unwrap_or_default(),
        active: matches!(active_state.as_str(), "active" | "reloading"),
        enabled: enabled_bool(&unit_file_state),
        active_state,
        unit_file_state,
    }
}

async fn run_systemctl_action(action: &str, unit: &str) -> Result<CommandOutput, String> {
    run_systemctl(&[action, "--", unit]).await
}

async fn restore_state_async(
    unit: &str,
    active: bool,
    enabled: Option<bool>,
) -> Result<(), String> {
    if let Some(enabled) = enabled {
        run_systemctl_action(if enabled { "enable" } else { "disable" }, unit).await?;
    }
    run_systemctl_action(if active { "start" } else { "stop" }, unit).await?;
    Ok(())
}

fn restore_state_sync(unit: &str, active: bool, enabled: Option<bool>) -> Result<(), String> {
    if let Some(enabled) = enabled {
        run_systemctl_sync(&[if enabled { "enable" } else { "disable" }, "--", unit])?;
    }
    run_systemctl_sync(&[if active { "start" } else { "stop" }, "--", unit])?;
    Ok(())
}

fn prepare_rollback_record(
    session: &SessionInfo,
    before: &UnitState,
) -> Result<RollbackRecord, String> {
    let Some(parent) = session.parent.as_deref() else {
        return Ok(RollbackRecord::unavailable(
            "App session has no durable parent task.",
        ));
    };
    let Ok(parent_id) = parent.parse::<SessionId>() else {
        return Ok(RollbackRecord::unavailable(
            "Parent session is not a durable task id.",
        ));
    };
    if !crate::session::session_dir(&parent_id).is_dir() {
        return Ok(RollbackRecord::unavailable(
            "Parent task has no mutation store.",
        ));
    }
    let seq = crate::session::record_mutation(
        &parent_id,
        MutationRecord::new(Mutation::SystemService {
            unit: before.unit.clone(),
            was_active: before.active,
            was_enabled: before.enabled,
        })
        .with_runtime("clawd-systemd"),
    )
    .map_err(|error| format!("record systemd rollback metadata: {error}"))?;
    Ok(RollbackRecord {
        available: true,
        mutation_seq: Some(seq),
        note: "Previous active and enabled state recorded on the parent task.".to_string(),
    })
}

#[derive(Debug, Serialize)]
struct RollbackRecord {
    available: bool,
    mutation_seq: Option<u64>,
    note: String,
}

impl RollbackRecord {
    fn unavailable(note: &str) -> Self {
        Self {
            available: false,
            mutation_seq: None,
            note: note.to_string(),
        }
    }

    fn not_applicable() -> Self {
        Self::unavailable("Restart and reload do not have a state-preserving inverse.")
    }
}

struct CommandOutput {
    stdout: String,
    stdout_tail: String,
    stderr_tail: String,
}

async fn run_systemctl(args: &[&str]) -> Result<CommandOutput, String> {
    let mut command = tokio::process::Command::new(systemctl_path()?);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(SYSTEMCTL_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("systemctl timed out after {}s", SYSTEMCTL_TIMEOUT.as_secs()))?
        .map_err(|error| format!("failed to launch systemctl: {error}"))?;
    command_output(args, output)
}

fn run_systemctl_sync(args: &[&str]) -> Result<CommandOutput, String> {
    let output = std::process::Command::new(systemctl_path()?)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to launch systemctl: {error}"))?;
    command_output(args, output)
}

fn command_output(args: &[&str], output: std::process::Output) -> Result<CommandOutput, String> {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(format!(
            "systemctl {} exited {}: {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            tail(&stderr)
        ));
    }
    Ok(CommandOutput {
        stdout_tail: tail(&stdout),
        stderr_tail: tail(&stderr),
        stdout,
    })
}

fn parse_properties(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn enabled_bool(state: &str) -> Option<bool> {
    match state {
        "enabled" | "enabled-runtime" | "linked" | "linked-runtime" | "alias" => Some(true),
        "disabled" => Some(false),
        _ => None,
    }
}

fn tail(text: &str) -> String {
    const MAX: usize = 8 * 1024;
    let start = text.len().saturating_sub(MAX);
    text.get(start..).unwrap_or(text).trim().to_string()
}

pub(crate) fn validate_unit_name(unit: &str) -> Result<(), String> {
    const SUFFIXES: &[&str] = &[
        ".service", ".socket", ".timer", ".mount", ".target", ".path",
    ];
    if unit.is_empty()
        || unit.len() > 255
        || unit.starts_with('-')
        || !unit.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'@' | b':')
        })
        || !SUFFIXES.iter().any(|suffix| unit.ends_with(suffix))
    {
        return Err(format!("invalid systemd unit name: {unit:?}"));
    }
    Ok(())
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("missing required string parameter: {key}"))
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

fn optional_bool(params: &Value, key: &str) -> Result<Option<bool>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("parameter `{key}` must be boolean or null")),
    }
}

fn systemctl_path() -> Result<&'static str, String> {
    if std::path::Path::new("/usr/bin/systemctl").is_file() {
        Ok("/usr/bin/systemctl")
    } else if std::path::Path::new("/bin/systemctl").is_file() {
        Ok("/bin/systemctl")
    } else {
        Err("systemctl is not installed".to_string())
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/systemd.rs"
    ));
}
