use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use crate::caps::{Cap, Scope, Verb};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;

mod settings;

pub(super) async fn start_thunderbird(client: &ClientIdentity) -> Result<(), String> {
    let uid = client.require_uid()?;
    let gid = client.gid.ok_or("Mail owner gid is unavailable")?;
    let home = client.require_home_dir()?;
    let pid = client.pid.ok_or("Mail caller pid is unavailable")?;
    let environment = DesktopEnvironment::for_user(uid, gid, home, pid)?;
    settings::launch_program(
        "/usr/bin/thunderbird", "thunderbird", &[], environment,
    ).await
}

const HELPER_TIMEOUT: Duration = Duration::from_secs(20);
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const STREAM_CAP_BYTES: usize = 2 * 1024 * 1024;
const MAX_LAUNCH_URIS: usize = 32;
const MAX_URI_BYTES: usize = 4096;
static DESKTOP_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (params, client, authority);
        return Err("Desktop Manager requires Linux Wayland".to_string());
    }

    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::geteuid() } != 0 {
            return Err("Desktop Manager requires root clawd".to_string());
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
        let identifier = optional_string(&params, "identifier")?;
        let app_id = optional_string(&params, "app_id")?;
        let uris = optional_string_list(&params, "uris")?;
        validate_action(&action, identifier.as_deref(), app_id.as_deref(), &uris)?;
        match native_launch_target(authority, &action, app_id.as_deref()) {
            Some(NativeLaunchTarget::Terminal) => {
                return open_terminal(&uris, authority, uid, gid, home, peer_pid).await;
            }
            Some(NativeLaunchTarget::Store) => {
                return open_store(&uris, authority, uid, gid, home, peer_pid).await;
            }
            Some(NativeLaunchTarget::Settings) => {
                return open_settings(&uris, authority, uid, gid, home, peer_pid).await;
            }
            Some(NativeLaunchTarget::Files) => {
                return reveal_files(&uris, authority, uid, gid, home, peer_pid).await;
            }
            Some(NativeLaunchTarget::Editor) => {
                return open_editor(&uris, authority, uid, gid, home, peer_pid).await;
            }
            None => {}
        }
        let uris = canonicalize_launch_uris(&uris)?;
        let _authorized = authorize_caller(authority, &action, app_id.as_deref(), &uris)?;
        let environment = DesktopEnvironment::for_user(uid, gid, home, peer_pid)?;

        if action == "list" {
            return run_helper(&environment, &["list"]).await;
        }
        if action == "launch" {
            return launch_desktop_app(
                &environment,
                app_id.as_deref().expect("validated launch app_id"),
                &uris,
            )
            .await;
        }
        let _guard = tokio::time::timeout(
            LOCK_TIMEOUT,
            DESKTOP_LOCK
                .get_or_init(|| tokio::sync::Mutex::new(()))
                .lock(),
        )
        .await
        .map_err(|_| "Desktop Manager is busy with another window action".to_string())?;
        match action.as_str() {
            "focus" | "close" => {
                run_helper(&environment, &[&action, identifier.as_deref().unwrap()]).await
            }
            "restart" => {
                restart(
                    &environment,
                    identifier.as_deref().unwrap(),
                    app_id.as_deref().unwrap(),
                )
                .await
            }
            _ => unreachable!("validated desktop action"),
        }
    }
}

fn requested_caps(action: &str, app_id: Option<&str>, uris: &[String]) -> Result<Vec<Cap>, String> {
    let mut caps = match action {
        "list" => vec![Cap::new(Verb::SYS_OBSERVE, Scope::name("desktop"))],
        "focus" | "close" => vec![Cap::new(Verb::DESKTOP_WINDOW, Scope::name("control"))],
        "launch" => vec![Cap::new(
            Verb::DESKTOP_LAUNCH,
            Scope::name(app_id.unwrap_or_default()),
        )],
        "restart" => vec![
            Cap::new(Verb::DESKTOP_WINDOW, Scope::name("control")),
            Cap::new(
                Verb::DESKTOP_LAUNCH,
                Scope::name(app_id.unwrap_or_default()),
            ),
        ],
        _ => Vec::new(),
    };
    if action == "launch" {
        for uri in uris {
            if let Some(path) = canonical_file_uri_path(uri)? {
                caps.push(Cap::new(Verb::FS_READ, Scope::path(path)));
            }
        }
    }
    Ok(caps)
}

/// Spend the action's exact scopes through the broker's live decision.
/// The authenticated App/owner/session remains attribution, not a business-name
/// entitlement; no process registry or policy is independently reconstructed.
fn authorize_caller(
    authority: &Decision,
    action: &str,
    app_id: Option<&str>,
    uris: &[String],
) -> Result<Authorized, String> {
    authority.require_all(&requested_caps(action, app_id, uris)?)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeLaunchTarget {
    Terminal,
    Store,
    Settings,
    Files,
    Editor,
}

impl NativeLaunchTarget {
    fn spawn_cap(self) -> Cap {
        let name = match self {
            Self::Terminal => "cosmic-term",
            Self::Store => "cosmic-store",
            Self::Settings => "cosmic-settings",
            Self::Files => "cosmic-files",
            Self::Editor => "cosmic-edit",
        };
        Cap::new(Verb::PROC_SPAWN, Scope::name(name))
    }
}

fn native_launch_target(
    authority: &Decision,
    action: &str,
    app_id: Option<&str>,
) -> Option<NativeLaunchTarget> {
    if action != "launch" {
        return None;
    }
    let target = match app_id? {
        "com.clawos.Term" => NativeLaunchTarget::Terminal,
        "com.clawos.Store" => NativeLaunchTarget::Store,
        "com.clawos.Settings" => NativeLaunchTarget::Settings,
        "com.clawos.Files" => NativeLaunchTarget::Files,
        "com.clawos.Edit" => NativeLaunchTarget::Editor,
        _ => return None,
    };
    // This snapshot selects only a compatibility backend. Its live capability
    // spend below is mandatory, and a refusal never falls back to desktop.launch.
    authority.caps().covers(&target.spawn_cap()).then_some(target)
}

fn authorize_native_launch(
    authority: &Decision,
    target: NativeLaunchTarget,
    local_path: Option<&str>,
) -> Result<Authorized, String> {
    let mut caps = vec![target.spawn_cap()];
    match (target, local_path) {
        (NativeLaunchTarget::Files, Some(path)) => {
            caps.push(Cap::new(Verb::FS_META, Scope::path(path)));
        }
        (NativeLaunchTarget::Files, None) => return Err("Files requires a local path".into()),
        (NativeLaunchTarget::Editor, Some(path)) => {
            caps.push(Cap::new(Verb::FS_READ, Scope::path(path)));
        }
        (_, Some(_)) => return Err("native target does not accept a file resource".into()),
        (_, None) => {}
    }
    authority.require_all(&caps)
}

fn editor_launch_path(uris: &[String]) -> Result<Option<String>, String> {
    match uris {
        [] => Ok(None),
        [uri] => canonical_file_uri_path(uri)?
            .ok_or_else(|| "Editor requires a local file URI".into())
            .map(Some),
        _ => Err("Editor accepts at most one local file".into()),
    }
}

async fn open_editor(
    uris: &[String],
    authority: &Decision,
    uid: u32,
    gid: u32,
    home: PathBuf,
    peer_pid: u32,
) -> Result<Value, String> {
    let path = editor_launch_path(uris)?;
    let _authorized =
        authorize_native_launch(authority, NativeLaunchTarget::Editor, path.as_deref())?;
    let environment = DesktopEnvironment::for_user(uid, gid, home, peer_pid)?;
    launch_desktop_app(&environment, "com.clawos.Edit", uris).await
}

fn settings_launch_args(uris: &[String]) -> Result<Vec<String>, String> {
    match uris {
        [] => Ok(vec![]),
        [uri] => {
            let page = uri.strip_prefix("settings://").ok_or("Settings requires a page URI")?;
            if page.is_empty() || page.len() > 64
                || !page.starts_with(|c: char| c.is_ascii_lowercase())
                || !page.bytes().all(|c| c.is_ascii_lowercase() || c == b'-')
            {
                return Err("invalid Settings page identifier".into());
            }
            Ok(vec![page.into()])
        }
        _ => Err("Settings accepts at most one page".into()),
    }
}

async fn open_settings(
    uris: &[String], authority: &Decision, uid: u32, gid: u32,
    home: PathBuf, peer_pid: u32,
) -> Result<Value, String> {
    let args = settings_launch_args(uris)?;
    let _authorized = authorize_native_launch(authority, NativeLaunchTarget::Settings, None)?;
    let environment = DesktopEnvironment::for_user(uid, gid, home, peer_pid)?;
    settings::launch(args, environment).await?;
    Ok(json!({"launched": true, "app_id": "com.clawos.Settings", "launcher": settings::PROGRAM}))
}

fn store_launch_args(uris: &[String]) -> Result<Vec<String>, String> {
    match uris {
        [] => Ok(vec![]),
        [uri] => {
            let name = uri.strip_prefix("apt://").ok_or("Store requires a package name URI")?;
            if name.len() > 255 || !regex::Regex::new(
                r"^[a-z0-9][a-z0-9+.-]*(?::[a-z0-9][a-z0-9-]*)?$",
            ).expect("static package pattern").is_match(name) {
                return Err("invalid Store package name".into());
            }
            Ok(vec![name.into()])
        }
        _ => Err("Store accepts at most one package name".into()),
    }
}

async fn open_store(
    uris: &[String], authority: &Decision, uid: u32, gid: u32,
    home: PathBuf, peer_pid: u32,
) -> Result<Value, String> {
    let args = store_launch_args(uris)?;
    let _authorized = authorize_native_launch(authority, NativeLaunchTarget::Store, None)?;
    let environment = DesktopEnvironment::for_user(uid, gid, home, peer_pid)?;
    let program = PathBuf::from("/usr/bin/cosmic-store");
    let status = run_user_launch_command(program.clone(), args, environment, LAUNCH_TIMEOUT).await?;
    if !status.success() {
        return Err(format!("native Store launcher failed: {status}"));
    }
    Ok(json!({"launched": true, "app_id": "com.clawos.Store", "launcher": program}))
}

fn terminal_launch_args(uris: &[String]) -> Result<Vec<String>, String> {
    match uris {
        [] => Ok(Vec::new()),
        [uri] => {
            let parsed = url::Url::parse(uri).map_err(|_| "invalid Terminal directory URI")?;
            if parsed.scheme() != "file" || parsed.host_str().is_some()
                || parsed.query().is_some() || parsed.fragment().is_some()
            {
                return Err("Terminal requires a local directory URI".into());
            }
            let path = parsed.to_file_path().map_err(|_| "invalid Terminal directory path")?;
            let directory = fs::canonicalize(path).map_err(|error| error.to_string())?;
            if !directory.is_dir() {
                return Err("Terminal working directory must be a directory".into());
            }
            Ok(vec![
                "--working-directory".into(),
                directory.to_str().ok_or("Terminal directory must be UTF-8")?.into(),
            ])
        }
        _ => Err("Terminal accepts at most one working directory".into()),
    }
}

async fn open_terminal(
    uris: &[String], authority: &Decision, uid: u32, gid: u32,
    home: PathBuf, peer_pid: u32,
) -> Result<Value, String> {
    // Preserve the existing manifest scope, not a new desktop.launch grant.
    let _authorized = authorize_native_launch(authority, NativeLaunchTarget::Terminal, None)?;
    let args = {
        let _identity = super::client_identity::FsIdentityGuard::enter(uid)?;
        terminal_launch_args(uris)?
    };
    let environment = DesktopEnvironment::for_user(uid, gid, home, peer_pid)?;
    let program = PathBuf::from("/usr/bin/cosmic-term");
    let status = run_user_launch_command(program.clone(), args, environment, LAUNCH_TIMEOUT).await?;
    if !status.success() {
        return Err(format!("native Terminal launcher failed: {status}"));
    }
    Ok(json!({"launched": true, "app_id": "com.clawos.Term", "launcher": program}))
}

fn files_reveal_path(uris: &[String]) -> Result<String, String> {
    let [uri] = uris else {
        return Err("Files requires exactly one local path".into());
    };
    canonical_local_uri_path(uri, true)?.ok_or_else(|| "Files requires a local file URI".into())
}

async fn reveal_files(
    uris: &[String], authority: &Decision, uid: u32, gid: u32,
    home: PathBuf, peer_pid: u32,
) -> Result<Value, String> {
    let path = files_reveal_path(uris)?;
    let _authorized = authorize_native_launch(authority, NativeLaunchTarget::Files, Some(&path))?;
    let path = Path::new(&path);
    let directory = if path.is_dir() { path } else {
        path.parent().ok_or("file has no containing directory")?
    };
    let uri = url::Url::from_file_path(directory)
        .map_err(|_| "cannot construct Files directory URI")?.to_string();
    let environment = DesktopEnvironment::for_user(uid, gid, home, peer_pid)?;
    let mut result = launch_desktop_app(&environment, "com.clawos.Files", &[uri]).await?;
    result["directory"] = json!(directory);
    Ok(result)
}

async fn restart(
    environment: &DesktopEnvironment,
    identifier: &str,
    app_id: &str,
) -> Result<Value, String> {
    let close = run_helper(environment, &["close-app", identifier, app_id]).await?;
    if close["remaining_count"].as_u64().unwrap_or(1) != 0 {
        return Ok(json!({
            "action": "restart",
            "identifier": identifier,
            "app_id": app_id,
            "action_applied": true,
            "restarted": false,
            "close": close,
            "error": "application still has open windows; relaunch was skipped",
        }));
    }
    let launch = launch_desktop_app(environment, app_id, &[]).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let windows = run_helper(environment, &["list"]).await?;
    let matching = windows["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|window| window["app_id"].as_str() == Some(app_id))
        .cloned()
        .collect::<Vec<_>>();
    Ok(json!({
        "action": "restart",
        "identifier": identifier,
        "app_id": app_id,
        "action_applied": true,
        "restarted": true,
        "close": close,
        "launch": launch,
        "windows": matching,
    }))
}

async fn run_helper(environment: &DesktopEnvironment, args: &[&str]) -> Result<Value, String> {
    let mut helper_args = vec!["--desktop-wayland-helper".to_string()];
    helper_args.extend(args.iter().map(|value| value.to_string()));
    let output = run_user_command(
        PathBuf::from("/proc/self/exe"),
        helper_args,
        environment.clone(),
        HELPER_TIMEOUT,
    )
    .await?;
    if !output.status.success() {
        return Err(format!(
            "desktop Wayland helper exited {}: {}",
            output.status.code().unwrap_or(-1),
            tail(&output.stderr)
        ));
    }
    serde_json::from_str(output.stdout.trim())
        .map_err(|error| format!("parse desktop Wayland helper JSON: {error}"))
}

async fn launch_desktop_app(
    environment: &DesktopEnvironment,
    app_id: &str,
    uris: &[String],
) -> Result<Value, String> {
    let program = PathBuf::from("/usr/bin/gtk4-launch");
    let args = gtk4_launch_args(app_id, uris);
    let status =
        run_user_launch_command(program.clone(), args, environment.clone(), LAUNCH_TIMEOUT).await?;
    if !status.success() {
        return Err(launch_exit_error(status));
    }
    Ok(json!({
        "launched": true,
        "app_id": app_id,
        "launcher": program,
    }))
}

fn gtk4_launch_args(app_id: &str, uris: &[String]) -> Vec<String> {
    let mut args = Vec::with_capacity(uris.len() + 2);
    args.push("--".to_string());
    args.push(app_id.to_string());
    args.extend(uris.iter().cloned());
    args
}

fn launch_exit_error(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("gtk4-launch exited {code}"),
        None => "gtk4-launch was terminated by a signal".to_string(),
    }
}

#[derive(Clone)]
pub(super) struct DesktopEnvironment {
    uid: u32,
    gid: u32,
    home: PathBuf,
    runtime_dir: PathBuf,
    wayland_display: String,
    display: Option<String>,
    xauthority: Option<String>,
    username: String,
}

impl DesktopEnvironment {
    pub(super) fn for_user(uid: u32, gid: u32, home: PathBuf, peer_pid: u32) -> Result<Self, String> {
        let home_metadata = fs::metadata(&home)
            .map_err(|error| format!("inspect desktop user home {}: {error}", home.display()))?;
        if home_metadata.uid() != uid {
            return Err(format!(
                "desktop user home {} belongs to uid {}, expected {uid}",
                home.display(),
                home_metadata.uid()
            ));
        }
        let runtime_dir = PathBuf::from(format!("/run/user/{uid}"));
        let runtime_metadata = fs::symlink_metadata(&runtime_dir).map_err(|error| {
            format!("inspect Wayland runtime {}: {error}", runtime_dir.display())
        })?;
        if !runtime_metadata.is_dir()
            || runtime_metadata.file_type().is_symlink()
            || runtime_metadata.uid() != uid
        {
            return Err(format!(
                "Wayland runtime {} is not a user-owned directory",
                runtime_dir.display()
            ));
        }
        let peer_env = peer_environment(peer_pid)?;
        let wayland_display = peer_env
            .get("WAYLAND_DISPLAY")
            .filter(|value| valid_wayland_display(value))
            .cloned()
            .or_else(|| discover_wayland_display(&runtime_dir, uid).ok())
            .ok_or_else(|| "no unique user-owned Wayland socket was found".to_string())?;
        validate_wayland_socket(&runtime_dir, &wayland_display, uid)?;
        let display = peer_env
            .get("DISPLAY")
            .filter(|value| valid_display(value))
            .cloned();
        let xauthority = peer_env
            .get("XAUTHORITY")
            .filter(|value| valid_environment_path(value))
            .cloned();
        Ok(Self {
            uid,
            gid,
            home,
            runtime_dir,
            wayland_display,
            display,
            xauthority,
            username: username_for_uid(uid)?,
        })
    }
}

fn peer_environment(pid: u32) -> Result<std::collections::BTreeMap<String, String>, String> {
    let file = fs::File::open(format!("/proc/{pid}/environ"))
        .map_err(|error| format!("read desktop request environment: {error}"))?;
    let mut data = Vec::new();
    let mut limited = file.take(256 * 1024);
    limited
        .read_to_end(&mut data)
        .map_err(|error| format!("read desktop request environment: {error}"))?;
    let mut values = std::collections::BTreeMap::new();
    for entry in data.split(|byte| *byte == 0) {
        let Some(separator) = entry.iter().position(|byte| *byte == b'=') else {
            continue;
        };
        let (key, value) = entry.split_at(separator);
        let value = &value[1..];
        let Ok(key) = std::str::from_utf8(key) else {
            continue;
        };
        if !matches!(key, "WAYLAND_DISPLAY" | "DISPLAY" | "XAUTHORITY") {
            continue;
        }
        let Ok(value) = std::str::from_utf8(value) else {
            continue;
        };
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}

fn discover_wayland_display(runtime_dir: &Path, uid: u32) -> Result<String, String> {
    let mut sockets = fs::read_dir(runtime_dir)
        .map_err(|error| format!("list Wayland runtime {}: {error}", runtime_dir.display()))?
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
        _ => Err("multiple Wayland sockets found without WAYLAND_DISPLAY".to_string()),
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
        return Err(format!(
            "Wayland socket {} is not a user-owned Unix socket",
            path.display()
        ));
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

fn valid_display(value: &str) -> bool {
    value.len() <= 64
        && value.starts_with(':')
        && value[1..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

fn valid_environment_path(value: &str) -> bool {
    value.len() <= 4096
        && value.starts_with('/')
        && !value.chars().any(|character| character.is_control())
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

fn validate_action(
    action: &str,
    identifier: Option<&str>,
    app_id: Option<&str>,
    uris: &[String],
) -> Result<(), String> {
    match action {
        "list" if identifier.is_none() && app_id.is_none() && uris.is_empty() => Ok(()),
        "focus" | "close"
            if valid_identifier(identifier) && app_id.is_none() && uris.is_empty() =>
        {
            Ok(())
        }
        "restart" if valid_identifier(identifier) && valid_app_id(app_id) && uris.is_empty() => {
            Ok(())
        }
        "launch" if identifier.is_none() && valid_app_id(app_id) => validate_launch_uris(uris),
        "list" => Err("list does not accept identifier, app_id, or uris".to_string()),
        "focus" | "close" => Err(format!("{action} requires one valid window identifier")),
        "restart" => Err("restart requires a window identifier and exact app_id".to_string()),
        "launch" => Err("launch requires an exact app_id and no identifier".to_string()),
        _ => Err(format!("unknown desktop action: {action}")),
    }
}

fn validate_launch_uris(uris: &[String]) -> Result<(), String> {
    if uris.len() > MAX_LAUNCH_URIS {
        return Err(format!("launch accepts at most {MAX_LAUNCH_URIS} URIs"));
    }
    if uris.iter().any(|uri| {
        uri.is_empty()
            || uri.len() > MAX_URI_BYTES
            || uri.chars().any(|character| character.is_control())
    }) {
        return Err(format!(
            "launch URIs must be non-empty, at most {MAX_URI_BYTES} bytes, \
             and contain no control characters"
        ));
    }
    for uri in uris {
        url::Url::parse(uri).map_err(|_| "launch values must be absolute URIs".to_string())?;
    }
    Ok(())
}

fn canonical_file_uri_path(uri: &str) -> Result<Option<String>, String> {
    canonical_local_uri_path(uri, false)
}

fn canonical_local_uri_path(uri: &str, allow_directory: bool) -> Result<Option<String>, String> {
    let parsed =
        url::Url::parse(uri).map_err(|_| "launch values must be absolute URIs".to_string())?;
    if parsed.scheme() != "file" {
        return Ok(None);
    }
    if parsed.host_str().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
    {
        return Err("file launch URIs must refer to the local host".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("file launch URIs must not contain a query or fragment".to_string());
    }

    let requested = parsed
        .to_file_path()
        .map_err(|_| "file launch URI must contain an absolute local path".to_string())?;
    let canonical = std::fs::canonicalize(&requested)
        .map_err(|error| format!("cannot resolve launch file: {error}"))?;
    if canonical != requested {
        return Err("file launch URI path must already be canonical".to_string());
    }
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| format!("cannot inspect launch file: {error}"))?;
    if !(metadata.is_file() || (allow_directory && metadata.is_dir())) {
        return Err("file launch URI path must be a regular file".to_string());
    }
    let path = canonical
        .to_str()
        .ok_or_else(|| "file launch URI path must be valid UTF-8".to_string())?;
    let canonical_uri = url::Url::from_file_path(&canonical)
        .map_err(|()| "cannot construct canonical file launch URI".to_string())?;
    if canonical_uri.as_str() != uri {
        return Err("file launch URI must already use its canonical form".to_string());
    }
    Ok(Some(path.to_string()))
}

fn canonicalize_launch_uris(uris: &[String]) -> Result<Vec<String>, String> {
    uris.iter()
        .map(|uri| {
            let Some(path) = canonical_file_uri_path(uri)? else {
                return Ok(uri.clone());
            };
            url::Url::from_file_path(path)
                .map(Into::into)
                .map_err(|()| "cannot construct canonical file launch URI".to_string())
        })
        .collect()
}

fn valid_identifier(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        !value.is_empty()
            && value.len() <= 512
            && !value.starts_with('-')
            && !value.chars().any(|character| character.is_control())
    })
}

fn valid_app_id(value: Option<&str>) -> bool {
    value.is_some_and(|value| {
        value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
            && value.len() <= 255
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
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

fn optional_string_list(params: &Value, key: &str) -> Result<Vec<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| format!("parameter `{key}` must contain only strings"))
            })
            .collect(),
        Some(_) => Err(format!("parameter `{key}` must be an array or null")),
    }
}

async fn run_user_command(
    program: PathBuf,
    args: Vec<String>,
    environment: DesktopEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    tokio::task::spawn_blocking(move || run_user_command_sync(program, args, environment, timeout))
        .await
        .map_err(|error| format!("desktop command worker failed: {error}"))?
}

async fn run_user_launch_command(
    program: PathBuf,
    args: Vec<String>,
    environment: DesktopEnvironment,
    timeout: Duration,
) -> Result<ExitStatus, String> {
    tokio::task::spawn_blocking(move || {
        run_user_launch_command_sync(program, args, environment, timeout)
    })
    .await
    .map_err(|error| format!("desktop launch worker failed: {error}"))?
}

fn run_user_launch_command_sync(
    program: PathBuf,
    args: Vec<String>,
    environment: DesktopEnvironment,
    timeout: Duration,
) -> Result<ExitStatus, String> {
    let mut command = configured_user_command(&program, &args, &environment);
    command.stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to launch {}: {error}", program.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                let _ = child.kill();
                child.wait().map_err(|error| {
                    format!("wait for timed-out {}: {error}", program.display())
                })?;
                return Err(format!(
                    "{} timed out after {}s",
                    program.display(),
                    timeout.as_secs()
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("wait for {}: {error}", program.display()));
            }
        }
    }
}

fn run_user_command_sync(
    program: PathBuf,
    args: Vec<String>,
    environment: DesktopEnvironment,
    timeout: Duration,
) -> Result<CommandOutput, String> {
    let mut command = configured_user_command(&program, &args, &environment);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
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

pub(super) fn configured_user_command(
    program: &Path,
    args: &[String],
    environment: &DesktopEnvironment,
) -> Command {
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
        .env("XDG_CURRENT_DESKTOP", "COSMIC")
        .current_dir("/")
        .stdin(Stdio::null());
    if let Some(display) = &environment.display {
        command.env("DISPLAY", display);
    }
    if let Some(xauthority) = &environment.xauthority {
        command.env("XAUTHORITY", xauthority);
    }
    let uid = environment.uid;
    let gid = environment.gid;
    let expected_parent = unsafe { libc::getpid() };
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
                    "desktop broker exited before child setup completed",
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
    command
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
            .map_err(|error| format!("read desktop command output: {error}"))?;
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
        "/test/unit/clawd/desktop.rs"
    ));
}
