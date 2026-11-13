use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use futures_util::StreamExt;
use tokio::io::AsyncReadExt;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

use super::{CAPTURE_TIMEOUT, MAX_PNG_BYTES};

pub fn helper(args: &[String]) -> Result<(), String> {
    if args.len() != 2 {
        return Err("Capture adapter requires modal state and a broker fd".into());
    }
    let modal = args[0]
        .parse::<bool>()
        .map_err(|_| "invalid capture modal state")?;
    let fd = args[1]
        .parse::<i32>()
        .map_err(|_| "invalid capture broker fd")?;
    if fd < 3 || unsafe { libc::getuid() } != unsafe { libc::geteuid() } {
        return Err("Capture adapter requires the authenticated owner identity".into());
    }
    let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // This private pre-fork channel is bound to the Root parent, not an App sender.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(peer).cast(),
            &mut length,
        )
    } != 0
        || length as usize != std::mem::size_of::<libc::ucred>()
        || peer.uid != 0
        || peer.pid != unsafe { libc::getppid() }
    {
        return Err("Capture adapter was not started by root clawd".into());
    }
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) } != 0 {
        return Err(format!(
            "harden capture adapter: {}",
            std::io::Error::last_os_error()
        ));
    }
    let gate = unsafe { UnixStream::from_raw_fd(fd) };
    gate.set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let png = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?
        .block_on(async {
            tokio::time::timeout(CAPTURE_TIMEOUT, async {
                let mut gate =
                    tokio::net::UnixStream::from_std(gate).map_err(|error| error.to_string())?;
                if gate.read_u8().await.map_err(|error| error.to_string())? != 1 {
                    return Err("Capture dispatch was not authorized by root clawd".into());
                }
                let connection = Connection::session()
                    .await
                    .map_err(|error| error.to_string())?;
                match portal_path(&connection, modal).await? {
                    Some(path) => consume_png(&path),
                    None => Ok(Vec::new()),
                }
            })
            .await
            .map_err(|_| "Capture adapter timed out".to_string())?
        })?;
    std::io::stdout()
        .lock()
        .write_all(&png)
        .map_err(|error| error.to_string())
}

async fn portal_path(connection: &Connection, modal: bool) -> Result<Option<PathBuf>, String> {
    const DESTINATION: &str = "org.freedesktop.portal.Desktop";
    let portal = Proxy::new(
        connection,
        DESTINATION,
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Screenshot",
    )
    .await
    .map_err(|error| error.to_string())?;
    portal
        .get_property::<u32>("version")
        .await
        .map_err(|error| error.to_string())?;
    let sender = connection
        .unique_name()
        .ok_or("Capture portal connection has no unique name")?
        .as_str()
        .trim_start_matches(':')
        .replace('.', "_");
    let token = format!("claw_capture_{}", uuid::Uuid::new_v4().simple());
    let expected = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = Proxy::new(
        connection,
        DESTINATION,
        expected.as_str(),
        "org.freedesktop.portal.Request",
    )
    .await
    .map_err(|error| error.to_string())?;
    let mut responses = request
        .receive_signal("Response")
        .await
        .map_err(|error| error.to_string())?;
    let options = HashMap::from([
        ("handle_token", Value::from(token.as_str())),
        ("interactive", Value::Bool(false)),
        ("modal", Value::Bool(modal)),
    ]);
    let actual: OwnedObjectPath = portal
        .call("Screenshot", &("", options))
        .await
        .map_err(|error| error.to_string())?;
    if actual.as_str() != expected {
        return Err("Capture portal returned an unrelated request handle".into());
    }
    let response = responses
        .next()
        .await
        .ok_or("Capture portal closed without a response")?;
    let (status, mut values): (u32, HashMap<String, OwnedValue>) = response
        .body()
        .deserialize()
        .map_err(|error| error.to_string())?;
    match status {
        1 => Ok(None),
        0 => {
            let uri = String::try_from(
                values
                    .remove("uri")
                    .ok_or("Capture portal omitted its image")?,
            )
            .map_err(|error| error.to_string())?;
            let path = url::Url::parse(&uri)
                .map_err(|error| error.to_string())?
                .to_file_path()
                .map_err(|_| "Capture portal did not return a local image")?;
            Ok(Some(path))
        }
        _ => Err(format!("Capture portal refused the request ({status})")),
    }
}

fn consume_png(path: &Path) -> Result<Vec<u8>, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())?;
    let pinned = file.metadata().map_err(|error| error.to_string())?;
    if !pinned.is_file()
        || pinned.uid() != unsafe { libc::geteuid() }
        || pinned.nlink() != 1
        || pinned.len() > MAX_PNG_BYTES as u64
    {
        return Err("Capture portal image is not a bounded, single-link owner file".into());
    }
    let mut png = Vec::new();
    (&file)
        .take((MAX_PNG_BYTES + 1) as u64)
        .read_to_end(&mut png)
        .map_err(|error| error.to_string())?;
    if png.len() > MAX_PNG_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("Capture portal image is not a bounded PNG".into());
    }
    let current = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if (pinned.dev(), pinned.ino()) != (current.dev(), current.ino()) {
        return Err("Capture portal image was replaced".into());
    }
    fs::remove_file(path).map_err(|error| error.to_string())?;
    Ok(png)
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/capture/portal.rs"
    ));
}
