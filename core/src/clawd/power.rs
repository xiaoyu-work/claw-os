use serde_json::{json, Map, Value};
use std::fs;
use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;
use super::protocol::BrokerError;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 1024 * 1024;
static POWER_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, BrokerError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err(BrokerError::unavailable(
            "Power Manager requires Linux systemd-logind",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err(BrokerError::unavailable(
                "Power Manager requires root clawd",
            ));
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(&action, confirm).map_err(BrokerError::execution)?;
        let requested = if action == "status" {
            Cap::new(Verb::SYS_OBSERVE, Scope::name("power"))
        } else {
            Cap::new(Verb::SYS_POWER, Scope::Wild)
        };
        crate::paths::with_user_override(uid, home, async {
            authorize_session(&session_id, peer_pid, requested)
        })
        .await?;

        if action == "status" {
            return power_status().await.map_err(BrokerError::execution);
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            POWER_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| BrokerError::unavailable("Power Manager is busy with another operation"))?;
        busctl_path().map_err(backend_unavailable)?;
        request_power_action(&action, uid, &session_id)
            .await
            .map_err(BrokerError::execution)
    }
}

fn backend_unavailable(message: String) -> BrokerError {
    BrokerError::unavailable(message)
}

fn authorize_session(
    session_id: &str,
    peer_pid: u32,
    requested: Cap,
) -> Result<(), BrokerError> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| {
            BrokerError::authorization(format!("power-manager session not found: {session_id}"))
        })?;
    if session.app_id.as_deref() != Some("power-manager") {
        return Err(BrokerError::authorization(
            "power control is restricted to the power-manager App",
        ));
    }
    if session.pending_bind || session.pid == 0 {
        return Err(BrokerError::authorization(
            "power-manager session is not bound to a process",
        ));
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| {
            BrokerError::authorization("power-manager session has no process identity")
        })?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err(BrokerError::authorization(
            "power-manager session process identity is stale",
        ));
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err(BrokerError::authorization(
            "power request did not originate from the authorized session",
        ));
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    if !caps.covers(&requested) {
        return Err(BrokerError::authorization(format!(
            "power-manager session lacks {}",
            requested.verb.as_str()
        )));
    }
    Ok(())
}

async fn power_status() -> Result<Value, String> {
    let devices = upower_devices().await;
    let capabilities = logind_capabilities().await;
    Ok(json!({
        "providers": {
            "upower": upower_path().is_ok(),
            "logind": busctl_path().is_ok(),
        },
        "devices": devices.unwrap_or_else(|error| vec![json!({"error": error})]),
        "capabilities": capabilities.unwrap_or_else(|error| json!({"error": error})),
    }))
}

async fn upower_devices() -> Result<Vec<Value>, String> {
    let upower = upower_path()?;
    let enumeration = run_checked(upower, &["--enumerate"], TOOL_TIMEOUT).await?;
    let mut devices = Vec::new();
    for object_path in enumeration
        .stdout
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("/org/freedesktop/UPower/devices/"))
        .take(32)
    {
        let output = run_checked(upower, &["--show-info", object_path], TOOL_TIMEOUT).await?;
        devices.push(parse_upower_device(object_path, &output.stdout));
    }
    Ok(devices)
}

fn parse_upower_device(object_path: &str, output: &str) -> Value {
    let mut properties = Map::new();
    let mut section = String::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == object_path {
            continue;
        }
        if !trimmed.contains(':') {
            section = normalize_key(trimmed);
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = normalize_key(key);
        let key = if section.is_empty() {
            key
        } else {
            format!("{section}.{key}")
        };
        properties.insert(key, parse_upower_value(value.trim()));
    }
    json!({
        "object_path": object_path,
        "properties": properties,
    })
}

fn normalize_key(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .replace([' ', '-'], "_")
}

fn parse_upower_value(value: &str) -> Value {
    match value.to_ascii_lowercase().as_str() {
        "yes" => Value::Bool(true),
        "no" => Value::Bool(false),
        _ if value.ends_with('%') => value
            .trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .map(Value::from)
            .unwrap_or_else(|_| Value::String(value.to_string())),
        _ => Value::String(value.to_string()),
    }
}

async fn logind_capabilities() -> Result<Value, String> {
    let mut capabilities = Map::new();
    for (name, method) in [
        ("suspend", "CanSuspend"),
        ("hibernate", "CanHibernate"),
        ("hybrid_sleep", "CanHybridSleep"),
        ("suspend_then_hibernate", "CanSuspendThenHibernate"),
        ("reboot", "CanReboot"),
        ("poweroff", "CanPowerOff"),
    ] {
        match call_logind(method, &[]).await {
            Ok(output) => {
                capabilities.insert(
                    name.to_string(),
                    Value::String(parse_busctl_string(&output.stdout).unwrap_or_default()),
                );
            }
            Err(error) => {
                capabilities.insert(name.to_string(), json!({"error": error}));
            }
        }
    }
    Ok(Value::Object(capabilities))
}

async fn request_power_action(
    action: &str,
    owner_uid: u32,
    session_id: &str,
) -> Result<Value, String> {
    let (capability_method, action_method) = match action {
        "suspend" => ("CanSuspend", "Suspend"),
        "hibernate" => ("CanHibernate", "Hibernate"),
        "hybrid-sleep" => ("CanHybridSleep", "HybridSleep"),
        "suspend-then-hibernate" => ("CanSuspendThenHibernate", "SuspendThenHibernate"),
        "reboot" => ("CanReboot", "Reboot"),
        "poweroff" => ("CanPowerOff", "PowerOff"),
        _ => unreachable!("validated power action"),
    };
    let capability = call_logind(capability_method, &[]).await?;
    let capability = parse_busctl_string(&capability.stdout)
        .ok_or_else(|| format!("unexpected logind {capability_method} response"))?;
    if !matches!(capability.as_str(), "yes" | "challenge") {
        return Err(format!(
            "logind reports {action} is unavailable: {capability}"
        ));
    }
    super::system_journal::record_power_intent(action, owner_uid, session_id)?;
    let before_boot_id = current_boot_id();
    let output = call_logind_no_reply(action_method, &["b", "false"]).await?;
    Ok(json!({
        "action": action,
        "requested": true,
        "confirmed": true,
        "logind_capability": capability,
        "before_boot_id": before_boot_id,
        "stdout_tail": tail(&output.stdout),
        "stderr_tail": tail(&output.stderr),
        "note": "The system may suspend, hibernate, reboot, or power off before this response is delivered.",
    }))
}

async fn call_logind(method: &str, method_args: &[&str]) -> Result<CommandOutput, String> {
    let mut args = vec![
        "call",
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
        method,
    ];
    args.extend_from_slice(method_args);
    run_checked(busctl_path()?, &args, TOOL_TIMEOUT).await
}

async fn call_logind_no_reply(method: &str, method_args: &[&str]) -> Result<CommandOutput, String> {
    let mut args = vec![
        "--expect-reply=no",
        "call",
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
        method,
    ];
    args.extend_from_slice(method_args);
    run_checked(busctl_path()?, &args, TOOL_TIMEOUT).await
}

fn parse_busctl_string(output: &str) -> Option<String> {
    output
        .split('"')
        .nth(1)
        .map(str::to_string)
        .or_else(|| output.split_whitespace().nth(1).map(str::to_string))
}

fn current_boot_id() -> Option<String> {
    fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn validate_action(action: &str, confirm: bool) -> Result<(), String> {
    match action {
        "status" if !confirm => Ok(()),
        "suspend"
        | "hibernate"
        | "hybrid-sleep"
        | "suspend-then-hibernate"
        | "reboot"
        | "poweroff"
            if confirm =>
        {
            Ok(())
        }
        "status" => Err("status does not accept --confirm".to_string()),
        "suspend"
        | "hibernate"
        | "hybrid-sleep"
        | "suspend-then-hibernate"
        | "reboot"
        | "poweroff" => Err(format!("{action} requires --confirm")),
        _ => Err(format!("unknown power action: {action}")),
    }
}

async fn run_checked(
    program: &'static str,
    args: &[&str],
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let args = args.iter().map(|value| value.to_string()).collect();
    tokio::task::spawn_blocking(move || run_checked_sync(program, args, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_checked_sync(
    program: &str,
    args: Vec<String>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", "/root")
        .env("LC_ALL", "C.UTF-8")
        .env("SYSTEMD_PAGER", "cat")
        .env("PAGER", "cat")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {program}: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{program} stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{program} stderr is unavailable"))?;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr));
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child
                    .wait()
                    .map_err(|error| format!("wait for timed-out {program}: {error}"))?;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for {program}: {error}"));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| format!("{program} stdout reader panicked"))??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| format!("{program} stderr reader panicked"))??;
    if timed_out {
        return Err(format!("{program} timed out after {}s", timeout.as_secs()));
    }
    let output = CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    };
    if !output.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            program,
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    Ok(output)
}

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    #[allow(dead_code)]
    stdout_truncated: bool,
    #[allow(dead_code)]
    stderr_truncated: bool,
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read power command output: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = STREAM_CAP_BYTES.saturating_sub(kept.len());
        let keep = remaining.min(read);
        kept.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok((kept, truncated))
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    match params.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(Value::String(_)) | None | Some(Value::Null) => {
            Err(format!("missing required string parameter: {key}"))
        }
        Some(_) => Err(format!("parameter `{key}` must be a string")),
    }
}

fn tool_path(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
        .ok_or_else(|| format!("{name} is not installed"))
}

fn busctl_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/busctl", "/bin/busctl"], "busctl")
}

fn upower_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/upower", "/bin/upower"], "upower")
}

fn tail(value: &str) -> String {
    const MAX: usize = 8 * 1024;
    if value.len() <= MAX {
        return value.trim().to_string();
    }
    let mut start = value.len() - MAX;
    while !value.is_char_boundary(start) {
        start += 1;
    }
    value[start..].trim().to_string()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/power.rs"
    ));
}
