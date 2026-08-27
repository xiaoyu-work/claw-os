use base64::Engine;
use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLIPBOARD_BYTES: usize = 4 * 1024 * 1024;
const MAX_WRITE_BYTES: u64 = 16 * 1024 * 1024;
static CLIPBOARD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("Clipboard Manager requires Linux Wayland".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Clipboard Manager requires root clawd".to_string());
        }
        let uid = client.require_uid()?;
        let gid = client
            .gid
            .ok_or_else(|| "clawd peer gid is unavailable".to_string())?;
        let home = client.require_home_dir()?;
        let peer_pid = client
            .pid
            .ok_or_else(|| "clawd peer pid is unavailable".to_string())?;
        let action = required_string(&params, "action")?;
        let mime = optional_string(&params, "mime")?;
        let source = optional_string(&params, "source")?;
        let primary = params
            .get("primary")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let confirm = params
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        validate_action(
            &action,
            mime.as_deref(),
            source.as_deref(),
            primary,
            confirm,
        )?;
        let source = source.as_deref().map(resolve_source).transpose()?;
        let requested = requested_caps(&action, source.as_deref());
        let _authorized = authorize_session(authority, &requested)?;
        let environment = ClipboardEnvironment::new(uid, gid, home, peer_pid)?;

        if matches!(action.as_str(), "status" | "types" | "read") {
            return read_action(&action, mime.as_deref(), primary, &environment).await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            CLIPBOARD_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Clipboard Manager is busy with another write".to_string())?;
        if action == "write" {
            write_clipboard(
                source.as_deref().unwrap(),
                mime.as_deref().unwrap_or("text/plain;charset=utf-8"),
                primary,
                &environment,
            )
            .await
        } else {
            clear_clipboard(primary, &environment).await
        }
    }
}

fn requested_caps(action: &str, source: Option<&Path>) -> Vec<Cap> {
    let verb = if matches!(action, "status" | "types" | "read") {
        Verb::CLIPBOARD_READ
    } else {
        Verb::CLIPBOARD_WRITE
    };
    let mut caps = vec![Cap::new(verb, Scope::name("selection"))];
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
    authority.require_app("clipboard-manager")?;
    authority.require_all(requested)
}

async fn read_action(
    action: &str,
    mime: Option<&str>,
    primary: bool,
    environment: &ClipboardEnvironment,
) -> Result<Value, String> {
    match action {
        "status" => {
            let types = clipboard_types(primary, environment).await;
            let portal = portal_clipboard_available(environment).await;
            Ok(json!({
                "provider": "wl-clipboard",
                "selection": if primary { "primary" } else { "clipboard" },
                "types": types.unwrap_or_else(|error| vec![format!("error:{error}")]),
                "xdg_clipboard_portal": portal.unwrap_or(false),
                "portal_note": "The XDG Clipboard portal is scoped to RemoteDesktop sessions; general desktop access uses Wayland data-control.",
            }))
        }
        "types" => Ok(json!({
            "selection": if primary { "primary" } else { "clipboard" },
            "types": clipboard_types(primary, environment).await?,
        })),
        "read" => {
            let mime = mime.unwrap_or("text/plain;charset=utf-8");
            let mut args = vec!["--no-newline", "--type", mime];
            if primary {
                args.push("--primary");
            }
            let output = run_user_command(
                wl_paste_path()?,
                args.iter().map(|value| value.to_string()).collect(),
                environment.clone(),
                None,
                TOOL_TIMEOUT,
            )
            .await?;
            require_success("wl-paste", &output)?;
            if output.stdout_truncated {
                return Err(format!(
                    "clipboard content exceeds the {} byte limit",
                    MAX_CLIPBOARD_BYTES
                ));
            }
            let text = std::str::from_utf8(&output.stdout).ok();
            Ok(json!({
                "selection": if primary { "primary" } else { "clipboard" },
                "mime": mime,
                "size_bytes": output.stdout.len(),
                "text": text,
                "base64": text.is_none().then(|| base64::engine::general_purpose::STANDARD.encode(&output.stdout)),
            }))
        }
        _ => unreachable!("validated clipboard read action"),
    }
}

async fn clipboard_types(
    primary: bool,
    environment: &ClipboardEnvironment,
) -> Result<Vec<String>, String> {
    let mut args = vec!["--list-types".to_string()];
    if primary {
        args.push("--primary".to_string());
    }
    let output = run_user_command(
        wl_paste_path()?,
        args,
        environment.clone(),
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("wl-paste", &output)?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(256)
        .map(str::to_string)
        .collect())
}

async fn write_clipboard(
    source: &Path,
    mime: &str,
    primary: bool,
    environment: &ClipboardEnvironment,
) -> Result<Value, String> {
    let (file, metadata) = open_source(source)?;
    let mut args = vec!["--type".to_string(), mime.to_string()];
    if primary {
        args.push("--primary".to_string());
    }
    let output = run_user_command(
        wl_copy_path()?,
        args,
        environment.clone(),
        Some(file),
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("wl-copy", &output)?;
    Ok(json!({
        "written": true,
        "selection": if primary { "primary" } else { "clipboard" },
        "mime": mime,
        "source": source,
        "size_bytes": metadata.len(),
        "stdout_tail": tail_bytes(&output.stdout),
        "stderr_tail": tail_bytes(&output.stderr),
    }))
}

async fn clear_clipboard(
    primary: bool,
    environment: &ClipboardEnvironment,
) -> Result<Value, String> {
    let mut args = vec!["--clear".to_string()];
    if primary {
        args.push("--primary".to_string());
    }
    let output = run_user_command(
        wl_copy_path()?,
        args,
        environment.clone(),
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    require_success("wl-copy", &output)?;
    Ok(json!({
        "cleared": true,
        "selection": if primary { "primary" } else { "clipboard" },
    }))
}

async fn portal_clipboard_available(environment: &ClipboardEnvironment) -> Result<bool, String> {
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
            "org.freedesktop.portal.Clipboard".to_string(),
        ],
        environment.clone(),
        None,
        TOOL_TIMEOUT,
    )
    .await?;
    Ok(output.status.success())
}

fn open_source(path: &Path) -> Result<(File, fs::Metadata), String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| format!("open clipboard source {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("inspect clipboard source {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "clipboard source must be a regular file no larger than {MAX_WRITE_BYTES} bytes"
        ));
    }
    Ok((file, metadata))
}

fn resolve_source(raw: &str) -> Result<PathBuf, String> {
    if raw.is_empty()
        || raw.len() > 4096
        || !raw.starts_with('/')
        || raw.chars().any(|character| character.is_control())
    {
        return Err("clipboard source must be an absolute path".to_string());
    }
    let metadata = fs::symlink_metadata(raw)
        .map_err(|error| format!("inspect clipboard source {raw:?}: {error}"))?;
    if metadata.file_type().is_symlink() {
        return Err("clipboard source symlinks are not allowed".to_string());
    }
    fs::canonicalize(raw).map_err(|error| format!("resolve clipboard source: {error}"))
}

#[derive(Clone)]
struct ClipboardEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    wayland_display: String,
    username: String,
}

impl ClipboardEnvironment {
    fn new(uid: u32, gid: u32, home: PathBuf, peer_pid: u32) -> Result<Self, String> {
        let home_metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect clipboard home {}: {error}", home.display()))?;
        if home_metadata.uid() != uid {
            return Err(format!(
                "clipboard home {} belongs to uid {}, expected {uid}",
                home.display(),
                home_metadata.uid()
            ));
        }
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        let runtime_metadata = fs::symlink_metadata(&runtime_dir)
            .map_err(|error| format!("inspect Wayland runtime: {error}"))?;
        if !runtime_metadata.is_dir()
            || runtime_metadata.file_type().is_symlink()
            || runtime_metadata.uid() != uid
        {
            return Err("Wayland runtime directory is not user-owned".to_string());
        }
        let wayland_display = peer_wayland_display(peer_pid)
            .filter(|display| valid_wayland_display(display))
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
    let mut data = Vec::new();
    let file = fs::File::open(format!("/proc/{pid}/environ")).ok()?;
    let mut limited = file.take(256 * 1024);
    limited.read_to_end(&mut data).ok()?;
    data.split(|byte| *byte == 0).find_map(|entry| {
        let value = entry.strip_prefix(b"WAYLAND_DISPLAY=")?;
        std::str::from_utf8(value).ok().map(str::to_string)
    })
}

fn discover_wayland_display(runtime_dir: &Path, uid: u32) -> Result<String, String> {
    let mut sockets = fs::read_dir(runtime_dir)
        .map_err(|error| format!("list Wayland runtime: {error}"))?
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
    environment: ClipboardEnvironment,
    stdin_file: Option<File>,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || {
        run_user_command_sync(program, args, environment, stdin_file, timeout)
    })
    .await
    .map_err(|error| format!("{program} worker failed: {error}"))?
}

fn run_user_command_sync(
    program: &str,
    args: Vec<String>,
    environment: ClipboardEnvironment,
    stdin_file: Option<File>,
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
        .stdin(stdin_file.map(Stdio::from).unwrap_or_else(Stdio::null))
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
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    })
}

struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
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
            .map_err(|error| format!("read clipboard output: {error}"))?;
        if read == 0 {
            break;
        }
        let remaining = MAX_CLIPBOARD_BYTES.saturating_sub(kept.len());
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
            tail_bytes(&output.stderr)
        ))
    }
}

fn validate_action(
    action: &str,
    mime: Option<&str>,
    source: Option<&str>,
    _primary: bool,
    confirm: bool,
) -> Result<(), String> {
    if let Some(mime) = mime {
        if mime.is_empty()
            || mime.len() > 255
            || mime.starts_with('-')
            || mime
                .chars()
                .any(|character| character.is_control() || character.is_whitespace())
            || !mime.contains('/')
        {
            return Err("invalid clipboard MIME type".to_string());
        }
    }
    match action {
        "status" | "types" if mime.is_none() && source.is_none() && !confirm => Ok(()),
        "read" if source.is_none() && !confirm => Ok(()),
        "write" if source.is_some() && !confirm => Ok(()),
        "clear" if mime.is_none() && source.is_none() && confirm => Ok(()),
        _ => Err(format!("invalid arguments for clipboard action {action:?}")),
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

fn tool_path(candidates: &[&'static str]) -> Option<&'static str> {
    candidates
        .iter()
        .copied()
        .find(|candidate| Path::new(candidate).is_file())
}

fn wl_paste_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/wl-paste", "/bin/wl-paste"])
        .ok_or_else(|| "wl-paste is not installed".to_string())
}

fn wl_copy_path() -> Result<&'static str, String> {
    tool_path(&["/usr/bin/wl-copy", "/bin/wl-copy"])
        .ok_or_else(|| "wl-copy is not installed".to_string())
}

fn tail_bytes(value: &[u8]) -> String {
    const MAX: usize = 8 * 1024;
    let start = value.len().saturating_sub(MAX);
    String::from_utf8_lossy(&value[start..]).trim().to_string()
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/clipboard.rs"
    ));
}
