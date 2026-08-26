use serde_json::{json, Value};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 4 * 1024 * 1024;
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
static CAMERA_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Camera Manager requires Linux PipeWire".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Camera Manager requires root clawd".to_string());
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
        let node_id = optional_u64(&params, "node_id")?;
        let expected_serial = optional_u64(&params, "expected_serial")?;
        let destination = optional_string(&params, "destination")?;
        let format = optional_string(&params, "format")?;
        let width = optional_u64(&params, "width")?.unwrap_or(1280);
        let height = optional_u64(&params, "height")?.unwrap_or(720);
        validate_action(
            &action,
            node_id,
            expected_serial,
            destination.as_deref(),
            format.as_deref(),
            width,
            height,
        )?;
        let destination = destination
            .as_deref()
            .map(resolve_destination)
            .transpose()?;
        let requested = if action == "status" {
            vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("camera"))]
        } else {
            vec![
                Cap::new(Verb::DEVICE_CAMERA, Scope::name("capture")),
                Cap::new(
                    Verb::FS_WRITE,
                    Scope::path(destination.as_ref().unwrap().to_string_lossy().into_owned()),
                ),
            ]
        };
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, &requested)
        })
        .await?;
        let environment = CameraEnvironment::new(uid, gid, home, peer_pid)?;

        if action == "status" {
            return camera_status(&environment).await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            CAMERA_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Camera Manager is busy with another capture".to_string())?;
        capture(
            node_id.unwrap() as u32,
            expected_serial.unwrap(),
            destination.as_deref().unwrap(),
            format.as_deref().unwrap_or("png"),
            width as u32,
            height as u32,
            &environment,
        )
        .await
    }
}

fn authorize_session(session_id: &str, peer_pid: u32, requested: &[Cap]) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("camera-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("camera-manager") {
        return Err("camera access is restricted to the camera-manager App".to_string());
    }
    if session.pending_bind || session.pid == 0 {
        return Err("camera-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "camera-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("camera-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err("camera request did not originate from the authorized session".to_string());
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    for cap in requested {
        if !caps.covers(cap) {
            return Err(format!(
                "camera-manager session lacks {}:{}",
                cap.verb.as_str(),
                cap.scope
            ));
        }
    }
    Ok(())
}

async fn camera_status(environment: &CameraEnvironment) -> Result<Value, String> {
    let nodes = video_nodes(environment).await?;
    let portal = portal_available(environment).await.unwrap_or(false);
    let count = nodes.len();
    Ok(json!({
        "provider": "pipewire",
        "nodes": nodes,
        "count": count,
        "gstreamer": gst_launch_path().is_ok(),
        "xdg_camera_portal": portal,
        "portal_note": "The XDG Camera portal mediates sandboxed sessions; this host-level manager captures directly from the approved PipeWire video node.",
    }))
}

async fn capture(
    node_id: u32,
    expected_serial: u64,
    destination: &Path,
    format: &str,
    width: u32,
    height: u32,
    environment: &CameraEnvironment,
) -> Result<Value, String> {
    let initial = video_node(environment, node_id).await?;
    if initial["serial"].as_u64() != Some(expected_serial) {
        return Err("PipeWire camera node does not match the expected serial".to_string());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "camera destination has no parent".to_string())?;
    let suffix = if format == "png" { ".png" } else { ".jpg" };
    let mut output_file = tempfile::Builder::new()
        .prefix(".claw-camera-")
        .suffix(suffix)
        .tempfile_in(parent)
        .map_err(|error| format!("create camera output file: {error}"))?;
    fs::set_permissions(output_file.path(), fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("secure camera output file: {error}"))?;
    chown(output_file.path(), environment.uid, environment.gid)?;
    let current = video_node(environment, node_id).await?;
    if current["serial"].as_u64() != Some(expected_serial) {
        return Err("PipeWire camera node changed identity before capture".to_string());
    }
    let node = node_id.to_string();
    let caps = format!("video/x-raw,width={width},height={height}");
    let location = format!("location={}", output_file.path().display());
    let mut args = vec![
        "-q".to_string(),
        "pipewiresrc".to_string(),
        format!("path={node}"),
        "num-buffers=1".to_string(),
        "!".to_string(),
        "videoconvert".to_string(),
        "!".to_string(),
        "videoscale".to_string(),
        "!".to_string(),
        caps,
        "!".to_string(),
    ];
    if format == "png" {
        args.extend([
            "pngenc".to_string(),
            "snapshot=true".to_string(),
            "!".to_string(),
        ]);
    } else {
        args.extend([
            "jpegenc".to_string(),
            "quality=90".to_string(),
            "snapshot=true".to_string(),
            "!".to_string(),
        ]);
    }
    args.extend(["filesink".to_string(), location]);
    let command =
        run_user_command(gst_launch_path()?, args, environment.clone(), TOOL_TIMEOUT).await?;
    require_success("gst-launch-1.0", &command)?;
    let metadata = fs::metadata(output_file.path())
        .map_err(|error| format!("inspect captured image: {error}"))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_IMAGE_BYTES {
        return Err("camera capture did not produce a bounded regular image".to_string());
    }
    output_file
        .as_file_mut()
        .sync_all()
        .map_err(|error| format!("fsync captured image: {error}"))?;
    fs::set_permissions(output_file.path(), fs::Permissions::from_mode(0o644))
        .map_err(|error| format!("set captured image permissions: {error}"))?;
    output_file
        .persist_noclobber(destination)
        .map_err(|error| {
            format!(
                "persist camera image {}: {}",
                destination.display(),
                error.error
            )
        })?;
    sync_directory(parent)?;
    Ok(json!({
        "captured": true,
        "node": initial,
        "destination": destination,
        "format": format,
        "width": width,
        "height": height,
        "size_bytes": metadata.len(),
        "stdout_tail": tail(&command.stdout),
        "stderr_tail": tail(&command.stderr),
    }))
}

async fn video_nodes(environment: &CameraEnvironment) -> Result<Vec<Value>, String> {
    let output = run_user_command(
        pw_dump_path()?,
        Vec::new(),
        environment.clone(),
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("pw-dump", &output)?;
    let values: Value = serde_json::from_str(&output.stdout)
        .map_err(|error| format!("parse pw-dump JSON: {error}"))?;
    Ok(values
        .as_array()
        .ok_or_else(|| "pw-dump did not return an array".to_string())?
        .iter()
        .filter_map(normalize_video_node)
        .collect())
}

fn normalize_video_node(value: &Value) -> Option<Value> {
    if !value["type"]
        .as_str()
        .is_some_and(|value| value.ends_with(":Node"))
    {
        return None;
    }
    let props = &value["info"]["props"];
    if prop_string(props, "media.class").as_deref() != Some("Video/Source") {
        return None;
    }
    Some(json!({
        "id": value["id"],
        "serial": prop_u64(props, "object.serial"),
        "name": prop_string(props, "node.name"),
        "description": prop_string(props, "node.description"),
        "nick": prop_string(props, "node.nick"),
        "device_id": prop_u64(props, "device.id"),
        "device_api": prop_string(props, "device.api"),
        "media_role": prop_string(props, "media.role"),
        "state": value["info"]["state"],
    }))
}

async fn video_node(environment: &CameraEnvironment, id: u32) -> Result<Value, String> {
    video_nodes(environment)
        .await?
        .into_iter()
        .find(|node| node["id"].as_u64() == Some(id as u64))
        .ok_or_else(|| format!("PipeWire Video/Source node not found: {id}"))
}

fn prop_string(props: &Value, key: &str) -> Option<String> {
    match props.get(key) {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
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

async fn portal_available(environment: &CameraEnvironment) -> Result<bool, String> {
    let Some(busctl) = tool_path(&["/usr/bin/busctl", "/bin/busctl"]) else {
        return Ok(false);
    };
    let output = run_user_command(
        busctl,
        vec![
            "--user".to_string(),
            "introspect".to_string(),
            "org.freedesktop.portal.Desktop".to_string(),
            "/org/freedesktop/portal/desktop".to_string(),
            "org.freedesktop.portal.Camera".to_string(),
        ],
        environment.clone(),
        TOOL_TIMEOUT,
    )
    .await?;
    Ok(output.status.success())
}

fn resolve_destination(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.len() > 4096
        || !raw.starts_with('/')
        || raw.chars().any(|character| character.is_control())
    {
        return Err("camera destination must be an absolute path".to_string());
    }
    let path = Path::new(raw);
    if path.exists() {
        return Err("camera destination already exists".to_string());
    }
    let parent = fs::canonicalize(
        path.parent()
            .ok_or_else(|| "camera destination has no parent".to_string())?,
    )
    .map_err(|error| format!("resolve camera destination parent: {error}"))?;
    let destination = parent.join(
        path.file_name()
            .ok_or_else(|| "camera destination has no filename".to_string())?,
    );
    if destination != path {
        return Err(format!(
            "use the canonical camera destination: {}",
            destination.display()
        ));
    }
    Ok(destination)
}

#[derive(Clone)]
struct CameraEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    wayland_display: String,
    username: String,
}

impl CameraEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf, peer_pid: u32) -> Result<Self, String> {
        let home_metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect camera home {}: {error}", home.display()))?;
        if home_metadata.uid() != uid {
            return Err(format!(
                "camera home {} belongs to uid {}, expected {uid}",
                home.display(),
                home_metadata.uid()
            ));
        }
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        let runtime_metadata = fs::symlink_metadata(&runtime_dir)
            .map_err(|error| format!("inspect camera runtime: {error}"))?;
        if !runtime_metadata.is_dir()
            || runtime_metadata.file_type().is_symlink()
            || runtime_metadata.uid() != uid
        {
            return Err("camera runtime directory is not user-owned".to_string());
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
        .map_err(|error| format!("list camera runtime: {error}"))?
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
    environment: CameraEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_user_command_sync(program, args, environment, timeout))
        .await
        .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_user_command_sync(
    program: &str,
    args: Vec<String>,
    environment: CameraEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .env("HOME", &environment.home)
        .env("USER", &environment.username)
        .env("LOGNAME", &environment.username)
        .env("LC_ALL", "C.UTF-8")
        .env("XDG_RUNTIME_DIR", &environment.runtime_dir)
        .env("WAYLAND_DISPLAY", &environment.wayland_display)
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            format!("unix:path={}/bus", environment.runtime_dir.display()),
        )
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
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
            .map_err(|error| format!("read camera command output: {error}"))?;
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
    node_id: Option<u64>,
    expected_serial: Option<u64>,
    destination: Option<&str>,
    format: Option<&str>,
    width: u64,
    height: u64,
) -> Result<(), String> {
    if !(16..=7680).contains(&width) || !(16..=4320).contains(&height) {
        return Err("camera dimensions are out of bounds".to_string());
    }
    match action {
        "status"
            if node_id.is_none()
                && expected_serial.is_none()
                && destination.is_none()
                && format.is_none()
                && width == 1280
                && height == 720 =>
        {
            Ok(())
        }
        "capture"
            if node_id.is_some_and(|id| id > 0 && id <= u32::MAX as u64)
                && expected_serial.is_some_and(|serial| serial > 0)
                && destination.is_some()
                && format.is_some_and(|format| matches!(format, "png" | "jpeg")) =>
        {
            Ok(())
        }
        _ => Err(format!("invalid arguments for camera action {action:?}")),
    }
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

fn chown(path: &Path, uid: u32, gid: u32) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "camera output path contains NUL".to_string())?;
    if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
        return Err(format!(
            "chown camera output: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| format!("fsync camera directory {}: {error}", path.display()))
}

fn tool_path(candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
}

fn pw_dump_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/pw-dump", "/bin/pw-dump"])
        .ok_or_else(|| "pw-dump is not installed".to_string())
}

fn gst_launch_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/gst-launch-1.0", "/bin/gst-launch-1.0"])
        .ok_or_else(|| "gst-launch-1.0 is not installed".to_string())
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
        "/test/unit/clawd/camera.rs"
    ));
}
