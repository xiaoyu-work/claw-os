use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 4 * 1024 * 1024;
static AUDIO_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Audio Manager requires Linux PipeWire".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Audio Manager requires root clawd".to_string());
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
        let target = optional_string(&params, "target")?;
        let value = optional_string(&params, "value")?;
        validate_action(&action, target.as_deref(), value.as_deref())?;
        let requested = requested_caps(&action);
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, &requested)
        })
        .await?;
        let environment = AudioEnvironment::for_user(uid, gid, home)?;

        if action == "status" {
            return audio_status(&environment).await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            AUDIO_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Audio Manager is busy with another operation".to_string())?;
        mutate(&action, target.as_deref(), value.as_deref(), &environment).await
    }
}

fn requested_caps(action: &str) -> Vec<Cap> {
    match action {
        "status" => vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("audio"))],
        "output-volume" | "output-mute" => {
            vec![Cap::new(Verb::DEVICE_AUDIO, Scope::name("output"))]
        }
        "input-volume" | "input-mute" => {
            vec![Cap::new(Verb::DEVICE_MICROPHONE, Scope::name("input"))]
        }
        "output-default" | "input-default" | "output-route" | "input-route" | "profile" => {
            vec![Cap::new(Verb::DEVICE_MEDIA_ROUTE, Scope::name("pipewire"))]
        }
        _ => Vec::new(),
    }
}

fn authorize_session(session_id: &str, peer_pid: u32, requested: &[Cap]) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("audio-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("audio-manager") {
        return Err("audio control is restricted to the audio-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("audio-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "audio-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("audio-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("audio request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    for cap in requested {
        if !caps.covers(cap) {
            return Err(format!(
                "audio-manager session lacks {}:{}",
                cap.verb.as_str(),
                cap.scope
            ));
        }
    }
    Ok(())
}

async fn audio_status(environment: &AudioEnvironment) -> Result<Value, String> {
    let objects = pipewire_objects(environment).await?;
    let raw_status = run_wpctl(environment, &["status", "--name"], ChildPolicy::default()).await?;
    let output_default = default_state(environment, Direction::Output)
        .await
        .unwrap_or_else(|error| json!({"available": false, "error": error}));
    let input_default = default_state(environment, Direction::Input)
        .await
        .unwrap_or_else(|error| json!({"available": false, "error": error}));
    let devices = objects
        .iter()
        .filter(|object| object["kind"] == "device")
        .cloned()
        .collect::<Vec<_>>();
    let sinks = objects
        .iter()
        .filter(|object| object["media_class"] == "Audio/Sink")
        .cloned()
        .collect::<Vec<_>>();
    let sources = objects
        .iter()
        .filter(|object| object["media_class"] == "Audio/Source")
        .cloned()
        .collect::<Vec<_>>();
    let streams = objects
        .iter()
        .filter(|object| {
            object["media_class"]
                .as_str()
                .is_some_and(|class| class.starts_with("Stream/"))
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({
        "provider": "wireplumber",
        "defaults": {
            "output": output_default,
            "input": input_default,
        },
        "devices": devices,
        "sinks": sinks,
        "sources": sources,
        "streams": streams,
        "raw_status": raw_status.stdout,
        "raw_status_truncated": raw_status.stdout_truncated,
        "raw_status_stderr_truncated": raw_status.stderr_truncated,
    }))
}

async fn mutate(
    action: &str,
    target: Option<&str>,
    value: Option<&str>,
    environment: &AudioEnvironment,
) -> Result<Value, String> {
    let mut guard = None;
    let (before, args) = match action {
        "output-volume" => {
            let before = volume_state(environment, Direction::Output).await?;
            let percent = value.unwrap();
            (
                before,
                vec![
                    "set-volume".to_string(),
                    Direction::Output.special().to_string(),
                    format!("{percent}%"),
                    "--limit".to_string(),
                    "1.5".to_string(),
                ],
            )
        }
        "input-volume" => {
            let before = volume_state(environment, Direction::Input).await?;
            let percent = value.unwrap();
            (
                before,
                vec![
                    "set-volume".to_string(),
                    Direction::Input.special().to_string(),
                    format!("{percent}%"),
                    "--limit".to_string(),
                    "1.0".to_string(),
                ],
            )
        }
        "output-mute" | "input-mute" => {
            let direction = if action.starts_with("output") {
                Direction::Output
            } else {
                Direction::Input
            };
            let before = volume_state(environment, direction).await?;
            let state = match value.unwrap() {
                "on" => "1",
                "off" => "0",
                "toggle" => "toggle",
                _ => unreachable!("validated mute state"),
            };
            (
                before,
                vec![
                    "set-mute".to_string(),
                    direction.special().to_string(),
                    state.to_string(),
                ],
            )
        }
        "output-default" | "input-default" => {
            let direction = if action.starts_with("output") {
                Direction::Output
            } else {
                Direction::Input
            };
            let id = parse_id(target.unwrap())?;
            let object = require_media_class(environment, id, direction.media_class()).await?;
            guard = Some(ObjectGuard::from_object(&object)?);
            let before = default_state(environment, direction).await?;
            (before, vec!["set-default".to_string(), id.to_string()])
        }
        "output-route" | "input-route" => {
            let direction = if action.starts_with("output") {
                Direction::Output
            } else {
                Direction::Input
            };
            let id = parse_id(target.unwrap())?;
            let object = require_media_class(environment, id, direction.media_class()).await?;
            guard = Some(ObjectGuard::from_object(&object)?);
            let route = parse_index(value.unwrap(), "route")?;
            let before = inspect_state(environment, id).await?;
            (
                before,
                vec!["set-route".to_string(), id.to_string(), route.to_string()],
            )
        }
        "profile" => {
            let id = parse_id(target.unwrap())?;
            let object = require_audio_device(environment, id).await?;
            guard = Some(ObjectGuard::from_object(&object)?);
            let profile = parse_index(value.unwrap(), "profile")?;
            let before = inspect_state(environment, id).await?;
            (
                before,
                vec![
                    "set-profile".to_string(),
                    id.to_string(),
                    profile.to_string(),
                ],
            )
        }
        _ => unreachable!("validated audio action"),
    };
    if let Some(guard) = guard {
        guard.verify(environment).await?;
    }
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let output = run_wpctl(environment, &refs, ChildPolicy::default()).await?;
    let after = match action {
        "output-volume" | "output-mute" => volume_state(environment, Direction::Output).await,
        "input-volume" | "input-mute" => volume_state(environment, Direction::Input).await,
        "output-default" => default_state(environment, Direction::Output).await,
        "input-default" => default_state(environment, Direction::Input).await,
        "output-route" | "input-route" | "profile" => {
            inspect_state(environment, parse_id(target.unwrap())?).await
        }
        _ => unreachable!("validated audio action"),
    };
    let after = match after {
        Ok(after) => after,
        Err(error) => {
            return Ok(json!({
                "action": action,
                "target": target,
                "value": value,
                "changed": Value::Null,
                "action_applied": true,
                "before": before,
                "stdout_tail": tail(&output.stdout),
                "stderr_tail": tail(&output.stderr),
                "stdout_truncated": output.stdout_truncated,
                "stderr_truncated": output.stderr_truncated,
                "post_state_error": error,
            }));
        }
    };
    Ok(json!({
        "action": action,
        "target": target,
        "value": value,
        "changed": before != after,
        "action_applied": true,
        "before": before,
        "after": after,
        "stdout_tail": tail(&output.stdout),
        "stderr_tail": tail(&output.stderr),
        "stdout_truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
    }))
}

#[derive(Clone, Copy)]
enum Direction {
    Output,
    Input,
}

impl Direction {
    fn special(self) -> &'static str {
        match self {
            Self::Output => "@DEFAULT_AUDIO_SINK@",
            Self::Input => "@DEFAULT_AUDIO_SOURCE@",
        }
    }

    fn media_class(self) -> &'static str {
        match self {
            Self::Output => "Audio/Sink",
            Self::Input => "Audio/Source",
        }
    }
}

async fn default_state(
    environment: &AudioEnvironment,
    direction: Direction,
) -> Result<Value, String> {
    let inspect = run_wpctl(
        environment,
        &["inspect", direction.special()],
        ChildPolicy::default(),
    )
    .await?;
    let volume = volume_state(environment, direction).await?;
    Ok(json!({
        "id": parse_inspect_id(&inspect.stdout),
        "properties": parse_inspect_properties(&inspect.stdout),
        "volume": volume,
    }))
}

async fn volume_state(
    environment: &AudioEnvironment,
    direction: Direction,
) -> Result<Value, String> {
    let output = run_wpctl(
        environment,
        &["get-volume", direction.special()],
        ChildPolicy::default(),
    )
    .await?;
    parse_volume(&output.stdout)
}

fn parse_volume(output: &str) -> Result<Value, String> {
    let trimmed = output.trim();
    let value = trimmed
        .strip_prefix("Volume:")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<f64>().ok())
        .ok_or_else(|| format!("unexpected wpctl volume output: {trimmed:?}"))?;
    Ok(json!({
        "linear": value,
        "percent": (value * 100.0).round(),
        "muted": trimmed.contains("[MUTED]"),
        "raw": trimmed,
    }))
}

async fn inspect_state(environment: &AudioEnvironment, id: u32) -> Result<Value, String> {
    let output = run_wpctl(
        environment,
        &["inspect", &id.to_string()],
        ChildPolicy::default(),
    )
    .await?;
    Ok(json!({
        "id": parse_inspect_id(&output.stdout).unwrap_or(id as u64),
        "properties": parse_inspect_properties(&output.stdout),
        "raw": output.stdout,
        "truncated": output.stdout_truncated,
        "stderr_truncated": output.stderr_truncated,
    }))
}

fn parse_inspect_id(output: &str) -> Option<u64> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("id ")
            .and_then(|value| value.split(',').next())
            .and_then(|value| value.trim().parse().ok())
    })
}

fn parse_inspect_properties(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.trim().split_once(" = "))
        .map(|(key, value)| {
            (
                key.trim_start_matches('*').trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            )
        })
        .collect()
}

async fn require_media_class(
    environment: &AudioEnvironment,
    id: u32,
    expected: &str,
) -> Result<Value, String> {
    let object = pipewire_object(environment, id).await?;
    if object["media_class"].as_str() != Some(expected) {
        return Err(format!(
            "PipeWire object {id} is {:?}, expected {expected}",
            object["media_class"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(object)
}

async fn require_audio_device(environment: &AudioEnvironment, id: u32) -> Result<Value, String> {
    let object = pipewire_object(environment, id).await?;
    let media_class = object["media_class"].as_str().unwrap_or_default();
    let api = object["device_api"].as_str().unwrap_or_default();
    if object["kind"].as_str() != Some("device")
        || !(media_class == "Audio/Device" || matches!(api, "alsa" | "bluez5"))
    {
        return Err(format!("PipeWire object {id} is not an audio device"));
    }
    Ok(object)
}

struct ObjectGuard {
    id: u32,
    kind: String,
    media_class: Option<String>,
    device_api: Option<String>,
    serial: Option<u64>,
}

impl ObjectGuard {
    fn from_object(object: &Value) -> Result<Self, String> {
        let id = object["id"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| "PipeWire object has no valid id".to_string())?;
        Ok(Self {
            id,
            kind: object["kind"].as_str().unwrap_or_default().to_string(),
            media_class: object["media_class"].as_str().map(str::to_string),
            device_api: object["device_api"].as_str().map(str::to_string),
            serial: object["serial"].as_u64(),
        })
    }

    async fn verify(&self, environment: &AudioEnvironment) -> Result<(), String> {
        let current = pipewire_object(environment, self.id).await?;
        if current["kind"].as_str() != Some(self.kind.as_str())
            || current["media_class"].as_str() != self.media_class.as_deref()
            || current["device_api"].as_str() != self.device_api.as_deref()
            || (self.serial.is_some() && current["serial"].as_u64() != self.serial)
        {
            return Err(format!(
                "PipeWire object {} changed identity before the action",
                self.id
            ));
        }
        Ok(())
    }
}

async fn pipewire_object(environment: &AudioEnvironment, id: u32) -> Result<Value, String> {
    pipewire_objects(environment)
        .await?
        .into_iter()
        .find(|object| object["id"].as_u64() == Some(id as u64))
        .ok_or_else(|| format!("PipeWire object not found: {id}"))
}

async fn pipewire_objects(environment: &AudioEnvironment) -> Result<Vec<Value>, String> {
    let output = run_user_tool(pw_dump_path()?, &[], environment, ChildPolicy::default()).await?;
    let values: Value = serde_json::from_str(&output.stdout)
        .map_err(|error| format!("parse pw-dump JSON: {error}"))?;
    let mut objects = Vec::new();
    for object in values
        .as_array()
        .ok_or_else(|| "pw-dump did not return an array".to_string())?
    {
        let id = object["id"].as_u64();
        let object_type = object["type"].as_str().unwrap_or_default();
        let props = &object["info"]["props"];
        let media_class = prop_string(props, "media.class");
        let kind = if object_type.ends_with(":Device") {
            "device"
        } else if object_type.ends_with(":Node") {
            "node"
        } else {
            "other"
        };
        let device_api = prop_string(props, "device.api");
        let is_audio = match kind {
            "device" => {
                media_class.as_deref() == Some("Audio/Device")
                    || matches!(device_api.as_deref(), Some("alsa" | "bluez5"))
            }
            "node" => media_class.as_deref().is_some_and(|class| {
                class.starts_with("Audio/")
                    || matches!(class, "Stream/Output/Audio" | "Stream/Input/Audio")
            }),
            _ => false,
        };
        if !is_audio {
            continue;
        }
        objects.push(json!({
            "id": id,
            "kind": kind,
            "type": object_type,
            "media_class": media_class,
            "name": prop_string(props, "node.name").or_else(|| prop_string(props, "device.name")),
            "description": prop_string(props, "node.description")
                .or_else(|| prop_string(props, "device.description")),
            "nick": prop_string(props, "node.nick").or_else(|| prop_string(props, "device.nick")),
            "device_api": device_api,
            "serial": prop_u64(props, "object.serial"),
            "application_name": prop_string(props, "application.name"),
            "application_process_id": prop_u64(props, "application.process.id"),
            "state": object["info"]["state"],
        }));
    }
    Ok(objects)
}

fn prop_string(props: &Value, key: &str) -> Option<String> {
    match props.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        Some(Value::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn prop_u64(props: &Value, key: &str) -> Option<u64> {
    match props.get(key) {
        Some(Value::Number(value)) => value.as_u64(),
        Some(Value::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn validate_action(action: &str, target: Option<&str>, value: Option<&str>) -> Result<(), String> {
    match action {
        "status" if target.is_none() && value.is_none() => Ok(()),
        "output-volume" | "input-volume" if target.is_none() => {
            let percent = value
                .ok_or_else(|| format!("{action} requires a percentage"))?
                .parse::<u16>()
                .map_err(|_| format!("{action} percentage must be an integer"))?;
            let maximum = if action == "output-volume" { 150 } else { 100 };
            if percent > maximum {
                return Err(format!("{action} percentage must be 0..{maximum}"));
            }
            Ok(())
        }
        "output-mute" | "input-mute"
            if target.is_none() && matches!(value, Some("on" | "off" | "toggle")) =>
        {
            Ok(())
        }
        "output-default" | "input-default" if value.is_none() => {
            parse_id(target.unwrap_or_default()).map(|_| ())
        }
        "output-route" | "input-route" | "profile"
            if target.is_some() && value.is_some() =>
        {
            parse_id(target.unwrap_or_default())?;
            parse_index(value.unwrap_or_default(), "index").map(|_| ())
        }
        "status" => Err("status does not accept target or value".to_string()),
        "output-volume" | "input-volume" => {
            Err(format!("{action} accepts only a percentage value"))
        }
        "output-mute" | "input-mute" => Err(format!("{action} requires on|off|toggle")),
        "output-default" | "input-default" => Err(format!("{action} requires one node id")),
        "output-route" | "input-route" => {
            Err(format!("{action} requires a node id and route index"))
        }
        "profile" => Err("profile requires a device id and profile index".to_string()),
        _ => Err(format!("unknown audio action: {action}")),
    }
}

fn parse_id(value: &str) -> Result<u32, String> {
    let id = value
        .parse::<u32>()
        .map_err(|_| "PipeWire object id must be a positive integer".to_string())?;
    if id == 0 {
        return Err("PipeWire object id must be positive".to_string());
    }
    Ok(id)
}

fn parse_index(value: &str, kind: &str) -> Result<u32, String> {
    let index = value
        .parse::<u32>()
        .map_err(|_| format!("{kind} index must be an integer"))?;
    if index > 4096 {
        return Err(format!("{kind} index must be 0..4096"));
    }
    Ok(index)
}

#[derive(Clone)]
struct AudioEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    username: String,
}

impl AudioEnvironment {
    fn for_user(uid: u32, gid: u32, home: PathBuf) -> Result<Self, String> {
        let home_metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect audio user home {}: {error}", home.display()))?;
        if home_metadata.uid() != uid {
            return Err(format!(
                "audio user home {} belongs to uid {}, expected {uid}",
                home.display(),
                home_metadata.uid()
            ));
        }
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        let runtime_metadata = fs::symlink_metadata(&runtime_dir).map_err(|error| {
            format!(
                "inspect PipeWire runtime {}: {error}",
                runtime_dir.display()
            )
        })?;
        if !runtime_metadata.is_dir()
            || runtime_metadata.file_type().is_symlink()
            || runtime_metadata.uid() != uid
        {
            return Err(format!(
                "PipeWire runtime {} is not a user-owned directory",
                runtime_dir.display()
            ));
        }
        let username = username_for_uid(uid)?;
        Ok(Self {
            uid,
            gid,
            home,
            runtime_dir,
            username,
        })
    }
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
    let username = unsafe { CStr::from_ptr(passwd.pw_name) }
        .to_str()
        .map_err(|_| format!("username is not UTF-8 for uid {uid}"))?
        .to_string();
    if username.is_empty() {
        return Err(format!("username is empty for uid {uid}"));
    }
    Ok(username)
}

async fn run_wpctl(
    environment: &AudioEnvironment,
    args: &[&str],
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    run_user_tool(wpctl_path()?, args, environment, policy).await
}

async fn run_user_tool(
    program: &'static str,
    args: &[&str],
    environment: &AudioEnvironment,
    policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    let args = args.iter().map(|value| value.to_string()).collect();
    let environment = environment.clone();
    let output =
        tokio::task::spawn_blocking(move || run_command_sync(program, args, environment, policy))
            .await
            .map_err(|error| format!("{program} worker failed: {error}"))??;
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

#[derive(Clone, Copy, Default)]
struct ChildPolicy;

struct CommandOutput {
    status: ExitStatus,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
}

fn run_command_sync(
    program: &str,
    args: Vec<String>,
    environment: AudioEnvironment,
    _policy: ChildPolicy,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(&args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", &environment.home)
        .env("USER", &environment.username)
        .env("LOGNAME", &environment.username)
        .env("LC_ALL", "C.UTF-8")
        .env("XDG_RUNTIME_DIR", &environment.runtime_dir)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/bus", environment.runtime_dir.display()),
        )
        .env(
            "PULSE_SERVER",
            format!("unix:{}/pulse/native", environment.runtime_dir.display()),
        )
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let expected_parent = unsafe { libc::getpid() };
    let uid = environment.uid;
    let gid = environment.gid;
    unsafe {
        command.pre_exec(move || {
            if libc::setgroups(0, std::ptr::null()) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setgid(gid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setuid(uid) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != expected_parent {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "audio broker exited before child setup completed",
                ));
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
    let deadline = Instant::now() + TOOL_TIMEOUT;
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
        return Err(format!(
            "{program} timed out after {}s",
            TOOL_TIMEOUT.as_secs()
        ));
    }
    Ok(CommandOutput {
        status,
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        stdout_truncated,
        stderr_truncated,
    })
}

fn read_bounded(mut reader: impl Read) -> Result<(Vec<u8>, bool), String> {
    let mut kept = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("read child output: {error}"))?;
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

fn validate_optional_string(value: &str, key: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 255
        || value.starts_with('-')
        || value.chars().any(|character| character.is_control())
    {
        return Err(format!("invalid {key}: {value:?}"));
    }
    Ok(())
}

fn optional_string(params: &Value, key: &str) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => {
            validate_optional_string(value, key)?;
            Ok(Some(value.clone()))
        }
        Some(Value::String(_)) => Ok(None),
        Some(_) => Err(format!("parameter `{key}` must be a string or null")),
    }
}

fn required_string(params: &Value, key: &str) -> Result<String, String> {
    optional_string(params, key)?.ok_or_else(|| format!("missing required string parameter: {key}"))
}

fn tool_path(candidates: &[&'static str], name: &str) -> Result<&'static str, String> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
        .ok_or_else(|| format!("{name} is not installed"))
}

fn wpctl_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/wpctl", "/bin/wpctl"], "wpctl")
}

fn pw_dump_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/pw-dump", "/bin/pw-dump"], "pw-dump")
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
    fn volume_parser_handles_mute() {
        let value = parse_volume("Volume: 0.42 [MUTED]\n").unwrap();
        assert_eq!(value["percent"], 42.0);
        assert_eq!(value["muted"], true);
    }

    #[test]
    fn action_validation_is_bounded() {
        validate_action("output-volume", None, Some("150")).unwrap();
        assert!(validate_action("output-volume", None, Some("151")).is_err());
        validate_action("input-mute", None, Some("toggle")).unwrap();
        assert!(validate_action("profile", Some("0"), Some("1")).is_err());
    }

    #[test]
    fn inspect_properties_are_normalized() {
        let properties = parse_inspect_properties(
            "id 42, type PipeWire:Interface:Node/3\n  * media.class = \"Audio/Sink\"\n",
        );
        assert_eq!(properties["media.class"], "Audio/Sink");
    }
}
