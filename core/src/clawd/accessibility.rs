use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, CapSet, Scope, Verb};

use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 1024 * 1024;
static A11Y_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(params: Value, client: &ClientIdentity) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client);
        return Err("Accessibility Manager requires Linux COSMIC".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Accessibility Manager requires root clawd".to_string());
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
        let value = optional_string(&params, "value")?;
        validate_action(&action, value.as_deref())?;
        let requested = if action == "status" {
            Cap::new(Verb::SYS_OBSERVE, Scope::name("accessibility"))
        } else {
            Cap::new(Verb::UI_ACCESSIBILITY, Scope::name("control"))
        };
        crate::paths::with_user_override(uid, home.clone(), async {
            authorize_session(&session_id, peer_pid, requested)
        })
        .await?;
        let environment = A11yEnvironment::new(uid, gid, home, peer_pid)?;

        if action == "status" {
            return accessibility_status(&environment).await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            A11Y_LOCK.get_or_init(|| tokio::sync::Mutex::new(())).lock(),
        )
        .await
        .map_err(|_| "Accessibility Manager is busy with another mutation".to_string())?;
        if action == "screen-reader" {
            set_screen_reader(&environment, value.as_deref().unwrap() == "on").await
        } else {
            run_helper(&environment, &[&action, value.as_deref().unwrap()]).await
        }
    }
}

fn authorize_session(session_id: &str, peer_pid: u32, requested: Cap) -> Result<(), String> {
    let session = crate::proc::session_info_by_id(session_id)
        .ok_or_else(|| format!("accessibility-manager session not found: {session_id}"))?;
    if session.app_id.as_deref() != Some("accessibility-manager") {
        return Err(
            "accessibility control is restricted to the accessibility-manager App".to_string(),
        );
    }
    if session.pending_bind || session.pid == 0 {
        return Err("accessibility-manager session is not bound to a process".to_string());
    }
    let expected_start = session
        .start_time_ticks
        .ok_or_else(|| "accessibility-manager session has no process identity".to_string())?;
    if crate::proc::read_start_time_ticks_pub(session.pid) != Some(expected_start) {
        return Err("accessibility-manager session process identity is stale".to_string());
    }
    if !crate::proc::process_descends_from(peer_pid, session.pid) {
        return Err(
            "accessibility request did not originate from the authorized session".to_string(),
        );
    }
    let mut caps = session.caps.unwrap_or_else(CapSet::new);
    if let Some(transient) = session.transient_caps {
        caps.extend(transient.iter().cloned());
    }
    if !caps.covers(&requested) {
        return Err(format!(
            "accessibility-manager session lacks {}:{}",
            requested.verb.as_str(),
            requested.scope
        ));
    }
    Ok(())
}

async fn accessibility_status(environment: &A11yEnvironment) -> Result<Value, String> {
    let wayland = run_helper(environment, &["status"])
        .await
        .unwrap_or_else(|error| json!({"available": false, "error": error}));
    let atspi = atspi_state(environment)
        .await
        .unwrap_or_else(|error| json!({"available": false, "error": error}));
    Ok(json!({
        "cosmic_wayland": wayland,
        "atspi": atspi,
    }))
}

async fn set_screen_reader(environment: &A11yEnvironment, enabled: bool) -> Result<Value, String> {
    let before = atspi_state(environment).await?;
    let previous_enabled = before["is_enabled"].as_bool().unwrap_or(false);
    let previous_reader = before["screen_reader_enabled"].as_bool().unwrap_or(false);
    set_atspi_property(environment, "IsEnabled", enabled).await?;
    if let Err(error) = set_atspi_property(environment, "ScreenReaderEnabled", enabled).await {
        let rollback = set_atspi_property(environment, "IsEnabled", previous_enabled).await;
        return match rollback {
            Ok(()) => Err(format!(
                "setting ScreenReaderEnabled failed and IsEnabled was restored: {error}"
            )),
            Err(rollback_error) => Err(format!(
                "setting ScreenReaderEnabled failed ({error}) and IsEnabled rollback failed ({rollback_error})"
            )),
        };
    }
    let after = atspi_state(environment).await?;
    if after["is_enabled"].as_bool() != Some(enabled)
        || after["screen_reader_enabled"].as_bool() != Some(enabled)
    {
        let _ = set_atspi_property(environment, "IsEnabled", previous_enabled).await;
        let _ = set_atspi_property(environment, "ScreenReaderEnabled", previous_reader).await;
        return Err("AT-SPI properties did not converge to the requested state".to_string());
    }
    Ok(json!({
        "action": "screen-reader",
        "changed": before != after,
        "before": before,
        "after": after,
    }))
}

async fn atspi_state(environment: &A11yEnvironment) -> Result<Value, String> {
    Ok(json!({
        "available": true,
        "is_enabled": get_atspi_property(environment, "IsEnabled").await?,
        "screen_reader_enabled": get_atspi_property(environment, "ScreenReaderEnabled").await?,
    }))
}

async fn get_atspi_property(environment: &A11yEnvironment, property: &str) -> Result<bool, String> {
    let output = run_user_command(
        busctl_path()?,
        vec![
            "--user".to_string(),
            "get-property".to_string(),
            "org.a11y.Bus".to_string(),
            "/org/a11y/bus".to_string(),
            "org.a11y.Status".to_string(),
            property.to_string(),
        ],
        environment.clone(),
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("busctl", &output)?;
    parse_busctl_bool(&output.stdout)
        .ok_or_else(|| format!("unexpected AT-SPI {property} response"))
}

async fn set_atspi_property(
    environment: &A11yEnvironment,
    property: &str,
    value: bool,
) -> Result<(), String> {
    let output = run_user_command(
        busctl_path()?,
        vec![
            "--user".to_string(),
            "set-property".to_string(),
            "org.a11y.Bus".to_string(),
            "/org/a11y/bus".to_string(),
            "org.a11y.Status".to_string(),
            property.to_string(),
            "b".to_string(),
            value.to_string(),
        ],
        environment.clone(),
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("busctl", &output)
}

fn parse_busctl_bool(output: &str) -> Option<bool> {
    match output.split_whitespace().nth(1)? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

async fn run_helper(environment: &A11yEnvironment, args: &[&str]) -> Result<Value, String> {
    let mut helper_args = vec!["--a11y-wayland-helper".to_string()];
    helper_args.extend(args.iter().map(|value| value.to_string()));
    let output = run_user_command(
        PathBuf::from("/proc/self/exe"),
        helper_args,
        environment.clone(),
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("a11y Wayland helper", &output)?;
    serde_json::from_str(output.stdout.trim())
        .map_err(|error| format!("parse a11y helper JSON: {error}"))
}

fn validate_action(action: &str, value: Option<&str>) -> Result<(), String> {
    match action {
        "status" if value.is_none() => Ok(()),
        "screen-reader" | "magnifier" | "invert" if matches!(value, Some("on" | "off")) => Ok(()),
        "filter"
            if matches!(
                value,
                Some("off" | "greyscale" | "protanopia" | "deuteranopia" | "tritanopia")
            ) =>
        {
            Ok(())
        }
        "status" => Err("status does not accept a value".to_string()),
        "screen-reader" | "magnifier" | "invert" => Err(format!("{action} requires on|off")),
        "filter" => {
            Err("filter requires off|greyscale|protanopia|deuteranopia|tritanopia".to_string())
        }
        _ => Err(format!("unknown accessibility action: {action}")),
    }
}

#[derive(Clone)]
struct A11yEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    wayland_display: String,
    username: String,
}

impl A11yEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf, peer_pid: u32) -> Result<Self, String> {
        let metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect accessibility home {}: {error}", home.display()))?;
        if metadata.uid() != uid {
            return Err(format!(
                "accessibility home {} belongs to uid {}, expected {uid}",
                home.display(),
                metadata.uid()
            ));
        }
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        let runtime_metadata = fs::symlink_metadata(&runtime_dir)
            .map_err(|error| format!("inspect accessibility runtime: {error}"))?;
        if !runtime_metadata.is_dir()
            || runtime_metadata.file_type().is_symlink()
            || runtime_metadata.uid() != uid
        {
            return Err("accessibility runtime directory is not user-owned".to_string());
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
        .map_err(|error| format!("list accessibility runtime: {error}"))?
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

async fn run_user_command<P>(
    program: P,
    args: Vec<String>,
    environment: A11yEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String>
where
    P: Into<PathBuf>,
{
    let program = program.into();
    tokio::task::spawn_blocking(move || run_user_command_sync(program, args, environment, timeout))
        .await
        .map_err(|error| format!("accessibility worker failed: {error}"))?
}

fn run_user_command_sync(
    program: PathBuf,
    args: Vec<String>,
    environment: A11yEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = Command::new(&program);
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
        .map_err(|error| format!("failed to launch {}: {error}", program.display()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("{} stdout is unavailable", program.display()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("{} stderr is unavailable", program.display()))?;
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
                break child.wait().map_err(|error| {
                    format!("wait for timed-out {}: {error}", program.display())
                })?;
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for {}: {error}", program.display()));
            }
        }
    };
    let (stdout, stdout_truncated) = stdout_reader
        .join()
        .map_err(|_| format!("{} stdout reader panicked", program.display()))??;
    let (stderr, stderr_truncated) = stderr_reader
        .join()
        .map_err(|_| format!("{} stderr reader panicked", program.display()))??;
    if timed_out {
        return Err(format!(
            "{} timed out after {}s",
            program.display(),
            timeout.as_secs()
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
            .map_err(|error| format!("read accessibility output: {error}"))?;
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

fn busctl_path() -> Result<&'static str, String> {
    ["/usr/bin/busctl", "/bin/busctl"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
        .ok_or_else(|| "busctl is not installed".to_string())
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
        "/test/unit/clawd/accessibility.rs"
    ));
}
