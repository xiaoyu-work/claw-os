//! The fixed transport helper has an empty home and no D-Bus socket.

use clawd_client::nearby::*;
use std::{os::unix::process::CommandExt, path::Path, process::Stdio};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt},
    process::{Child, ChildStdin},
};

pub(super) fn spawn(root: &Path, owner: u32) -> Result<Child, String> {
    super::radio::installed()?;
    crate::agentd::spawn::validate_root_owned_executable(Path::new("/usr/bin/bwrap"))?;
    let command = sandbox_command(root, owner, super::radio::WORKER)?;
    tokio::process::Command::from(command)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("start isolated nearby transport: {e}"))
}

fn sandbox_command(
    root: &Path,
    owner: u32,
    executable: &str,
) -> Result<std::process::Command, String> {
    let gid = super::super::client_identity::owner_groups(owner)?.0;
    let mut command = std::process::Command::new("/usr/bin/bwrap");
    command.args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--disable-userns",
        "--assert-userns-disabled",
        "--cap-drop",
        "ALL",
        "--new-session",
        "--die-with-parent",
        "--uid",
        &owner.to_string(),
        "--gid",
        &gid.to_string(),
        "--ro-bind",
        "/usr",
        "/usr",
        "--ro-bind",
        "/lib",
        "/lib",
        "--ro-bind-try",
        "/lib64",
        "/lib64",
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/data",
        "--bind",
        root.join("incoming")
            .to_str()
            .ok_or("runtime path must be UTF-8")?,
        "/data/incoming",
        "--ro-bind",
        root.join("outgoing")
            .to_str()
            .ok_or("runtime path must be UTF-8")?,
        "/data/outgoing",
        "--chdir",
        "/data",
        "--clearenv",
        "--setenv",
        "HOME",
        "/tmp",
        "--setenv",
        "LC_ALL",
        "C.UTF-8",
        "--",
        executable,
    ]);
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let parent = unsafe { libc::getpid() };
    unsafe {
        command.pre_exec(move || {
            crate::agentd::spawn::mark_inherited_descriptors_cloexec_except(3, -1);
            if libc::setgroups(0, std::ptr::null()) != 0
                || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                return Err(std::io::Error::from_raw_os_error(libc::EPIPE));
            }
            for (resource, value) in [
                (libc::RLIMIT_CORE, 0),
                (libc::RLIMIT_NOFILE, 128),
                (libc::RLIMIT_AS, 768 * 1024 * 1024),
                (libc::RLIMIT_FSIZE, MAX_TOTAL_BYTES + 32 * 1024 * 1024),
            ] {
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(command)
}

pub(super) async fn send(input: &mut ChildStdin, command: &WorkerCommand) -> Result<(), String> {
    let mut bytes = serde_json::to_vec(command).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("nearby helper assignment exceeds bound".into());
    }
    bytes.push(b'\n');
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        input.write_all(&bytes).await?;
        input.flush().await
    })
    .await
    .map_err(|_| "nearby helper write timed out")?
    .map_err(|e| e.to_string())
}

pub(super) async fn event<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    bytes: &mut Vec<u8>,
) -> Result<Option<WorkerEvent>, String> {
    loop {
        let chunk = reader.fill_buf().await.map_err(|e| e.to_string())?;
        if chunk.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err("truncated helper event".into())
            };
        }
        let count = chunk
            .iter()
            .position(|b| *b == b'\n')
            .map_or(chunk.len(), |index| index + 1);
        if bytes.len() + count > MAX_FRAME_BYTES {
            return Err("helper event exceeds bound".into());
        }
        bytes.extend_from_slice(&chunk[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            let event = serde_json::from_slice(bytes)
                .map(Some)
                .map_err(|e| e.to_string());
            bytes.clear();
            return event;
        }
    }
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/nearby/process.rs"
    ));
}
