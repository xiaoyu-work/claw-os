use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
static DISPLAY_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum DisplayBackupState {
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

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("Display Manager requires Linux".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Display Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
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
        if !matches!(action.as_str(), "status" | "brightness" | "restore") {
            return Err("The active desktop does not provide this output layout effect".to_string());
        }
        let requested = requested_caps(&action, None);
        let _authorized = authorize_session(authority, &requested)?;
        if action == "status" {
            let outputs = super::desktop::output_state(client).await?;
            let backlights = backlights();
            return Ok(json!({
                "output_count": outputs.as_array().map_or(0, Vec::len),
                "outputs": outputs,
                "backlight_count": backlights.len(),
                "backlights": backlights,
                "layout_control_available": false,
            }));
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
            "brightness" => {
                set_brightness(uid, backlight.as_deref().unwrap(), percent.unwrap()).await
            }
            "restore" => restore_display(uid, token.as_deref().unwrap()).await,
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

/// Final provider check, taken against the decision the broker already
/// made.
///
/// The session, its App identity, the process allowed to act under it,
/// and the capabilities it holds all come from the grant `clawd`
/// issued and the middleware resolved. Nothing here re-reads the
/// process registry or re-derives policy, so the two can no longer
/// disagree; the check still runs, because a privileged mutation
/// should be refused twice.
fn authorize_session(authority: &Decision, requested: &[Cap]) -> Result<Authorized, String> {
    authority.require_all(requested)
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
    let DisplayBackupState::Brightness { device, before, maximum, .. } = &backup.state;
    let (device, before, maximum) = (device.clone(), *before, *maximum);
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

async fn run_root_command(
    program: &'static str,
    args: Vec<String>,
    stdin_data: Option<Vec<u8>>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_command_sync(program, args, stdin_data, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
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
    command.env("HOME", "/root");
    unsafe {
        command.pre_exec(move || {
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
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/display.rs"
    ));
}
