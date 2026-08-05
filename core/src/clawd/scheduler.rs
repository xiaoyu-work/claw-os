use serde_json::Value;
use std::sync::{Arc, OnceLock};

use crate::caps::{Role, Scope};
use crate::proc::SessionInfo;

use super::client_identity::ClientIdentity;

pub async fn run(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    require_unconfined_client(client)?;
    let uid = client.require_uid()?;
    let home = client.require_home_dir()?;
    let subsystem = required_string(&params, "subsystem")?;
    let command = required_string(&params, "command")?;
    let args = params
        .get("args")
        .cloned()
        .map(serde_json::from_value::<Vec<String>>)
        .transpose()
        .map_err(|error| format!("invalid scheduler args: {error}"))?
        .unwrap_or_default();
    if !matches!(subsystem.as_str(), "cron" | "triggers") {
        return Err(format!("unsupported scheduler subsystem: {subsystem}"));
    }
    if command == "tick" {
        return Err("scheduler tick is reserved for the kernel heartbeat".to_string());
    }

    let session_id = format!("scheduler-client-{}", uuid::Uuid::new_v4().simple());
    let role = Role::AgentHost;
    let home_scope = Scope::path(format!("{}/**", home.display()));
    let info = SessionInfo {
        session_id,
        pid: std::process::id(),
        command: vec![format!("{subsystem}.{command}")],
        started_at: chrono::Utc::now().to_rfc3339(),
        stdout_path: String::new(),
        stderr_path: String::new(),
        group: Some("scheduler-client".to_string()),
        parent: None,
        workdir: Some(home.to_string_lossy().into_owned()),
        exit_code: None,
        ended_at: None,
        tier: Some(role.credential_tier()),
        scope: Some("scheduler-client".to_string()),
        priority: None,
        caps: Some(role.caps_with_scopes(
            Some(home_scope),
            Some(Scope::Wild),
            Some(Scope::Wild),
        )),
        transient_caps: None,
        role: Some(role.name().to_string()),
        app_id: None,
        pending_bind: false,
        start_time_ticks: crate::proc::read_start_time_ticks_pub(std::process::id()),
    };
    let permit = scheduler_slots()
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| "scheduler executor is unavailable".to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|error| format!("create scheduler runtime: {error}"))?;
        runtime.block_on(crate::paths::with_user_override(
            uid,
            home,
            crate::proc::with_trusted_session_override(info, async move {
                match subsystem.as_str() {
                    "cron" => crate::cron::run(&command, &args),
                    "triggers" => crate::triggers::run(&command, &args),
                    _ => unreachable!(),
                }
            }),
        ))
    })
    .await
    .map_err(|error| format!("scheduler executor failed: {error}"))?
}

fn scheduler_slots() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)))
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

fn require_unconfined_client(client: &ClientIdentity) -> Result<(), String> {
    let pid = client
        .pid
        .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
    if process_no_new_privs(pid) != Some(false) {
        return Err("confined processes cannot manage proactive jobs".to_string());
    }
    if !process_has_controlling_tty(pid) {
        return Err(
            "scheduler management requires an interactive, unconfined CLI".to_string(),
        );
    }
    Ok(())
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
fn process_has_controlling_tty(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(close) = stat.rfind(')') else {
        return false;
    };
    stat[close + 1..]
        .split_whitespace()
        .nth(4)
        .and_then(|value| value.parse::<i64>().ok())
        .is_some_and(|tty_nr| tty_nr != 0)
}

#[cfg(not(target_os = "linux"))]
fn process_has_controlling_tty(_pid: u32) -> bool {
    false
}
