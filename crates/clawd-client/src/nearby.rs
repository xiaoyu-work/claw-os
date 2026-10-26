//! Renderer-independent nearby file sharing. Identifiers are not permissions.

use serde::{Deserialize, Serialize};

pub const SCHEMA: u32 = 1;
pub const MAX_FILES: usize = 16;
pub const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_WIRE_BYTES: u64 = MAX_TOTAL_BYTES + 16 * 1024 * 1024;
pub const UPLOAD_CHUNK_BYTES: usize = 192 * 1024;
pub const MAX_PEERS: usize = 32;
pub const MAX_OFFERS: usize = 16;
pub const MAX_TRANSFERS: usize = 32;
pub const MAX_FRAME_BYTES: usize = 512 * 1024;
pub const WORKER_PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioMode {
    Shared,
    Exclusive,
    ActiveMonitor,
}

impl RadioMode {
    pub fn disrupts_wifi(self) -> bool {
        self != Self::Shared
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioProbe {
    pub available: bool,
    pub mode: Option<RadioMode>,
    pub driver: Option<String>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Starting,
    Ready,
    Stopping,
    Ended,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferPhase {
    Offered,
    Accepted,
    Sending,
    Received,
    Delivered,
    Declined,
    Failed,
    Expired,
    Interrupted,
}

impl TransferPhase {
    pub fn terminal(self) -> bool {
        !matches!(self, Self::Offered | Self::Accepted | Self::Sending)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SharedFile {
    pub name: String,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyPeer {
    pub id: String,
    pub name: String,
    pub discovered_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyOffer {
    pub id: String,
    pub sender: String,
    pub files: Vec<SharedFile>,
    pub expires_at: String,
    pub phase: TransferPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyTransfer {
    pub id: String,
    pub incoming: bool,
    pub peer: String,
    pub files: Vec<SharedFile>,
    pub phase: TransferPhase,
    /// Outgoing wire bytes or incoming persisted file bytes; no inferred live receive progress.
    pub bytes: u64,
    pub total_bytes: Option<u64>,
    pub received_paths: Vec<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbySession {
    pub id: String,
    pub phase: SessionPhase,
    pub mode: RadioMode,
    pub receive: bool,
    pub expires_at: String,
    pub peers: Vec<NearbyPeer>,
    pub offers: Vec<NearbyOffer>,
    pub discovering: bool,
    pub transfers: Vec<NearbyTransfer>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyView {
    pub schema: u32,
    pub owner_uid: u32,
    pub probe: RadioProbe,
    pub session: Option<NearbySession>,
    pub inbox: String,
    pub uploads: Vec<UploadView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadView {
    pub id: String,
    pub name: String,
    pub bytes: u64,
    pub written: u64,
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyResponse {
    pub view: NearbyView,
    pub upload: Option<UploadView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum NearbyAction {
    Open {
        duration_secs: u32,
        receive: bool,
        expected_mode: RadioMode,
        allow_disruption: bool,
    },
    Discover {
        session_id: String,
    },
    Send {
        session_id: String,
        peer_id: String,
        paths: Vec<String>,
        uploads: Vec<String>,
    },
    Decide {
        session_id: String,
        offer_id: String,
        accept: bool,
    },
    Stop {
        session_id: String,
    },
    Cancel {
        session_id: String,
        transfer_id: String,
    },
    UploadBegin {
        name: String,
        bytes: u64,
    },
    UploadChunk {
        upload_id: String,
        offset: u64,
        data: String,
    },
    UploadFinish {
        upload_id: String,
    },
    UploadDelete {
        upload_id: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NearbyStatusRequest {}

#[derive(Debug, Clone, Serialize)]
pub struct NearbyRequest {
    pub request: NearbyAction,
}
impl<'de> Deserialize<'de> for NearbyRequest {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Body {
            request: NearbyAction,
        }
        let body = Body::deserialize(deserializer)?;
        body.request.validate().map_err(serde::de::Error::custom)?;
        Ok(Self {
            request: body.request,
        })
    }
}

pub fn valid_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

pub fn text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty()
        && value.len() <= limit
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c,'\u{202a}'..='\u{202e}'|'\u{2066}'..='\u{2069}'))
}

pub fn file_name(value: &str) -> bool {
    text(value, 240) && value != "." && value != ".." && !value.contains(['/', '\\', ':'])
}

impl NearbyAction {
    pub fn validate(&self) -> Result<(), String> {
        let id = |value: &str| {
            if valid_id(value) {
                Ok(())
            } else {
                Err("invalid nearby identifier".to_string())
            }
        };
        match self {
            Self::Open { duration_secs, .. } if !(30..=600).contains(duration_secs) => {
                return Err("receive/discovery window must be 30-600 seconds".into())
            }
            Self::Discover { session_id } | Self::Stop { session_id } => id(session_id)?,
            Self::Decide {
                session_id,
                offer_id,
                ..
            } => {
                id(session_id)?;
                id(offer_id)?;
            }
            Self::Cancel {
                session_id,
                transfer_id,
            } => {
                id(session_id)?;
                id(transfer_id)?;
            }
            Self::Send {
                session_id,
                peer_id,
                paths,
                uploads,
            } => {
                id(session_id)?;
                id(peer_id)?;
                if paths.is_empty() == uploads.is_empty() || paths.len() + uploads.len() > MAX_FILES
                {
                    return Err("select 1-16 files, using paths or uploaded files, not both".into());
                }
                let mut distinct = std::collections::HashSet::new();
                for path in paths {
                    if !text(path, 4096) || !std::path::Path::new(path).is_absolute() {
                        return Err("file paths must be bounded absolute paths".into());
                    }
                    if !distinct.insert(path) {
                        return Err("selected file paths must be distinct".into());
                    }
                }
                distinct.clear();
                for upload in uploads {
                    id(upload)?;
                    if !distinct.insert(upload) {
                        return Err("selected uploads must be distinct".into());
                    }
                }
            }
            Self::UploadBegin { name, bytes } => {
                if !file_name(name) || *bytes > MAX_FILE_BYTES {
                    return Err("invalid name or file exceeds 128 MiB".into());
                }
            }
            Self::UploadChunk {
                upload_id,
                data,
                offset,
            } => {
                id(upload_id)?;
                if data.len() > UPLOAD_CHUNK_BYTES.div_ceil(3) * 4 || *offset > MAX_FILE_BYTES {
                    return Err("upload chunk exceeds its bound".into());
                }
            }
            Self::UploadFinish { upload_id } | Self::UploadDelete { upload_id } => id(upload_id)?,
            _ => {}
        }
        Ok(())
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Discover { .. } => "discover",
            Self::Send { .. } => "send",
            Self::Decide { .. } => "decide",
            Self::Stop { .. } => "stop",
            Self::Cancel { .. } => "cancel",
            Self::UploadBegin { .. } => "upload_begin",
            Self::UploadChunk { .. } => "upload_chunk",
            Self::UploadFinish { .. } => "upload_finish",
            Self::UploadDelete { .. } => "upload_delete",
        }
    }
}

impl NearbyView {
    pub fn validate(&self, owner: u32) -> Result<(), String> {
        if owner == 0
            || self.schema != SCHEMA
            || self.owner_uid != owner
            || !text(&self.inbox, 4096)
            || self.probe.diagnostics.len() > 16
            || self.probe.diagnostics.iter().any(|s| !text(s, 1024))
            || self.probe.driver.as_deref().is_some_and(|s| !text(s, 128))
            || self.probe.available && self.probe.mode.is_none()
        {
            return Err("invalid nearby owner, schema or probe".into());
        }
        let mut upload_ids = std::collections::HashSet::new();
        if self.uploads.len() > MAX_FILES
            || self.uploads.iter().any(|upload| {
                !valid_id(&upload.id)
                    || !file_name(&upload.name)
                    || upload.bytes > MAX_FILE_BYTES
                    || !upload_ids.insert(&upload.id)
                    || upload.written > upload.bytes
                    || upload.ready && upload.written != upload.bytes
            })
            || self.uploads.iter().map(|upload| upload.bytes).sum::<u64>() > MAX_TOTAL_BYTES
        {
            return Err("invalid nearby uploads".into());
        }
        if let Some(session) = &self.session {
            if !valid_id(&session.id)
                || !timestamp(&session.expires_at)
                || session.peers.len() > MAX_PEERS
                || session.offers.len() > MAX_OFFERS
                || session.transfers.len() > MAX_TRANSFERS
                || session.message.as_deref().is_some_and(|s| !text(s, 2048))
            {
                return Err("invalid nearby session".into());
            }
            let mut ids = std::collections::HashSet::new();
            for peer in &session.peers {
                if !valid_id(&peer.id)
                    || !ids.insert(&peer.id)
                    || !text(&peer.name, 240)
                    || !timestamp(&peer.discovered_at)
                {
                    return Err("invalid nearby peer".into());
                }
            }
            ids.clear();
            for offer in &session.offers {
                if !valid_id(&offer.id)
                    || !ids.insert(&offer.id)
                    || !text(&offer.sender, 240)
                    || !timestamp(&offer.expires_at)
                {
                    return Err("invalid nearby offer".into());
                }
                validate_files(&offer.files)?;
            }
            ids.clear();
            for transfer in &session.transfers {
                if !valid_id(&transfer.id)
                    || !ids.insert(&transfer.id)
                    || !text(&transfer.peer, 240)
                    || transfer.bytes > MAX_WIRE_BYTES
                    || transfer
                        .total_bytes
                        .is_some_and(|n| n > MAX_WIRE_BYTES || transfer.bytes > n)
                    || transfer.received_paths.len() > MAX_FILES
                    || transfer.received_paths.iter().any(|path| !text(path, 4096))
                    || transfer.message.as_deref().is_some_and(|s| !text(s, 2048))
                {
                    return Err("invalid nearby transfer".into());
                }
                validate_files(&transfer.files)?;
            }
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_FRAME_BYTES {
            return Err("nearby view exceeds its serialized bound".into());
        }
        Ok(())
    }
}

pub fn validate_files(files: &[SharedFile]) -> Result<(), String> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err("file count exceeds 1-16".into());
    }
    let mut total = 0u64;
    let mut names = std::collections::HashSet::new();
    for file in files {
        if !file_name(&file.name)
            || !names.insert(&file.name)
            || file.bytes.is_some_and(|bytes| bytes > MAX_FILE_BYTES)
        {
            return Err("invalid file metadata".into());
        }
        total = total
            .checked_add(file.bytes.unwrap_or(0))
            .ok_or("file size overflow")?;
    }
    if total > MAX_TOTAL_BYTES {
        return Err("files exceed 512 MiB".into());
    }
    Ok(())
}

fn timestamp(value: &str) -> bool {
    text(value, 64) && chrono::DateTime::parse_from_rfc3339(value).is_ok()
}

impl NearbyResponse {
    pub fn validate(&self, owner: u32, action: &NearbyAction) -> Result<(), String> {
        self.view.validate(owner)?;
        let fail = || "nearby acknowledgement differs from the requested action".to_string();
        match action {
            NearbyAction::Open {
                expected_mode,
                receive,
                ..
            } => {
                if self.view.session.as_ref().is_none_or(|session| {
                    session.mode != *expected_mode || session.receive != *receive
                }) {
                    return Err(fail());
                }
            }
            NearbyAction::Discover { session_id }
            | NearbyAction::Send { session_id, .. }
            | NearbyAction::Decide { session_id, .. }
            | NearbyAction::Stop { session_id }
            | NearbyAction::Cancel { session_id, .. }
                if self
                    .view
                    .session
                    .as_ref()
                    .is_none_or(|session| &session.id != session_id) =>
            {
                return Err(fail())
            }
            _ => {}
        }
        let session = self.view.session.as_ref();
        match action {
            NearbyAction::Decide {
                offer_id, accept, ..
            } => {
                let expected = if *accept {
                    TransferPhase::Accepted
                } else {
                    TransferPhase::Declined
                };
                if session.is_none_or(|session| {
                    !session
                        .offers
                        .iter()
                        .any(|offer| &offer.id == offer_id && offer.phase == expected)
                }) {
                    return Err(fail());
                }
            }
            NearbyAction::Cancel { transfer_id, .. }
                if session.is_none_or(|session| {
                    !session
                        .transfers
                        .iter()
                        .any(|transfer| &transfer.id == transfer_id && !transfer.incoming)
                }) =>
            {
                return Err(fail())
            }
            NearbyAction::Send { .. }
                if session.is_none_or(|session| {
                    session.transfers.last().is_none_or(|transfer| {
                        transfer.incoming || transfer.phase != TransferPhase::Sending
                    })
                }) =>
            {
                return Err(fail())
            }
            _ => {}
        }
        match (action, &self.upload) {
            (NearbyAction::UploadBegin { name, bytes }, Some(upload))
                if &upload.name == name
                    && upload.bytes == *bytes
                    && upload.written == 0
                    && !upload.ready => {}
            (
                NearbyAction::UploadChunk {
                    upload_id,
                    offset,
                    data,
                },
                Some(upload),
            ) => {
                let padding = if data.ends_with("==") {
                    2
                } else if data.ends_with('=') {
                    1
                } else {
                    0
                };
                let decoded = data.len() / 4 * 3;
                if &upload.id != upload_id
                    || decoded < padding
                    || upload.written != offset + (decoded - padding) as u64
                {
                    return Err(fail());
                }
            }
            (NearbyAction::UploadFinish { upload_id }, Some(upload))
                if &upload.id == upload_id && upload.ready => {}
            (NearbyAction::UploadDelete { upload_id }, None)
                if !self
                    .view
                    .uploads
                    .iter()
                    .any(|upload| &upload.id == upload_id) => {}
            (
                NearbyAction::UploadBegin { .. }
                | NearbyAction::UploadChunk { .. }
                | NearbyAction::UploadFinish { .. }
                | NearbyAction::UploadDelete { .. },
                _,
            ) => return Err(fail()),
            (_, None) => {}
            _ => return Err(fail()),
        }
        if self
            .upload
            .as_ref()
            .is_some_and(|upload| !self.view.uploads.contains(upload))
        {
            return Err(fail());
        }
        Ok(())
    }
}

// Private helper IPC: no capabilities, owner selectors or arbitrary executables.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkConfig {
    pub protocol: u32,
    pub interface: String,
    pub index: u32,
    pub address: String,
    pub name: String,
    pub receive: bool,
    pub duration_secs: u32,
    pub announce: Option<(u32, u32, u32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerCommand {
    Start(LinkConfig),
    Discover {
        peers: Vec<String>,
    },
    Send {
        id: String,
        address: String,
        flags: u32,
        files: Vec<String>,
    },
    Decide {
        id: u64,
        accept: bool,
    },
    Cancel {
        id: String,
    },
    Stop {},
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerEvent {
    Ready {},
    Peer {
        address: String,
        name: String,
        flags: u32,
    },
    DiscoveryFinished {},
    Offer {
        id: u64,
        sender: String,
        files: Vec<SharedFile>,
    },
    Withdrawn {
        id: u64,
    },
    Received {
        id: u64,
        paths: Vec<String>,
    },
    ReceiveFailed {
        id: u64,
        message: String,
    },
    Progress {
        id: String,
        bytes: u64,
        total: u64,
    },
    Sent {
        id: String,
        outcome: String,
    },
    Error {
        message: String,
    },
    Stopped {},
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/nearby.rs"));
}
