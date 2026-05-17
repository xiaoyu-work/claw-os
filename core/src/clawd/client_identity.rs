use serde::Serialize;
use tokio::net::UnixStream;

#[derive(Debug, Clone, Serialize)]
pub struct ClientIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gid: Option<u32>,
}

impl ClientIdentity {
    pub fn from_stream(stream: &UnixStream) -> Self {
        peer_identity(stream).unwrap_or_else(Self::unknown)
    }

    pub fn unknown() -> Self {
        Self {
            pid: None,
            uid: None,
            gid: None,
        }
    }
}

#[cfg(target_os = "linux")]
fn peer_identity(stream: &UnixStream) -> Option<ClientIdentity> {
    let cred = stream.peer_cred().ok()?;
    Some(ClientIdentity {
        pid: cred.pid().and_then(|pid| u32::try_from(pid).ok()),
        uid: Some(cred.uid()),
        gid: Some(cred.gid()),
    })
}

#[cfg(not(target_os = "linux"))]
fn peer_identity(_stream: &UnixStream) -> Option<ClientIdentity> {
    None
}
