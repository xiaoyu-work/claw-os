//! Owner-bound, capability-gated adapter to the installed Media Player, not a
//! general MPRIS controller. Only this OS helper enters the owner's session bus.

use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::authority::{Authorized, Decision};
use super::client_identity::ClientIdentity;
use super::wire::requests::{MediaPlayerAction, MediaPlayerControl};
use crate::caps::{Cap, Scope, Verb};

mod mpris;

const MAX_CALL: Duration = Duration::from_secs(5);
const MAX_OUTPUT: usize = 64 * 1024;

fn required_cap(action: MediaPlayerAction) -> Cap {
    Cap::new(
        if action == MediaPlayerAction::Status {
            Verb::DESKTOP_MEDIA_OBSERVE
        } else {
            Verb::DESKTOP_MEDIA_CONTROL
        },
        Scope::name("cosmic-player"),
    )
}

fn authorize(
    authority: &Decision,
    action: MediaPlayerAction,
    owner: u32,
) -> Result<Authorized, String> {
    if authority.owner_uid() != owner {
        return Err("Media Player owner mismatch".into());
    }
    authority.require(required_cap(action))
}

fn remaining(deadline: u64) -> Result<Duration, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock precedes the Unix epoch")?
        .as_millis();
    let left = u128::from(deadline)
        .checked_sub(now)
        .filter(|left| *left > 0)
        .ok_or("Media Player deadline expired")?;
    Ok(Duration::from_millis(left.min(MAX_CALL.as_millis()) as u64))
}

pub async fn control(
    params: Value,
    client: &ClientIdentity,
    authority: &Decision,
) -> Result<Value, String> {
    let request: MediaPlayerControl = serde_json::from_value(params)
        .map_err(|error| format!("invalid Media Player request: {error}"))?;
    let uid = client.require_uid()?;
    let _authorized = authorize(authority, request.action, uid)?;
    let timeout = remaining(request.deadline_unix_ms)?;
    if unsafe { libc::geteuid() } != 0 || uid == 0 {
        return Err("Media Player requires root clawd and a non-root owner session".into());
    }
    let gid = super::client_identity::owner_groups(uid)?.0;
    let deadline = request.deadline_unix_ms.min(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid system clock")?
            .as_millis() as u64
            + MAX_CALL.as_millis() as u64,
    );
    let (parent, child_socket) = UnixStream::pair().map_err(|error| error.to_string())?;
    let child_fd = child_socket.as_raw_fd();
    let parent_pid = unsafe { libc::getpid() };
    let mut command = std::process::Command::new("/proc/self/exe");
    command
        .args([
            "--media-player-helper",
            request.action.as_str(),
            &deadline.to_string(),
            &child_fd.to_string(),
        ])
        .env_clear()
        .env("LC_ALL", "C.UTF-8")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(child_fd, libc::F_SETFD, 0) != 0
                || libc::setgroups(0, std::ptr::null()) != 0
                || libc::setgid(gid) != 0
                || libc::setuid(uid) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent_pid {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "Media Player broker exited",
                ));
            }
            Ok(())
        });
    }
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|error| format!("start Media Player adapter: {error}"))?;
    drop(child_socket);
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or("Media Player stdout unavailable")?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or("Media Player stderr unavailable")?;
    parent
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let mut gate = tokio::net::UnixStream::from_std(parent).map_err(|error| error.to_string())?;
    let result = tokio::time::timeout(timeout, async {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let read = async {
            stdout_pipe
                .take((MAX_OUTPUT + 1) as u64)
                .read_to_end(&mut stdout)
                .await
                .map_err(|error| error.to_string())?;
            if stdout.len() > MAX_OUTPUT {
                return Err("Media Player response exceeds the limit".to_string());
            }
            Ok(())
        };
        let errors = async {
            stderr_pipe
                .take(8193)
                .read_to_end(&mut stderr)
                .await
                .map_err(|error| error.to_string())?;
            if stderr.len() > 8192 {
                return Err("Media Player diagnostics exceed the limit".to_string());
            }
            Ok(())
        };
        let permission = async {
            match gate.read_u8().await {
                Ok(1) => {
                    remaining(deadline)?;
                    let _authorized = authorize(authority, request.action, uid)?;
                    gate.write_u8(1).await.map_err(|error| error.to_string())?;
                    Ok(())
                }
                // An early discovery failure reports its own diagnostics.
                Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
                _ => Err("invalid Media Player dispatch gate".to_string()),
            }
        };
        tokio::try_join!(read, errors, permission)?;
        let status = child.wait().await.map_err(|error| error.to_string())?;
        if !status.success() {
            return Err(format!(
                "Media Player adapter failed: {}",
                String::from_utf8_lossy(&stderr).trim()
            ));
        }
        serde_json::from_slice(&stdout)
            .map_err(|error| format!("invalid Media Player response: {error}"))
    })
    .await
    .map_err(|_| "Media Player adapter timed out".to_string())
    .and_then(|value| value);
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    // Never disclose metadata under authority revoked while the helper ran.
    let _authorized = authorize(authority, request.action, uid)?;
    result
}

/// The daemon's fixed subprocess entry. No caller-selected bus, executable,
/// media URI or MPRIS destination is accepted.
pub fn helper(args: &[String]) -> Result<Value, String> {
    if args.len() != 3 {
        return Err("Media Player adapter requires action, deadline and broker fd".into());
    }
    let action: MediaPlayerAction =
        serde_json::from_value(json!(args[0])).map_err(|_| "invalid Media Player action")?;
    let deadline = args[1]
        .parse::<u64>()
        .map_err(|_| "invalid Media Player deadline")?;
    let fd = args[2]
        .parse::<i32>()
        .map_err(|_| "invalid Media Player broker fd")?;
    let uid = unsafe { libc::geteuid() };
    if uid == 0 || uid != unsafe { libc::getuid() } || fd < 3 {
        return Err("Media Player adapter requires the owner's unprivileged identity".into());
    }
    let peer = mpris::socket_peer(fd)?;
    if peer.uid != 0 || peer.pid != u32::try_from(unsafe { libc::getppid() }).unwrap_or(0) {
        return Err("Media Player adapter was not started by root clawd".into());
    }
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(format!(
            "harden Media Player adapter: {}",
            std::io::Error::last_os_error()
        ));
    }
    // The inherited descriptor was authenticated above and is now owned here.
    let gate = unsafe { UnixStream::from_raw_fd(fd) };
    gate.set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let timeout = remaining(deadline)?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            tokio::time::timeout(timeout, async {
                let connection = mpris::owner_bus(uid).await?;
                let executable = mpris::Executable::installed("/usr/bin/cosmic-player")?;
                let mut gate =
                    tokio::net::UnixStream::from_std(gate).map_err(|error| error.to_string())?;
                let permission = async {
                    gate.write_u8(1).await.map_err(|error| error.to_string())?;
                    match gate.read_u8().await {
                        Ok(1) => Ok(()),
                        _ => Err("Media Player dispatch authorization was withdrawn".into()),
                    }
                };
                mpris::execute(&connection, uid, &executable, action, deadline, permission).await
            })
            .await
            .map_err(|_| "Media Player adapter timed out".to_string())?
        })
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/media_player.rs"
    ));
}
