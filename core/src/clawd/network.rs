use serde_json::{json, Value};
use std::io::Write;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use crate::caps::{Cap, Scope, Verb};
use crate::proc::SessionInfo;

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const NMCLI_TIMEOUT: Duration = Duration::from_secs(120);
static NETWORK_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("NetworkManager control requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("NetworkManager control requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let home = client.require_home_dir()?;
        let action = required_string(&params, "action")?;
        let target = optional_string(&params, "target")?;
        let state = optional_string(&params, "state")?;
        let credential = optional_string(&params, "credential")?;
        validate_action(
            &action,
            target.as_deref(),
            state.as_deref(),
            credential.as_deref(),
        )?;

        let (verb, scope) = if is_read_action(&action) {
            (Verb::SYS_OBSERVE, Scope::name("network"))
        } else {
            (Verb::NET_MANAGE, Scope::name(action_scope(&action)))
        };
        let (session, _authorized) =
            authorize_session(authority, verb, scope, credential.as_deref())?;

        if is_read_action(&action) {
            return read_action(&action).await;
        }

        let _guard = NETWORK_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await;
        let (command_target, before) = prepare_action(&action, target.as_deref()).await?;
        let password = match credential.as_deref() {
            Some(reference) => {
                let (namespace, name) = parse_credential_ref(reference)?;
                Some(
                    crate::paths::with_user_override(uid, home, async {
                        crate::credential::load_for_broker(
                            &name,
                            &namespace,
                            session.tier.unwrap_or(u8::MAX),
                        )
                    })
                    .await?,
                )
            }
            None => None,
        };
        let output = run_action(
            &action,
            command_target.as_deref(),
            state.as_deref(),
            password.as_deref(),
        )
        .await?;
        let after =
            match read_action_state(&action, target.as_deref(), command_target.as_deref()).await {
                Ok(after) => after,
                Err(error) => {
                    return Ok(json!({
                        "action": action,
                        "target": target,
                        "state": state,
                        "changed": Value::Null,
                        "action_applied": true,
                        "before": before,
                        "stdout_tail": output,
                        "post_state_error": error,
                    }));
                }
            };
        Ok(json!({
            "action": action,
            "target": target,
            "state": state,
            "changed": before != after,
            "before": before,
            "after": after,
            "stdout_tail": output,
        }))
    }
}

async fn prepare_action(
    action: &str,
    target: Option<&str>,
) -> Result<(Option<String>, Value), String> {
    match action {
        "wifi-disconnect" => {
            let device = target.unwrap_or_default();
            let before = device_state(device).await?;
            require_wifi_device(&before)?;
            Ok((Some(device.to_string()), before))
        }
        "wifi-forget" => {
            let name = target.unwrap_or_default();
            let uuid = resolve_connection_uuid(name, "wifi").await?;
            let before = connection_state_by_uuid(name, &uuid).await?;
            Ok((Some(uuid), before))
        }
        "vpn-up" | "vpn-down" => {
            let name = target.unwrap_or_default();
            let uuid = resolve_connection_uuid(name, "vpn").await?;
            let before = connection_state_by_uuid(name, &uuid).await?;
            Ok((Some(uuid), before))
        }
        _ => {
            let command_target = target.map(str::to_string);
            let before = read_action_state(action, target, command_target.as_deref()).await?;
            Ok((command_target, before))
        }
    }
}

async fn read_action_state(
    action: &str,
    target: Option<&str>,
    command_target: Option<&str>,
) -> Result<Value, String> {
    match action {
        "wifi-toggle" | "airplane" => read_status().await,
        "wifi-disconnect" => device_state(target.unwrap_or_default()).await,
        "wifi-connect" => connection_state(target.unwrap_or_default()).await,
        "wifi-forget" | "vpn-up" | "vpn-down" => {
            connection_state_by_uuid(
                target.unwrap_or_default(),
                command_target.unwrap_or_default(),
            )
            .await
        }
        _ => read_status().await,
    }
}

async fn connection_state(name: &str) -> Result<Value, String> {
    let connections = connection_list().await?;
    let matches = connections
        .into_iter()
        .filter(|connection| connection["name"].as_str() == Some(name))
        .collect::<Vec<_>>();
    Ok(json!({
        "name": name,
        "exists": !matches.is_empty(),
        "connections": matches,
    }))
}

async fn connection_state_by_uuid(name: &str, uuid: &str) -> Result<Value, String> {
    let connections = connection_list().await?;
    let matches = connections
        .into_iter()
        .filter(|connection| connection["uuid"].as_str() == Some(uuid))
        .collect::<Vec<_>>();
    Ok(json!({
        "name": name,
        "uuid": uuid,
        "exists": !matches.is_empty(),
        "connections": matches,
    }))
}

async fn device_state(device: &str) -> Result<Value, String> {
    let output = run_nmcli(&[
        "-t",
        "-m",
        "multiline",
        "-f",
        "GENERAL.DEVICE,GENERAL.TYPE,GENERAL.STATE,GENERAL.CONNECTION",
        "device",
        "show",
        device,
    ])
    .await?;
    Ok(json!({
        "device": device,
        "properties": parse_key_values(&output),
    }))
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
fn authorize_session(
    authority: &Decision,
    verb: Verb,
    scope: Scope,
    credential: Option<&str>,
) -> Result<(SessionInfo, Authorized), String> {
    authority.require_app("network-manager")?;
    let mut requested = vec![Cap::new(verb, scope)];
    if let Some(credential) = credential {
        let (namespace, name) = parse_credential_ref(credential)?;
        requested.push(Cap::new(
            Verb::SECRET_READ,
            Scope::name(format!("{namespace}/{name}")),
        ));
    }
    // One spend for the whole request: a connection that needs a
    // credential it cannot prove authorises nothing at all, rather
    // than burning the network capability first.
    let proof = authority.require_all(&requested)?;
    Ok((authority.session()?.clone(), proof))
}

fn require_wifi_device(state: &Value) -> Result<(), String> {
    let device = state["device"].as_str().unwrap_or_default();
    let device_type = state["properties"]["general.type"]
        .as_str()
        .ok_or_else(|| format!("NetworkManager did not report a type for device {device:?}"))?;
    if !is_wifi_type(device_type) {
        return Err(format!(
            "device {device:?} is type {device_type:?}, not a Wi-Fi device"
        ));
    }
    Ok(())
}

async fn resolve_connection_uuid(name: &str, category: &str) -> Result<String, String> {
    let connections = connection_list().await?;
    let mut named_count = 0_usize;
    let mut matching_uuids = Vec::new();
    for connection in connections {
        if connection["name"].as_str() != Some(name) {
            continue;
        }
        named_count += 1;
        let connection_type = connection["type"].as_str().unwrap_or_default();
        if connection_type_matches(connection_type, category) {
            let uuid = connection["uuid"]
                .as_str()
                .ok_or_else(|| format!("connection {name:?} has no UUID"))?;
            validate_name("connection UUID", uuid)?;
            matching_uuids.push(uuid.to_string());
        }
    }
    match matching_uuids.as_slice() {
        [uuid] => Ok(uuid.clone()),
        [] if named_count == 0 => Err(format!("NetworkManager connection not found: {name:?}")),
        [] => Err(format!(
            "connection {name:?} is not a {category} connection"
        )),
        _ => Err(format!(
            "multiple {category} connections are named {name:?}; use a unique profile name"
        )),
    }
}

fn connection_type_matches(connection_type: &str, category: &str) -> bool {
    match category {
        "wifi" => is_wifi_type(connection_type),
        "vpn" => matches!(connection_type, "vpn" | "wireguard"),
        _ => false,
    }
}

fn is_wifi_type(connection_type: &str) -> bool {
    matches!(connection_type, "wifi" | "802-11-wireless")
}

async fn read_action(action: &str) -> Result<Value, String> {
    match action {
        "status" => Ok(json!({"status": read_status().await?})),
        "wifi-list" => Ok(json!({"wifi": wifi_list().await?})),
        "connection-list" => Ok(json!({"connections": connection_list().await?})),
        "vpn-list" => Ok(json!({
            "connections": connection_list()
                .await?
                .into_iter()
                .filter(|item| {
                    item["type"]
                        .as_str()
                        .is_some_and(|kind| connection_type_matches(kind, "vpn"))
                })
                .collect::<Vec<_>>()
        })),
        _ => Err(format!("unsupported read action: {action}")),
    }
}

async fn read_status() -> Result<Value, String> {
    let general = run_nmcli(&[
        "-t",
        "-m",
        "multiline",
        "-f",
        "STATE,CONNECTIVITY",
        "general",
    ])
    .await?;
    let radio = run_nmcli(&[
        "-t",
        "-m",
        "multiline",
        "-f",
        "WIFI,WIFI-HW,WWAN,WWAN-HW",
        "radio",
    ])
    .await?;
    Ok(json!({
        "general": parse_key_values(&general),
        "radio": parse_key_values(&radio),
    }))
}

async fn wifi_list() -> Result<Vec<Value>, String> {
    let output = run_nmcli(&[
        "-t",
        "-f",
        "IN-USE,SSID,BSSID,CHAN,RATE,SIGNAL,SECURITY",
        "device",
        "wifi",
        "list",
        "--rescan",
        "yes",
    ])
    .await?;
    Ok(output
        .lines()
        .map(split_terse)
        .filter(|fields| fields.len() >= 7)
        .map(|fields| {
            json!({
                "in_use": fields[0] == "*",
                "ssid": fields[1],
                "bssid": fields[2],
                "channel": fields[3],
                "rate": fields[4],
                "signal": fields[5].parse::<u64>().ok(),
                "security": fields[6],
            })
        })
        .collect())
}

async fn connection_list() -> Result<Vec<Value>, String> {
    let output = run_nmcli(&["-t", "-f", "NAME,UUID,TYPE,DEVICE", "connection", "show"]).await?;
    Ok(output
        .lines()
        .map(split_terse)
        .filter(|fields| fields.len() >= 4)
        .map(|fields| {
            json!({
                "name": fields[0],
                "uuid": fields[1],
                "type": fields[2],
                "device": fields[3],
                "active": !fields[3].is_empty() && fields[3] != "--",
            })
        })
        .collect())
}

async fn run_action(
    action: &str,
    target: Option<&str>,
    state: Option<&str>,
    password: Option<&str>,
) -> Result<String, String> {
    match action {
        "wifi-connect" => wifi_connect(target.unwrap(), password).await,
        "wifi-disconnect" => run_nmcli(&["device", "disconnect", target.unwrap()]).await,
        "wifi-forget" => run_nmcli(&["connection", "delete", "uuid", target.unwrap()]).await,
        "wifi-toggle" => run_nmcli(&["radio", "wifi", state.unwrap()]).await,
        "airplane" => {
            run_nmcli(&[
                "radio",
                "all",
                if state.unwrap() == "on" { "off" } else { "on" },
            ])
            .await
        }
        "vpn-up" => run_nmcli(&["connection", "up", "uuid", target.unwrap()]).await,
        "vpn-down" => run_nmcli(&["connection", "down", "uuid", target.unwrap()]).await,
        _ => Err(format!("unsupported network action: {action}")),
    }
}

async fn wifi_connect(ssid: &str, password: Option<&str>) -> Result<String, String> {
    let mut args = vec!["--wait", "30"];
    if let Some(password) = password {
        if password.is_empty()
            || password.len() > 1024
            || password.chars().any(|character| character.is_control())
        {
            return Err("stored Wi-Fi credential is not a valid single-line secret".to_string());
        }
        let mut file = tempfile::NamedTempFile::new()
            .map_err(|error| format!("create nmcli password file: {error}"))?;
        writeln!(file, "802-11-wireless-security.psk:{password}")
            .map_err(|error| format!("write nmcli password file: {error}"))?;
        file.flush()
            .map_err(|error| format!("flush nmcli password file: {error}"))?;
        let path = file
            .path()
            .to_str()
            .ok_or_else(|| "nmcli password path is not UTF-8".to_string())?
            .to_string();
        args.extend(["--passwd-file", &path]);
        args.extend(["device", "wifi", "connect", ssid]);
        let result = run_nmcli(&args).await;
        drop(file);
        return result;
    }
    args.extend(["device", "wifi", "connect", ssid]);
    run_nmcli(&args).await
}

async fn run_nmcli(args: &[&str]) -> Result<String, String> {
    let mut command = tokio::process::Command::new(nmcli_path());
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = tokio::time::timeout(NMCLI_TIMEOUT, command.output())
        .await
        .map_err(|_| format!("nmcli timed out after {}s", NMCLI_TIMEOUT.as_secs()))?
        .map_err(|error| format!("failed to launch nmcli: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if !output.status.success() {
        return Err(format!(
            "nmcli {} exited {}: {}",
            args.join(" "),
            output.status.code().unwrap_or(-1),
            tail(&stderr)
        ));
    }
    Ok(stdout.trim().to_string())
}

fn parse_key_values(output: &str) -> Value {
    let mut map = serde_json::Map::new();
    for line in output.lines() {
        if let Some((key, value)) = line.split_once(':') {
            map.insert(key.to_ascii_lowercase(), Value::String(value.to_string()));
        }
    }
    Value::Object(map)
}

fn split_terse(line: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in line.chars() {
        if escaped {
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ':' {
            values.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    values.push(current);
    values
}

fn validate_action(
    action: &str,
    target: Option<&str>,
    state: Option<&str>,
    credential: Option<&str>,
) -> Result<(), String> {
    if is_read_action(action) {
        if target.is_some() || state.is_some() || credential.is_some() {
            return Err(format!(
                "{action} does not accept target, state, or credential"
            ));
        }
        return Ok(());
    }
    match action {
        "wifi-connect" => {
            validate_text("SSID", target.unwrap_or_default(), 32)?;
            if state.is_some() {
                return Err("wifi-connect does not accept state".to_string());
            }
            if let Some(credential) = credential {
                parse_credential_ref(credential)?;
            }
        }
        "wifi-disconnect" => {
            validate_name("device", target.unwrap_or_default())?;
            require_none(action, state, credential)?;
        }
        "wifi-forget" | "vpn-up" | "vpn-down" => {
            validate_text("connection", target.unwrap_or_default(), 255)?;
            require_none(action, state, credential)?;
        }
        "wifi-toggle" | "airplane" => {
            if target.is_some() || credential.is_some() {
                return Err(format!("{action} accepts only state=on|off"));
            }
            if !matches!(state, Some("on" | "off")) {
                return Err(format!("{action} requires state=on|off"));
            }
        }
        _ => return Err(format!("unsupported network action: {action}")),
    }
    Ok(())
}

fn require_none(action: &str, state: Option<&str>, credential: Option<&str>) -> Result<(), String> {
    if state.is_some() || credential.is_some() {
        return Err(format!("{action} does not accept state or credential"));
    }
    Ok(())
}

fn is_read_action(action: &str) -> bool {
    matches!(
        action,
        "status" | "wifi-list" | "connection-list" | "vpn-list"
    )
}

fn action_scope(action: &str) -> &'static str {
    match action {
        "wifi-connect" | "wifi-disconnect" | "wifi-forget" | "wifi-toggle" => "wifi",
        "vpn-up" | "vpn-down" => "vpn",
        "airplane" => "airplane",
        _ => "network",
    }
}

fn validate_name(kind: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err(format!("invalid {kind}: {value:?}"));
    }
    Ok(())
}

fn parse_credential_ref(value: &str) -> Result<(String, String), String> {
    let (namespace, name) = value
        .split_once('/')
        .ok_or_else(|| "credential must use namespace/name form".to_string())?;
    validate_name("credential namespace", namespace)?;
    validate_name("credential name", name)?;
    Ok((namespace.to_string(), name.to_string()))
}

fn validate_text(kind: &str, value: &str, max_bytes: usize) -> Result<(), String> {
    if value.is_empty()
        || value.len() > max_bytes
        || value.starts_with('-')
        || value.chars().any(|character| character.is_control())
    {
        return Err(format!("invalid {kind}: {value:?}"));
    }
    Ok(())
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value.clone())),
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(format!("parameter `{key}` must be a string or null")),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key)?.ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn tail(value: &str) -> String {
    const MAX: usize = 8 * 1024;
    let start = value.len().saturating_sub(MAX);
    value.get(start..).unwrap_or(value).trim().to_string()
}

fn nmcli_path() -> &'static str {
    "/usr/bin/nmcli"
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/network.rs"
    ));
}
