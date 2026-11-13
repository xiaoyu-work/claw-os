//! Screen capture authority and portal I/O stay in the OS. Workers receive
//! neither the owner's session bus nor a host temporary-file path.

use std::fs;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::AsyncReadExt;

use super::authority::Decision;
use super::client_identity::{ClientIdentity, FsIdentityGuard};
use super::desktop::{configured_user_command, DesktopEnvironment};
use super::filesystem::Target;
use super::wire::requests::ScreenshotRequest;
use crate::caps::{Cap, Scope, Verb};

mod portal;
pub use portal::helper;

const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn capture(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    let request: ScreenshotRequest = serde_json::from_value(params["request"].clone())
        .map_err(|error| format!("invalid screenshot request: {error}"))?;
    let directory = resolve_directory(request.directory.as_str(), authority.owner_uid())?;
    let caps = requested_caps(&directory);
    let _authorized = authority.require_all(&caps)?;
    if client.require_uid()? != authority.owner_uid() {
        return Err("screenshot owner mismatch".into());
    }
    let environment = DesktopEnvironment::for_user(
        authority.owner_uid(),
        client.gid.ok_or("screenshot owner gid is unavailable")?,
        client.require_home_dir()?,
        client.pid.ok_or("screenshot peer pid is unavailable")?,
    )?;
    validate_session_bus(authority.owner_uid())?;
    let (mut gate, helper_gate) = UnixStream::pair().map_err(|error| error.to_string())?;
    let fd = helper_gate.as_raw_fd();
    let mut command = configured_user_command(
        Path::new("/proc/self/exe"),
        &helper_args(request.modal, fd),
        &environment,
    );
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    for key in ["WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY"] {
        command.env_remove(key);
    }
    gate.write_all(&[1]).map_err(|error| error.to_string())?;
    let result = capture_to(&directory, authority, command, CAPTURE_TIMEOUT).await;
    drop(helper_gate);
    result
}

fn helper_args(modal: bool, fd: i32) -> Vec<String> {
    vec!["--capture-helper".into(), modal.to_string(), fd.to_string()]
}

fn requested_caps(directory: &Path) -> Vec<Cap> {
    vec![
        Cap::new(Verb::DESKTOP_CAPTURE, Scope::name("screen")),
        Cap::new(Verb::FS_WRITE, Scope::path(directory.to_string_lossy())),
    ]
}

fn resolve_directory(raw: &str, owner: u32) -> Result<PathBuf, String> {
    if raw.is_empty() || raw.contains('\0') || !Path::new(raw).is_absolute() {
        return Err("capture directory must be a nonempty absolute path without NUL".into());
    }
    let _identity = FsIdentityGuard::enter(owner)?;
    let directory = fs::canonicalize(raw).map_err(|error| format!("capture directory: {error}"))?;
    if directory.parent().is_none() || !directory.is_dir() {
        return Err("capture destination must be an existing directory".into());
    }
    crate::worker::derive::reject_forbidden(&directory)?;
    Ok(directory)
}

fn validate_session_bus(uid: u32) -> Result<(), String> {
    let metadata = fs::symlink_metadata(format!("/run/user/{uid}/bus"))
        .map_err(|error| format!("screenshot session bus is unavailable: {error}"))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != uid
    {
        return Err("screenshot session bus is not an owner-bound socket".into());
    }
    Ok(())
}

async fn capture_to(
    directory: &Path,
    authority: &Decision,
    command: std::process::Command,
    timeout: Duration,
) -> Result<Value, String> {
    let caps = requested_caps(directory);
    let _authorized = authority.require_all(&caps)?;
    let path = directory.join(
        chrono::Local::now()
            .format("Screenshot_%Y-%m-%d_%H-%M-%S.png")
            .to_string(),
    );
    let target = Target::open(&path, authority.owner_uid())?;
    let png = run_capture(command, timeout).await?;
    if png.is_empty() {
        return Ok(json!({"cancelled": true, "path": null}));
    }
    let _authorized = authority.require_all(&caps)?;
    target.write_new(&png, authority)?;
    Ok(json!({"cancelled": false, "path": path}))
}

struct ProcessGroup(i32);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        // One freshly-created group, never a process-name or global signal.
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

async fn run_capture(
    mut command: std::process::Command,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("start native Capture: {error}"))?;
    let group = ProcessGroup(child.id().ok_or("capture process has no pid")? as i32);
    let stdout = child.stdout.take().ok_or("capture stdout is unavailable")?;
    let stderr = child.stderr.take().ok_or("capture stderr is unavailable")?;
    let result = tokio::time::timeout(timeout, async {
        let read = async {
            let mut png = Vec::new();
            stdout
                .take((MAX_PNG_BYTES + 1) as u64)
                .read_to_end(&mut png)
                .await
                .map_err(|error| format!("read capture image: {error}"))?;
            if png.len() > MAX_PNG_BYTES {
                return Err("capture image exceeds 64 MiB".to_string());
            }
            Ok(png)
        };
        let errors = async {
            let mut bytes = Vec::new();
            stderr
                .take(8193)
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| format!("read capture diagnostics: {error}"))?;
            if bytes.len() > 8192 {
                return Err("capture diagnostics exceed the limit".to_string());
            }
            Ok(bytes)
        };
        let wait = async {
            child
                .wait()
                .await
                .map_err(|error| format!("wait for capture: {error}"))
        };
        tokio::try_join!(read, errors, wait)
    })
    .await;
    drop(group);
    let result = match result {
        Ok(result) => result,
        Err(_) => Err("native Capture timed out".into()),
    };
    if result.is_err() {
        let _ = child.wait().await;
    }
    let (png, errors, status) = result?;
    if !status.success() {
        return Err(format!(
            "native Capture failed ({status}): {}",
            String::from_utf8_lossy(&errors).trim()
        ));
    }
    if !png.is_empty() && !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("native Capture returned invalid PNG data".into());
    }
    Ok(png)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/capture.rs"
    ));
}
