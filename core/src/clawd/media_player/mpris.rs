use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use zbus::fdo::DBusProxy;
use zbus::names::BusName;
use zbus::zvariant::OwnedValue;
use zbus::{Connection, Proxy};

use super::{remaining, MediaPlayerAction, MAX_OUTPUT};
use crate::clawd::transport::peer::Credentials;

const PREFIX: &str = "org.mpris.MediaPlayer2.com.clawos.Player.";
const OBJECT: &str = "/org/mpris/MediaPlayer2";
const INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

pub(super) fn socket_peer(fd: i32) -> Result<Credentials, String> {
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut credentials as *mut libc::ucred as *mut libc::c_void,
            &mut length,
        )
    } != 0
        || length as usize != std::mem::size_of::<libc::ucred>()
    {
        return Err("Media Player could not authenticate the session socket".into());
    }
    Ok(Credentials {
        uid: credentials.uid,
        gid: credentials.gid,
        pid: u32::try_from(credentials.pid).map_err(|_| "invalid session bus pid")?,
    })
}

pub(super) struct Executable {
    path: PathBuf,
    dev: u64,
    ino: u64,
}

impl Executable {
    pub(super) fn installed(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        crate::provenance::fsec::require_secure_location(path, &[0])
            .map_err(|error| format!("Media Player executable is not trusted: {error}"))?;
        let meta = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !meta.is_file() || meta.mode() & 0o111 == 0 {
            return Err("Media Player executable must be an installed regular executable".into());
        }
        Ok(Self {
            path: path.into(),
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }

    fn process(&self, uid: u32, pid: u32) -> Result<u64, String> {
        let start = crate::proc::read_start_time_ticks_pub(pid)
            .ok_or("Media Player process disappeared")?;
        let status = fs::read_to_string(format!("/proc/{pid}/status"))
            .map_err(|_| "Media Player process identity unavailable")?;
        let ids = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .ok_or("Media Player process has no UID")?
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "invalid Media Player process UID")?;
        if ids.len() != 4 || ids.iter().any(|value| *value != uid) {
            return Err("Media Player process owner mismatch".into());
        }
        let proc_exe = format!("/proc/{pid}/exe");
        let meta =
            fs::metadata(&proc_exe).map_err(|_| "Media Player process executable unavailable")?;
        if (meta.dev(), meta.ino()) != (self.dev, self.ino)
            || fs::read_link(&proc_exe).map_err(|_| "Media Player executable unavailable")?
                != self.path
            || crate::proc::read_start_time_ticks_pub(pid) != Some(start)
        {
            return Err("Media Player endpoint is not the installed native product".into());
        }
        Ok(start)
    }
}

pub(super) async fn owner_bus(uid: u32) -> Result<Connection, String> {
    use std::os::fd::AsRawFd;
    let runtime = PathBuf::from(format!("/run/user/{uid}"));
    let dir =
        fs::symlink_metadata(&runtime).map_err(|_| "Media Player owner session is unavailable")?;
    if !dir.is_dir() || dir.uid() != uid || dir.mode() & 0o077 != 0 {
        return Err("Media Player runtime is not owner-private".into());
    }
    let path = runtime.join("bus");
    let before =
        fs::symlink_metadata(&path).map_err(|_| "Media Player owner session bus is unavailable")?;
    if !before.file_type().is_socket() || before.uid() != uid {
        return Err("Media Player session bus is not an owner-bound socket".into());
    }
    let stream = tokio::net::UnixStream::connect(&path)
        .await
        .map_err(|error| error.to_string())?;
    let peer = socket_peer(stream.as_raw_fd())?;
    if peer.uid != uid || crate::clawd::transport::peer::verify(peer).is_none() {
        return Err("Media Player session bus owner mismatch".into());
    }
    let after = fs::symlink_metadata(&path).map_err(|_| "Media Player session bus disappeared")?;
    if (before.dev(), before.ino()) != (after.dev(), after.ino()) {
        return Err("Media Player session bus was replaced".into());
    }
    // A user-created lookalike bus must not forge GetConnectionCredentials.
    let bus_path = fs::read_link(format!("/proc/{}/exe", peer.pid))
        .map_err(|_| "Media Player session bus executable unavailable")?;
    if !matches!(
        bus_path.to_str(),
        Some("/usr/bin/dbus-daemon" | "/usr/bin/dbus-broker" | "/usr/lib/systemd/systemd")
    ) {
        return Err("Media Player requires the installed user D-Bus service".into());
    }
    Executable::installed(bus_path)?.process(uid, peer.pid)?;
    let connection = zbus::connection::Builder::unix_stream(stream)
        .build()
        .await
        .map_err(|error| error.to_string())?;
    // With socket activation SO_PEERCRED names the user service manager.
    // Recheck the bus implementation as well, before trusting its name table.
    let bus = DBusProxy::new(&connection)
        .await
        .map_err(|error| error.to_string())?;
    let daemon = BusName::try_from("org.freedesktop.DBus").map_err(|error| error.to_string())?;
    let daemon_uid = bus
        .get_connection_unix_user(daemon.clone())
        .await
        .map_err(|error| error.to_string())?;
    let daemon_pid = bus
        .get_connection_unix_process_id(daemon)
        .await
        .map_err(|error| error.to_string())?;
    let daemon_path =
        fs::read_link(format!("/proc/{daemon_pid}/exe")).map_err(|error| error.to_string())?;
    if daemon_uid != uid
        || !matches!(
            daemon_path.to_str(),
            Some("/usr/bin/dbus-daemon" | "/usr/bin/dbus-broker")
        )
    {
        return Err("Media Player user bus is not the installed D-Bus implementation".into());
    }
    Executable::installed(daemon_path)?.process(uid, daemon_pid)?;
    Ok(connection)
}

fn select(names: impl IntoIterator<Item = String>) -> Result<(String, u32), String> {
    let candidates = names
        .into_iter()
        .filter(|name| name.starts_with(PREFIX))
        .collect::<Vec<_>>();
    let name = match candidates.as_slice() {
        [] => return Err("no native Media Player is running for this owner".into()),
        [name] => name,
        _ => {
            return Err(
                "multiple native Media Player instances are running; selection is ambiguous".into(),
            )
        }
    };
    let suffix = name
        .strip_prefix(PREFIX)
        .and_then(|value| value.strip_prefix("pid"))
        .ok_or("invalid native Media Player endpoint name")?;
    let pid = suffix
        .parse::<u32>()
        .map_err(|_| "invalid native Media Player endpoint pid")?;
    if pid == 0 || suffix != pid.to_string() {
        return Err("invalid native Media Player endpoint pid".into());
    }
    Ok((name.clone(), pid))
}

pub(super) async fn execute(
    connection: &Connection,
    uid: u32,
    executable: &Executable,
    action: MediaPlayerAction,
    deadline: u64,
    permission: impl std::future::Future<Output = Result<(), String>>,
) -> Result<Value, String> {
    remaining(deadline)?;
    let dbus = DBusProxy::new(connection)
        .await
        .map_err(|error| error.to_string())?;
    let names = dbus.list_names().await.map_err(|error| error.to_string())?;
    let (name, expected_pid) = select(names.into_iter().map(|value| value.to_string()))?;
    let name = BusName::try_from(name.as_str()).map_err(|error| error.to_string())?;
    let unique = dbus
        .get_name_owner(name.clone())
        .await
        .map_err(|_| "Media Player endpoint disappeared")?;
    let unique_name = BusName::from(unique.clone());
    let owner = dbus
        .get_connection_unix_user(unique_name.clone())
        .await
        .map_err(|error| error.to_string())?;
    let pid = dbus
        .get_connection_unix_process_id(unique_name)
        .await
        .map_err(|error| error.to_string())?;
    if owner != uid || pid != expected_pid {
        return Err("Media Player endpoint owner or pid mismatch".into());
    }
    let start = executable.process(uid, pid)?;
    let root = Proxy::new(
        connection,
        unique.as_str(),
        OBJECT,
        "org.mpris.MediaPlayer2",
    )
    .await
    .map_err(|error| error.to_string())?;
    let desktop: String = root
        .get_property("DesktopEntry")
        .await
        .map_err(|error| error.to_string())?;
    if desktop != "com.clawos.Player" {
        return Err("Media Player endpoint has the wrong desktop identity".into());
    }
    if dbus
        .get_name_owner(name)
        .await
        .map_err(|_| "Media Player endpoint disappeared")?
        != unique
        || executable.process(uid, pid)? != start
    {
        return Err("Media Player endpoint changed before dispatch".into());
    }
    remaining(deadline)?;
    permission.await?;
    remaining(deadline)?;
    // Always address the authenticated unique connection, never the mutable
    // well-known name (and never another player when that connection exits).
    let player = Proxy::new(connection, unique.as_str(), OBJECT, INTERFACE)
        .await
        .map_err(|error| error.to_string())?;
    let value = match action {
        MediaPlayerAction::Status => {
            let status: String = player
                .get_property("PlaybackStatus")
                .await
                .map_err(|error| error.to_string())?;
            let metadata: HashMap<String, OwnedValue> = player
                .get_property("Metadata")
                .await
                .map_err(|error| error.to_string())?;
            status_value(status, metadata)?
        }
        _ => {
            let method = match action {
                MediaPlayerAction::Play => "Play",
                MediaPlayerAction::Pause => "Pause",
                MediaPlayerAction::Stop => "Stop",
                MediaPlayerAction::Next => "Next",
                MediaPlayerAction::Previous => "Previous",
                MediaPlayerAction::Toggle => "PlayPause",
                MediaPlayerAction::Status => unreachable!(),
            };
            player
                .call::<_, _, ()>(method, &())
                .await
                .map_err(|error| format!("Media Player {method}: {error}"))?;
            json!({"ok": true})
        }
    };
    if value.to_string().len() > MAX_OUTPUT {
        return Err("Media Player metadata exceeds the limit".into());
    }
    Ok(value)
}

fn status_value(
    status: String,
    mut metadata: HashMap<String, OwnedValue>,
) -> Result<Value, String> {
    if !matches!(status.as_str(), "Playing" | "Paused" | "Stopped") {
        return Err("invalid native Media Player playback status".into());
    }
    fn text(
        metadata: &mut HashMap<String, OwnedValue>,
        name: &str,
    ) -> Result<Option<String>, String> {
        metadata
            .remove(name)
            .map(String::try_from)
            .transpose()
            .map_err(|_| format!("invalid Media Player {name} metadata"))
    }
    Ok(json!({
        "status": status,
        "title": text(&mut metadata, "xesam:title")?,
        "album": text(&mut metadata, "xesam:album")?,
        "url": text(&mut metadata, "xesam:url")?,
        "artist": metadata.remove("xesam:artist").map(Vec::<String>::try_from).transpose()
            .map_err(|_| "invalid Media Player artist metadata")?.unwrap_or_default(),
        "length_micros": metadata.remove("mpris:length").map(i64::try_from).transpose()
            .map_err(|_| "invalid Media Player length metadata")?,
    }))
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/clawd/media_player/mpris.rs"
    ));
}
