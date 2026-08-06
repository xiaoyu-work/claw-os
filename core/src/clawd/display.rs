use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
const MAX_LAYOUT_BYTES: u64 = 1024 * 1024;
static DISPLAY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum DisplayBackupState {
    Layout {
        before_kdl: String,
        applied_sha256: String,
    },
    Brightness {
        device: String,
        before: u64,
        applied: u64,
        maximum: u64,
    },
}

#[derive(Clone, Serialize, Deserialize)]
struct DisplayBackup {
    token: String,
    owner_uid: u32,
    created_at: String,
    action: String,
    state: DisplayBackupState,
    status: String,
}

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Display Manager requires Linux COSMIC".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Display Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let gid = client
            .gid
            .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let session_id = required_string(&params, "session")?;
        let action = required_string(&params, "action")?;
        let output = optional_string(&params, "output")?;
        let from = optional_string(&params, "from")?;
        let width = optional_i64(&params, "width")?;
        let height = optional_i64(&params, "height")?;
        let refresh = optional_f64(&params, "refresh")?;
        let scale = optional_f64(&params, "scale")?;
        let x = optional_i64(&params, "x")?;
        let y = optional_i64(&params, "y")?;
        let transform = optional_string(&params, "transform")?;
        let adaptive_sync = optional_string(&params, "adaptive_sync")?;
        let source = optional_string(&params, "source")?;
        let backlight = optional_string(&params, "backlight")?;
        let percent = optional_u64(&params, "percent")?;
        let token = optional_string(&params, "token")?;
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            output.as_deref(),
            from.as_deref(),
            width,
            height,
            refresh,
            scale,
            x,
            y,
            transform.as_deref(),
            adaptive_sync.as_deref(),
            source.as_deref(),
            backlight.as_deref(),
            percent,
            token.as_deref(),
            confirm,
        )?;
        let source = source.as_deref().map(resolve_source).transpose()?;
        let requested = requested_caps(&action, source.as_deref());
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, &requested)
        })
        .await?;
        let environment = DisplayEnvironment::new(uid, gid, home, peer_pid)?;

        if action == "status" {
            return display_status(&environment).await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            DISPLAY_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Display Manager is busy with another mutation".to_string())?;
        match action.as_str() {
            "enable" | "disable" | "mirror" | "position" | "mode" | "scale" => {
                mutate_layout(
                    uid,
                    &action,
                    output.as_deref().unwrap(),
                    from.as_deref(),
                    width,
                    height,
                    refresh,
                    scale,
                    x,
                    y,
                    transform.as_deref(),
                    adaptive_sync.as_deref(),
                    &environment,
                )
                .await
            }
            "apply-layout" => {
                let content = read_source(source.as_deref().unwrap())?;
                apply_layout(uid, &content, &environment).await
            }
            "brightness" => {
                set_brightness(uid, backlight.as_deref().unwrap(), percent.unwrap()).await
            }
            "restore" => restore_display(uid, token.as_deref().unwrap(), &environment).await,
            _ => unreachable!("validated display action"),
        }
    }
}

fn requested_caps(action: &str, source: Option<&Path>) -> Vec<Cap> {
    if action == "status" {
        return vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("display"))];
    }
    let mut caps = vec![Cap::new(Verb::DEVICE_DISPLAY, Scope::name("manage"))];
    if let Some(source) = source {
        caps.push(Cap::new(
            Verb::FS_READ,
            Scope::path(source.to_string_lossy().into_owned()),
        ));
    }
    caps
}

fn authorize_session(session_id: &str, peer_pid: u32, requested: &[Cap]) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("display-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("display-manager") {
        return Err("display control is restricted to the display-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("display-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "display-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("display-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("display request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    for cap in requested {
        if !caps.covers(cap) {
            return Err(format!(
                "display-manager session lacks {}:{}",
                cap.verb.as_str(),
                cap.scope
            ));
        }
    }
    Ok(())
}

async fn display_status(environment: &DisplayEnvironment) -> Result<Value, String> {
    let kdl = list_kdl(environment).await?;
    let outputs = parse_outputs(&kdl)?;
    let backlights = backlights();
    let output_count = outputs.len();
    let backlight_count = backlights.len();
    Ok(json!({
        "outputs": outputs,
        "output_count": output_count,
        "backlights": backlights,
        "backlight_count": backlight_count,
        "kdl": kdl,
    }))
}

async fn mutate_layout(
    owner_uid: u32,
    action: &str,
    output: &str,
    from: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
    refresh: Option<f64>,
    scale: Option<f64>,
    x: Option<i64>,
    y: Option<i64>,
    transform: Option<&str>,
    adaptive_sync: Option<&str>,
    environment: &DisplayEnvironment,
) -> Result<Value, String> {
    let before_kdl = list_kdl(environment).await?;
    let before = parse_outputs(&before_kdl)?;
    ensure_output_exists(&before, output)?;
    if action == "disable"
        && before
            .iter()
            .filter(|output| output["enabled"].as_bool() == Some(true))
            .count()
            <= 1
    {
        return Err("refusing to disable the last enabled output".to_string());
    }
    if let Some(from) = from {
        ensure_output_exists(&before, from)?;
        if output == from {
            return Err("an output cannot mirror itself".to_string());
        }
    }
    if action == "mode" {
        let modes = before
            .iter()
            .find(|item| item["name"].as_str() == Some(output))
            .and_then(|item| item["modes"].as_array())
            .ok_or_else(|| "output has no modes".to_string())?;
        if !modes
            .iter()
            .any(|mode| mode["width"].as_i64() == width && mode["height"].as_i64() == height)
        {
            return Err(format!(
                "output {output} does not advertise mode {}x{}",
                width.unwrap(),
                height.unwrap()
            ));
        }
    }
    let backup = prepare_layout_backup(owner_uid, action, &before_kdl)?;
    let args = layout_command(
        action,
        output,
        from,
        width,
        height,
        refresh,
        scale,
        x,
        y,
        transform,
        adaptive_sync,
        &before,
    )?;
    let command = run_user_command(
        cosmic_randr_path()?,
        args,
        environment.clone(),
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("cosmic-randr", &command)?;
    finish_layout_backup(backup, environment).await
}

fn layout_command(
    action: &str,
    output: &str,
    from: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
    refresh: Option<f64>,
    scale: Option<f64>,
    x: Option<i64>,
    y: Option<i64>,
    transform: Option<&str>,
    adaptive_sync: Option<&str>,
    current: &[Value],
) -> Result<Vec<String>, String> {
    match action {
        "enable" | "disable" => Ok(vec![action.to_string(), output.to_string()]),
        "mirror" => Ok(vec![
            "mirror".to_string(),
            output.to_string(),
            from.unwrap().to_string(),
        ]),
        "position" => {
            let output_state = current
                .iter()
                .find(|item| item["name"].as_str() == Some(output))
                .ok_or_else(|| "output not found".to_string())?;
            let current_mode = output_state["modes"]
                .as_array()
                .and_then(|modes| {
                    modes
                        .iter()
                        .find(|mode| mode["current"].as_bool() == Some(true))
                })
                .ok_or_else(|| "output has no current mode".to_string())?;
            Ok(vec![
                "mode".to_string(),
                "--pos-x".to_string(),
                x.unwrap().to_string(),
                "--pos-y".to_string(),
                y.unwrap().to_string(),
                "--refresh".to_string(),
                format!("{:.3}", current_mode["refresh_hz"].as_f64().unwrap_or(60.0)),
                output.to_string(),
                current_mode["width"].as_i64().unwrap().to_string(),
                current_mode["height"].as_i64().unwrap().to_string(),
            ])
        }
        "mode" => {
            let mut args = vec!["mode".to_string()];
            if let Some(refresh) = refresh {
                args.extend(["--refresh".to_string(), format!("{refresh:.3}")]);
            }
            if let Some(scale) = scale {
                args.extend(["--scale".to_string(), format!("{scale:.2}")]);
            }
            if let Some(x) = x {
                args.extend(["--pos-x".to_string(), x.to_string()]);
            }
            if let Some(y) = y {
                args.extend(["--pos-y".to_string(), y.to_string()]);
            }
            if let Some(transform) = transform {
                args.extend(["--transform".to_string(), transform.to_string()]);
            }
            if let Some(adaptive_sync) = adaptive_sync {
                args.extend(["--adaptive-sync".to_string(), adaptive_sync.to_string()]);
            }
            args.extend([
                output.to_string(),
                width.unwrap().to_string(),
                height.unwrap().to_string(),
            ]);
            Ok(args)
        }
        "scale" => {
            let output_state = current
                .iter()
                .find(|item| item["name"].as_str() == Some(output))
                .ok_or_else(|| "output not found".to_string())?;
            let current_mode = output_state["modes"]
                .as_array()
                .and_then(|modes| {
                    modes
                        .iter()
                        .find(|mode| mode["current"].as_bool() == Some(true))
                })
                .ok_or_else(|| "output has no current mode".to_string())?;
            Ok(vec![
                "mode".to_string(),
                "--scale".to_string(),
                format!("{:.2}", scale.unwrap()),
                "--refresh".to_string(),
                format!("{:.3}", current_mode["refresh_hz"].as_f64().unwrap_or(60.0)),
                output.to_string(),
                current_mode["width"].as_i64().unwrap().to_string(),
                current_mode["height"].as_i64().unwrap().to_string(),
            ])
        }
        _ => unreachable!("validated display action"),
    }
}

async fn apply_layout(
    owner_uid: u32,
    content: &[u8],
    environment: &DisplayEnvironment,
) -> Result<Value, String> {
    let text = std::str::from_utf8(content).map_err(|_| "display KDL must be UTF-8".to_string())?;
    let parsed = parse_outputs(text)?;
    let before_kdl = list_kdl(environment).await?;
    let current = parse_outputs(&before_kdl)?;
    let enabled_matches_current = parsed.iter().any(|submitted| {
        submitted["enabled"].as_bool() == Some(true)
            && current.iter().any(|existing| {
                existing["name"] == submitted["name"]
                    && existing["make"] == submitted["make"]
                    && existing["model"] == submitted["model"]
            })
    });
    if !enabled_matches_current {
        return Err(
            "display layout must leave at least one currently connected output enabled".to_string(),
        );
    }
    let backup = prepare_layout_backup(owner_uid, "apply-layout", &before_kdl)?;
    let command = run_user_command(
        cosmic_randr_path()?,
        vec!["kdl".to_string()],
        environment.clone(),
        Some(content.to_vec()),
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("cosmic-randr", &command)?;
    finish_layout_backup(backup, environment).await
}

fn prepare_layout_backup(
    owner_uid: u32,
    action: &str,
    before_kdl: &str,
) -> Result<DisplayBackup, String> {
    let backup = DisplayBackup {
        token: uuid::Uuid::new_v4().simple().to_string(),
        owner_uid,
        created_at: chrono::Utc::now().to_rfc3339(),
        action: action.to_string(),
        state: DisplayBackupState::Layout {
            before_kdl: before_kdl.to_string(),
            applied_sha256: String::new(),
        },
        status: "prepared".to_string(),
    };
    save_backup(&backup)?;
    Ok(backup)
}

async fn finish_layout_backup(
    mut backup: DisplayBackup,
    environment: &DisplayEnvironment,
) -> Result<Value, String> {
    let after_kdl = match list_kdl(environment).await {
        Ok(after_kdl) => after_kdl,
        Err(error) => return rollback_layout_failure(backup, environment, error).await,
    };
    let hash = sha256(after_kdl.as_bytes());
    let before = match &backup.state {
        DisplayBackupState::Layout { before_kdl, .. } => before_kdl.clone(),
        _ => unreachable!(),
    };
    backup.state = DisplayBackupState::Layout {
        before_kdl: before.clone(),
        applied_sha256: hash.clone(),
    };
    backup.status = "applied".to_string();
    if let Err(error) = save_backup(&backup) {
        return rollback_layout_failure(backup, environment, error).await;
    }
    Ok(json!({
        "action": backup.action,
        "changed": before != after_kdl,
        "backup_token": backup.token,
        "before": parse_outputs(&before)?,
        "after": parse_outputs(&after_kdl)?,
        "applied_sha256": hash,
    }))
}

async fn rollback_layout_failure(
    mut backup: DisplayBackup,
    environment: &DisplayEnvironment,
    error: String,
) -> Result<Value, String> {
    let before_kdl = match &backup.state {
        DisplayBackupState::Layout { before_kdl, .. } => before_kdl.clone(),
        _ => return Err(error),
    };
    let rollback = async {
        let command = run_user_command(
            cosmic_randr_path()?,
            vec!["kdl".to_string()],
            environment.clone(),
            Some(before_kdl.as_bytes().to_vec()),
            TOOL_TIMEOUT,
        )
        .await?;
        require_success("cosmic-randr rollback", &command)?;
        let restored = list_kdl(environment).await?;
        if sha256(restored.as_bytes()) != sha256(before_kdl.as_bytes()) {
            return Err("rolled-back layout does not match the backup".to_string());
        }
        Ok::<(), String>(())
    }
    .await;
    match rollback {
        Ok(()) => {
            backup.status = "auto-rolled-back".to_string();
            let metadata_error = save_backup(&backup).err();
            Err(format!(
                "display mutation failed after applying and the previous layout was restored: {error}{}",
                metadata_error
                    .map(|metadata_error| format!("; backup metadata update also failed: {metadata_error}"))
                    .unwrap_or_default()
            ))
        }
        Err(rollback_error) => {
            backup.status = "rollback-failed".to_string();
            let metadata_error = save_backup(&backup).err();
            Ok(json!({
                "action": backup.action,
                "action_applied": true,
                "backup_token": backup.token,
                "post_state_error": error,
                "rollback_error": rollback_error,
                "metadata_error": metadata_error,
            }))
        }
    }
}

async fn set_brightness(owner_uid: u32, device: &str, percent: u64) -> Result<Value, String> {
    validate_backlight_name(device)?;
    let before = backlight_state(device)?;
    let maximum = before["maximum"].as_u64().unwrap();
    let current = before["current"].as_u64().unwrap();
    let backup = DisplayBackup {
        token: uuid::Uuid::new_v4().simple().to_string(),
        owner_uid,
        created_at: chrono::Utc::now().to_rfc3339(),
        action: "brightness".to_string(),
        state: DisplayBackupState::Brightness {
            device: device.to_string(),
            before: current,
            applied: 0,
            maximum,
        },
        status: "prepared".to_string(),
    };
    save_backup(&backup)?;
    let value = format!("{percent}%");
    let command = run_root_command(
        brightnessctl_path()?,
        vec![
            "--device".to_string(),
            device.to_string(),
            "set".to_string(),
            value,
        ],
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("brightnessctl", &command)?;
    let after = match backlight_state(device) {
        Ok(after) => after,
        Err(error) => return rollback_brightness_failure(backup, error).await,
    };
    let mut backup = backup;
    backup.state = DisplayBackupState::Brightness {
        device: device.to_string(),
        before: current,
        applied: after["current"].as_u64().unwrap(),
        maximum,
    };
    backup.status = "applied".to_string();
    if let Err(error) = save_backup(&backup) {
        return rollback_brightness_failure(backup, error).await;
    }
    Ok(json!({
        "action": "brightness",
        "changed": before != after,
        "backup_token": backup.token,
        "before": before,
        "after": after,
    }))
}

async fn rollback_brightness_failure(
    mut backup: DisplayBackup,
    error: String,
) -> Result<Value, String> {
    let (device, before, maximum) = match &backup.state {
        DisplayBackupState::Brightness {
            device,
            before,
            maximum,
            ..
        } => (device.clone(), *before, *maximum),
        _ => return Err(error),
    };
    let rollback = async {
        let command = run_root_command(
            brightnessctl_path()?,
            vec![
                "--device".to_string(),
                device.clone(),
                "set".to_string(),
                before.to_string(),
            ],
            None,
            TOOL_TIMEOUT,
        )
        .await?;
        require_success("brightnessctl rollback", &command)?;
        let restored = backlight_state(&device)?;
        if restored["current"].as_u64() != Some(before)
            || restored["maximum"].as_u64() != Some(maximum)
        {
            return Err("rolled-back backlight state does not match the backup".to_string());
        }
        Ok::<(), String>(())
    }
    .await;
    match rollback {
        Ok(()) => {
            backup.status = "auto-rolled-back".to_string();
            let metadata_error = save_backup(&backup).err();
            Err(format!(
                "brightness mutation failed after applying and the previous value was restored: {error}{}",
                metadata_error
                    .map(|metadata_error| format!("; backup metadata update also failed: {metadata_error}"))
                    .unwrap_or_default()
            ))
        }
        Err(rollback_error) => {
            backup.status = "rollback-failed".to_string();
            let metadata_error = save_backup(&backup).err();
            Ok(json!({
                "action": "brightness",
                "action_applied": true,
                "backup_token": backup.token,
                "post_state_error": error,
                "rollback_error": rollback_error,
                "metadata_error": metadata_error,
            }))
        }
    }
}

async fn restore_display(
    owner_uid: u32,
    token: &str,
    environment: &DisplayEnvironment,
) -> Result<Value, String> {
    validate_token(token)?;
    let mut backup = load_backup(token)?;
    if backup.owner_uid != owner_uid {
        return Err("display backup belongs to another user".to_string());
    }
    if !matches!(backup.status.as_str(), "applied" | "rollback-failed") {
        return Err(format!("display backup is not applied: {}", backup.status));
    }
    match &backup.state {
        DisplayBackupState::Layout {
            before_kdl,
            applied_sha256,
        } => {
            if backup.status == "applied" {
                let current = list_kdl(environment).await?;
                if sha256(current.as_bytes()) != *applied_sha256 {
                    return Err("display layout changed after this backup was created".to_string());
                }
            }
            let command = run_user_command(
                cosmic_randr_path()?,
                vec!["kdl".to_string()],
                environment.clone(),
                Some(before_kdl.as_bytes().to_vec()),
                TOOL_TIMEOUT,
            )
            .await?;
            require_success("cosmic-randr", &command)?;
            let restored = list_kdl(environment).await?;
            if sha256(restored.as_bytes()) != sha256(before_kdl.as_bytes()) {
                return Err(
                    "display restore completed but layout does not match backup".to_string()
                );
            }
        }
        DisplayBackupState::Brightness {
            device,
            before,
            applied,
            maximum,
        } => {
            if backup.status == "applied" {
                let current = backlight_state(device)?;
                if current["current"].as_u64() != Some(*applied)
                    || current["maximum"].as_u64() != Some(*maximum)
                {
                    return Err("backlight state changed after this backup was created".to_string());
                }
            }
            let command = run_root_command(
                brightnessctl_path()?,
                vec![
                    "--device".to_string(),
                    device.clone(),
                    "set".to_string(),
                    before.to_string(),
                ],
                None,
                TOOL_TIMEOUT,
            )
            .await?;
            require_success("brightnessctl", &command)?;
            let restored = backlight_state(device)?;
            if restored["current"].as_u64() != Some(*before)
                || restored["maximum"].as_u64() != Some(*maximum)
            {
                return Err("display restore did not restore the backlight backup".to_string());
            }
        }
    }
    backup.status = "restored".to_string();
    save_backup(&backup)?;
    Ok(json!({
        "restored": true,
        "backup_token": token,
        "action": backup.action,
    }))
}

async fn list_kdl(environment: &DisplayEnvironment) -> Result<String, String> {
    let output = run_user_command(
        cosmic_randr_path()?,
        vec!["list".to_string(), "--kdl".to_string()],
        environment.clone(),
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("cosmic-randr", &output)?;
    Ok(output.stdout)
}

fn parse_outputs(kdl: &str) -> Result<Vec<Value>, String> {
    let mut outputs = Vec::new();
    let mut current = None::<serde_json::Map<String, Value>>;
    let mut modes = Vec::new();
    for raw in kdl.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("output \"") {
            if let Some(mut output) = current.take() {
                output.insert(
                    "modes".to_string(),
                    Value::Array(std::mem::take(&mut modes)),
                );
                outputs.push(Value::Object(output));
            }
            let (name, rest) = rest
                .split_once('"')
                .ok_or_else(|| "invalid output header in cosmic-randr KDL".to_string())?;
            validate_output_name(name)?;
            let enabled = rest.contains("enabled=#true");
            let mut output = serde_json::Map::new();
            output.insert("name".to_string(), Value::String(name.to_string()));
            output.insert("enabled".to_string(), Value::Bool(enabled));
            output.insert("make".to_string(), Value::String(String::new()));
            output.insert("model".to_string(), Value::String(String::new()));
            current = Some(output);
        } else if let Some(output) = current.as_mut() {
            if line.starts_with("description") {
                if let Some(make) = quoted_attribute(line, "make") {
                    output.insert("make".to_string(), Value::String(make));
                }
                if let Some(model) = quoted_attribute(line, "model") {
                    output.insert("model".to_string(), Value::String(model));
                }
            } else if let Some(rest) = line.strip_prefix("position ") {
                let values = rest.split_whitespace().collect::<Vec<_>>();
                if values.len() == 2 {
                    output.insert(
                        "position".to_string(),
                        json!([values[0].parse::<i64>().ok(), values[1].parse::<i64>().ok()]),
                    );
                }
            } else if let Some(value) = line.strip_prefix("scale ") {
                output.insert(
                    "scale".to_string(),
                    Value::from(value.parse::<f64>().unwrap_or(1.0)),
                );
            } else if let Some(value) = quoted_property(line, "mirroring") {
                output.insert("mirroring".to_string(), Value::String(value));
            } else if let Some(value) = quoted_property(line, "transform") {
                output.insert("transform".to_string(), Value::String(value));
            } else if let Some(value) = quoted_property(line, "serial_number") {
                output.insert("serial_number".to_string(), Value::String(value));
            } else if let Some(rest) = line.strip_prefix("mode ") {
                let fields = rest.split_whitespace().collect::<Vec<_>>();
                if fields.len() >= 3 {
                    modes.push(json!({
                        "width": fields[0].parse::<i64>().ok(),
                        "height": fields[1].parse::<i64>().ok(),
                        "refresh_millihz": fields[2].parse::<i64>().ok(),
                        "refresh_hz": fields[2].parse::<f64>().ok().map(|value| value / 1000.0),
                        "current": fields.iter().any(|field| *field == "current=#true"),
                        "preferred": fields.iter().any(|field| *field == "preferred=#true"),
                    }));
                }
            }
        }
    }
    if let Some(mut output) = current {
        output.insert("modes".to_string(), Value::Array(modes));
        outputs.push(Value::Object(output));
    }
    if outputs.is_empty() {
        return Err("cosmic-randr returned no parseable outputs".to_string());
    }
    Ok(outputs)
}

fn quoted_property(line: &str, key: &str) -> Option<String> {
    line.strip_prefix(&format!("{key} \""))?
        .strip_suffix('"')
        .map(str::to_string)
}

fn quoted_attribute(line: &str, key: &str) -> Option<String> {
    let marker = format!("{key}=\"");
    let start = line.find(&marker)? + marker.len();
    let mut value = String::new();
    let mut escaped = false;
    for character in line[start..].chars() {
        if escaped {
            value.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(value);
        } else {
            value.push(character);
        }
    }
    None
}

fn ensure_output_exists(outputs: &[Value], name: &str) -> Result<(), String> {
    validate_output_name(name)?;
    if outputs
        .iter()
        .any(|output| output["name"].as_str() == Some(name))
    {
        Ok(())
    } else {
        Err(format!("display output not found: {name}"))
    }
}

fn validate_output_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        Err(format!("invalid output name: {value:?}"))
    } else {
        Ok(())
    }
}

fn backlights() -> Vec<Value> {
    let Ok(entries) = fs::read_dir("/sys/class/backlight") else {
        return Vec::new();
    };
    let mut values = entries
        .flatten()
        .filter_map(|entry| backlight_state(entry.file_name().to_str()?).ok())
        .collect::<Vec<_>>();
    values.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    values
}

fn backlight_state(device: &str) -> Result<Value, String> {
    validate_backlight_name(device)?;
    let path = Path::new("/sys/class/backlight").join(device);
    let current = read_u64(path.join("brightness"))
        .ok_or_else(|| format!("read backlight brightness for {device}"))?;
    let maximum = read_u64(path.join("max_brightness"))
        .ok_or_else(|| format!("read backlight maximum for {device}"))?;
    Ok(json!({
        "name": device,
        "current": current,
        "maximum": maximum,
        "percent": if maximum == 0 { None } else { Some((current as f64 / maximum as f64 * 100.0).round()) },
        "actual": read_u64(path.join("actual_brightness")),
        "type": fs::read_to_string(path.join("type")).ok().map(|value| value.trim().to_string()),
    }))
}

fn validate_backlight_name(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('-')
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        return Err("invalid backlight device name".to_string());
    }
    let path = Path::new("/sys/class/backlight").join(value);
    if path.is_dir() {
        Ok(())
    } else {
        Err(format!("backlight device not found: {value}"))
    }
}

fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn resolve_source(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.len() > 4096
        || !raw.starts_with('/')
        || raw.chars().any(|character| character.is_control())
    {
        return Err("display layout source must be an absolute path".to_string());
    }
    let metadata = fs::symlink_metadata(raw)
        .map_err(|error| format!("inspect display layout source: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("display layout source symlinks are not allowed".to_string());
    }
    fs::canonicalize(raw).map_err(|error| format!("resolve display layout source: {error}"))
}

fn read_source(path: &Path) -> Result<Vec<u8>, String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("open display layout source: {error}"))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect display layout source: {error}"))?;
    if !metadata.is_file() || metadata.len() > MAX_LAYOUT_BYTES {
        return Err(format!(
            "display layout source must be a regular file no larger than {MAX_LAYOUT_BYTES} bytes"
        ));
    }
    let mut content = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut content)
        .map_err(|error| format!("read display layout source: {error}"))?;
    Ok(content)
}

#[derive(Clone)]
struct DisplayEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    wayland_display: String,
    username: String,
}

impl DisplayEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf, peer_pid: u32) -> Result<Self, String> {
        let metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect display home {}: {error}", home.display()))?;
        if metadata.uid() != uid {
            return Err(format!(
                "display home {} belongs to uid {}, expected {uid}",
                home.display(),
                metadata.uid()
            ));
        }
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        let runtime_metadata = fs::symlink_metadata(&runtime_dir)
            .map_err(|error| format!("inspect display runtime: {error}"))?;
        if !runtime_metadata.is_dir()
            || runtime_metadata.file_type().is_symlink()
            || runtime_metadata.uid() != uid
        {
            return Err("display runtime directory is not user-owned".to_string());
        }
        let wayland_display = peer_wayland_display(peer_pid)
            .filter(|value| valid_wayland_display(value))
            .or_else(|| discover_wayland_display(&runtime_dir, uid).ok())
            .ok_or_else(|| "no unique user-owned Wayland socket was found".to_string())?;
        validate_wayland_socket(&runtime_dir, &wayland_display, uid)?;
        Ok(Self {
            uid,
            gid,
            home,
            runtime_dir,
            wayland_display,
            username: username_for_uid(uid)?,
        })
    }
}

fn peer_wayland_display(pid: u32) -> Option<String> {
    let file = fs::File::open(format!("/proc/{pid}/environ")).ok()?;
    let mut reader = file.take(256 * 1024);
    let mut data = Vec::new();
    reader.read_to_end(&mut data).ok()?;
    data.split(|byte| *byte == 0).find_map(|entry| {
        let value = entry.strip_prefix(b"WAYLAND_DISPLAY=")?;
        std::str::from_utf8(value).ok().map(str::to_string)
    })
}

fn discover_wayland_display(runtime_dir: &Path, uid: u32) -> Result<String, String> {
    let mut sockets = fs::read_dir(runtime_dir)
        .map_err(|error| format!("list display runtime: {error}"))?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_string();
            if !valid_wayland_display(&name) {
                return None;
            }
            let metadata = fs::symlink_metadata(entry.path()).ok()?;
            (metadata.file_type().is_socket() && metadata.uid() == uid).then_some(name)
        })
        .collect::<Vec<_>>();
    sockets.sort();
    sockets.dedup();
    match sockets.as_slice() {
        [socket] => Ok(socket.clone()),
        [] => Err("no Wayland socket found".to_string()),
        _ => Err("multiple Wayland sockets found".to_string()),
    }
}

fn validate_wayland_socket(runtime_dir: &Path, display: &str, uid: u32) -> Result<(), String> {
    let path = runtime_dir.join(display);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("inspect Wayland socket {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != uid
    {
        return Err("Wayland socket is not a user-owned Unix socket".to_string());
    }
    Ok(())
}

fn valid_wayland_display(value: &str) -> bool {
    value.starts_with("wayland-")
        && value.len() <= 108
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn username_for_uid(uid: u32) -> Result<String, String> {
    use std::ffi::CStr;
    const BUF_SIZE: usize = 16 * 1024;
    let mut buffer = vec![0 as libc::c_char; BUF_SIZE];
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || passwd.pw_name.is_null() {
        return Err(format!("passwd entry is unavailable for uid {uid}"));
    }
    unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .map(str::to_string)
        .map_err(|_| format!("username is not UTF-8 for uid {uid}"))
}

async fn run_user_command(
    program: &'static str,
    args: Vec<String>,
    environment: DisplayEnvironment,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || {
        run_command_sync(program, args, Some(environment), stdin_data, timeout)
    })
    .await
    .map_err(|error| format!("{program} worker failed: {error}"))?
}

async fn run_root_command(
    program: &'static str,
    args: Vec<String>,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_command_sync(program, args, None, stdin_data, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
    environment: Option<DisplayEnvironment>,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .current_dir("/")
        .stdin(if stdin_data.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let identity = environment.as_ref().map(|environment| {
        command
            .env("HOME", &environment.home)
            .env("USER", &environment.username)
            .env("LOGNAME", &environment.username)
            .env("XDG_RUNTIME_DIR", &environment.runtime_dir)
            .env("WAYLAND_DISPLAY", &environment.wayland_display);
        (environment.uid, environment.gid)
    });
    if environment.is_none() {
        command.env("HOME", "/root");
    }
    unsafe {
        command.pre_exec(move || {
            if let Some((uid, gid)) = identity {
                if libc::setgroups(0, std::ptr::null()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setgid(gid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::setuid(uid) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE as _, &limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {program}: {error}"))?;
    if let Some(data) = stdin_data {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{program} stdin is unavailable"))?;
        stdin
            .write_all(&data)
            .map_err(|error| format!("write {program} input: {error}"))?;
    }
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
    Ok(CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    })
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
            .map_err(|error| format!("read display output: {error}"))?;
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

fn require_success(program: &str, output: &CommandOutput) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited {}: {}",
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ))
    }
}

fn validate_action(
    action: &str,
    output: Option<&str>,
    from: Option<&str>,
    width: Option<i64>,
    height: Option<i64>,
    refresh: Option<f64>,
    scale: Option<f64>,
    x: Option<i64>,
    y: Option<i64>,
    transform: Option<&str>,
    adaptive_sync: Option<&str>,
    source: Option<&str>,
    backlight: Option<&str>,
    percent: Option<u64>,
    token: Option<&str>,
    confirm: bool,
) -> Result<(), String> {
    if let Some(output) = output {
        validate_output_name(output)?;
    }
    if let Some(from) = from {
        validate_output_name(from)?;
    }
    if let Some(transform) = transform {
        if !matches!(
            transform,
            "normal"
                | "rotate90"
                | "rotate180"
                | "rotate270"
                | "flipped"
                | "flipped90"
                | "flipped180"
                | "flipped270"
        ) {
            return Err("invalid display transform".to_string());
        }
    }
    if let Some(adaptive_sync) = adaptive_sync {
        if !matches!(adaptive_sync, "true" | "automatic" | "false") {
            return Err("invalid adaptive-sync mode".to_string());
        }
    }
    if refresh.is_some_and(|value| !(1.0..=1000.0).contains(&value))
        || scale.is_some_and(|value| !(0.5..=4.0).contains(&value))
        || x.is_some_and(|value| !(-32768..=32768).contains(&value))
        || y.is_some_and(|value| !(-32768..=32768).contains(&value))
    {
        return Err("display mode parameters are out of bounds".to_string());
    }
    match action {
        "status"
            if output.is_none()
                && from.is_none()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && scale.is_none()
                && x.is_none()
                && y.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && source.is_none()
                && backlight.is_none()
                && percent.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "enable" | "disable"
            if output.is_some()
                && from.is_none()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && scale.is_none()
                && x.is_none()
                && y.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && source.is_none()
                && backlight.is_none()
                && percent.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "mirror"
            if output.is_some()
                && from.is_some()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && scale.is_none()
                && x.is_none()
                && y.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && source.is_none()
                && backlight.is_none()
                && percent.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "position"
            if output.is_some()
                && x.is_some()
                && y.is_some()
                && from.is_none()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && scale.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && source.is_none()
                && backlight.is_none()
                && percent.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "mode"
            if output.is_some()
                && width.is_some_and(|value| value > 0 && value <= 16384)
                && height.is_some_and(|value| value > 0 && value <= 16384)
                && (x.is_some() == y.is_some())
                && from.is_none()
                && source.is_none()
                && backlight.is_none()
                && percent.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "scale"
            if output.is_some()
                && scale.is_some()
                && from.is_none()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && x.is_none()
                && y.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && source.is_none()
                && backlight.is_none()
                && percent.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "apply-layout"
            if source.is_some()
                && output.is_none()
                && from.is_none()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && scale.is_none()
                && x.is_none()
                && y.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && backlight.is_none()
                && percent.is_none()
                && token.is_none()
                && confirm =>
        {
            Ok(())
        }
        "brightness"
            if backlight.is_some()
                && percent.is_some_and(|value| (1..=100).contains(&value))
                && output.is_none()
                && from.is_none()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && scale.is_none()
                && x.is_none()
                && y.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && source.is_none()
                && token.is_none()
                && !confirm =>
        {
            Ok(())
        }
        "restore"
            if token.is_some_and(|token| validate_token(token).is_ok())
                && output.is_none()
                && from.is_none()
                && width.is_none()
                && height.is_none()
                && refresh.is_none()
                && scale.is_none()
                && x.is_none()
                && y.is_none()
                && transform.is_none()
                && adaptive_sync.is_none()
                && source.is_none()
                && backlight.is_none()
                && percent.is_none()
                && confirm =>
        {
            Ok(())
        }
        _ => Err(format!("invalid arguments for display action {action:?}")),
    }
}

fn save_backup(backup: &DisplayBackup) -> Result<(), String> {
    let dir = backup_dir();
    fs::create_dir_all(&dir)
        .map_err(|error| format!("create display backup directory: {error}"))?;
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("secure display backup directory: {error}"))?;
    let data = serde_json::to_vec_pretty(backup)
        .map_err(|error| format!("serialize display backup: {error}"))?;
    crate::agent::util::atomic_write_with_fsync(&backup_path(&backup.token), &data)
        .map_err(|error| format!("write display backup: {error}"))
}

fn load_backup(token: &str) -> Result<DisplayBackup, String> {
    let data =
        fs::read(backup_path(token)).map_err(|error| format!("read display backup: {error}"))?;
    serde_json::from_slice(&data).map_err(|error| format!("parse display backup: {error}"))
}

fn validate_token(value: &str) -> Result<(), String> {
    if value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("invalid display backup token".to_string())
    }
}

fn backup_dir() -> PathBuf {
    crate::paths::data_dir()
        .join("clawd")
        .join("display-backups")
}

fn backup_path(token: &str) -> PathBuf {
    backup_dir().join(format!("{token}.json"))
}

fn sha256(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn cosmic_randr_path() -> Result<&'static str, String> {
    ["/usr/bin/cosmic-randr", "/usr/local/bin/cosmic-randr"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| "cosmic-randr is not installed".to_string())
}

fn brightnessctl_path() -> Result<&'static str, String> {
    ["/usr/bin/brightnessctl", "/bin/brightnessctl"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| "brightnessctl is not installed".to_string())
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

fn optional_u64(params: &Value, key: &str) -> Result<Option<u64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("parameter `{key}` must be a non-negative integer")),
        Some(Value::String(value)) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("parameter `{key}` must be an integer")),
        Some(_) => Err(format!("parameter `{key}` must be an integer or null")),
    }
}

fn optional_i64(params: &Value, key: &str) -> Result<Option<i64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("parameter `{key}` must be an integer")),
        Some(Value::String(value)) => value
            .parse::<i64>()
            .map(Some)
            .map_err(|_| format!("parameter `{key}` must be an integer")),
        Some(_) => Err(format!("parameter `{key}` must be an integer or null")),
    }
}

fn optional_f64(params: &Value, key: &str) -> Result<Option<f64>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("parameter `{key}` must be a number")),
        Some(Value::String(value)) => value
            .parse::<f64>()
            .map(Some)
            .map_err(|_| format!("parameter `{key}` must be a number")),
        Some(_) => Err(format!("parameter `{key}` must be a number or null")),
    }
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
    use super::*;

    #[test]
    fn parses_cosmic_randr_kdl() {
        let outputs = parse_outputs(
            "output \"eDP-1\" enabled=#true {\n  position 0 0\n  scale 1.00\n  modes {\n    mode 1920 1080 60000 current=#true preferred=#true\n  }\n}\n",
        )
        .unwrap();
        assert_eq!(outputs[0]["name"], "eDP-1");
        assert_eq!(outputs[0]["modes"][0]["refresh_hz"], 60.0);
    }

    #[test]
    fn refuses_last_output_disable_in_validation_inputs() {
        validate_action(
            "disable",
            Some("eDP-1"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        )
        .unwrap();
    }
}
