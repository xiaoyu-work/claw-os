//! Versioned framed JSON ABI spoken by an isolated Agent extension child.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::agent_extensions::capability_ref::CapabilityReference;
use crate::agent_extensions::manifest::{
    EventKind, ABI_VERSION, FEATURE_OBSERVATIONAL_EVENTS, FEATURE_PROPOSED_ACTIONS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MonotonicDeadlineNs(pub u64);

impl MonotonicDeadlineNs {
    pub fn after(duration: Duration) -> Result<Self, String> {
        let nanos = u64::try_from(duration.as_nanos())
            .map_err(|_| "extension event deadline is too large".to_string())?;
        Ok(Self(monotonic_now_ns()?.saturating_add(nanos)))
    }

    pub fn remaining(self) -> Result<Duration, String> {
        let now = monotonic_now_ns()?;
        if self.0 <= now {
            return Err("extension event deadline expired".to_string());
        }
        Ok(Duration::from_nanos(self.0 - now))
    }
}

fn monotonic_now_ns() -> Result<u64, String> {
    #[cfg(unix)]
    {
        let mut value = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut value) } != 0 {
            return Err(format!(
                "read monotonic clock: {}",
                std::io::Error::last_os_error()
            ));
        }
        let seconds = u64::try_from(value.tv_sec)
            .map_err(|_| "monotonic clock returned a negative value".to_string())?;
        let nanos = u64::try_from(value.tv_nsec)
            .map_err(|_| "monotonic clock returned invalid nanoseconds".to_string())?;
        Ok(seconds.saturating_mul(1_000_000_000).saturating_add(nanos))
    }
    #[cfg(not(unix))]
    {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        let elapsed = START.get_or_init(std::time::Instant::now).elapsed();
        u64::try_from(elapsed.as_nanos())
            .map_err(|_| "monotonic clock exceeded the supported range".to_string())
    }
}

pub const MAX_ABI_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 16 * 1024;
pub const MAX_ACTION_INPUT_BYTES: usize = 16 * 1024;
pub const MAX_ACTIONS_PER_EVENT: usize = 4;
pub const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAGIC: [u8; 4] = *b"CEX1";
const HEADER_BYTES: usize = 10;
const REQUEST_KIND: u8 = 1;
const RESPONSE_KIND: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbiBinding {
    pub task_id: String,
    pub session_id: String,
    pub owner_uid: u32,
    pub extension_id: String,
    pub extension_version: String,
    pub package_digest: String,
    pub manifest_digest: String,
    pub entry_digest: String,
    pub capability_generation: String,
    pub lease_digest: String,
    pub instance_nonce: String,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

impl AbiBinding {
    pub fn validate(&self) -> Result<(), String> {
        for (value, label) in [
            (&self.task_id, "task id"),
            (&self.session_id, "session id"),
            (&self.extension_id, "extension id"),
        ] {
            if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
                return Err(format!("extension ABI {label} is invalid"));
            }
        }
        if self.owner_uid == 0 {
            return Err("extension ABI owner is invalid".to_string());
        }
        semver::Version::parse(&self.extension_version)
            .map_err(|_| "extension ABI version is invalid".to_string())?;
        if !crate::provenance::envelope::is_sha256_ref(&self.package_digest) {
            return Err("extension ABI package digest is invalid".to_string());
        }
        for (digest, label, len) in [
            (&self.manifest_digest, "manifest digest", 64),
            (&self.entry_digest, "entry digest", 64),
            (&self.capability_generation, "capability generation", 16),
            (&self.lease_digest, "lease digest", 64),
        ] {
            if digest.len() != len
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(format!("extension ABI {label} is invalid"));
            }
        }
        if self.instance_nonce.len() != 64
            || !self
                .instance_nonce
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("extension ABI instance nonce is invalid".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbiRequest {
    pub protocol: u32,
    pub binding: AbiBinding,
    pub sequence: u64,
    pub message: HostMessage,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "lifecycle", rename_all = "kebab-case")]
pub enum HostMessage {
    Initialize {
        min_version: u32,
        max_version: u32,
        required_features: Vec<String>,
        subscriptions: Vec<EventKind>,
        requested_capability_count: usize,
    },
    Event {
        event_id: String,
        deadline_monotonic_ns: MonotonicDeadlineNs,
        payload: EventPayload,
        capability_refs: Vec<CapabilityReference>,
    },
    Shutdown {
        reason: ShutdownReason,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbiResponse {
    pub protocol: u32,
    pub binding: AbiBinding,
    pub sequence: u64,
    pub message: ExtensionMessage,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "lifecycle", rename_all = "kebab-case")]
pub enum ExtensionMessage {
    Ready {
        selected_version: u32,
        accepted_features: Vec<String>,
    },
    Result {
        event_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        #[serde(default)]
        proposed_actions: Vec<ProposedAction>,
    },
    ShutdownAck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum EventPayload {
    SessionStart {
        source: String,
        attended: bool,
        delegated: bool,
    },
    PreModelCall {
        turn_index: u32,
        attempt_id: String,
        provider: String,
        model: String,
    },
    PostModelCall {
        turn_index: u32,
        attempt_id: String,
        provider: String,
        model: String,
        success: bool,
        latency_ms: u64,
        input_tokens: u32,
        output_tokens: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error_class: Option<String>,
    },
    PreTool {
        turn_index: u32,
        tool: String,
        tool_use_id_digest: String,
        input_bytes: usize,
        input_digest: String,
    },
    PostTool {
        turn_index: u32,
        tool: String,
        tool_use_id_digest: String,
        success: bool,
        latency_ms: u64,
        result_bytes: usize,
        result_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<crate::audit_policy::TextDigest>,
    },
    Completion {
        success: bool,
        turns: u32,
        answer_bytes: usize,
        answer_digest: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<crate::audit_policy::TextDigest>,
    },
}

impl EventPayload {
    pub const fn kind(&self) -> EventKind {
        match self {
            Self::SessionStart { .. } => EventKind::SessionStart,
            Self::PreModelCall { .. } => EventKind::PreModelCall,
            Self::PostModelCall { .. } => EventKind::PostModelCall,
            Self::PreTool { .. } => EventKind::PreTool,
            Self::PostTool { .. } => EventKind::PostTool,
            Self::Completion { .. } => EventKind::Completion,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let bytes = serde_json::to_vec(self)
            .map_err(|error| format!("encode extension event payload: {error}"))?
            .len();
        if bytes > MAX_EVENT_PAYLOAD_BYTES {
            return Err("extension event payload exceeds its limit".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposedAction {
    pub action_id: String,
    pub capability_ref: CapabilityReference,
    pub tool: String,
    #[serde(default)]
    pub input: Value,
    #[serde(flatten)]
    pub additive: BTreeMap<String, Value>,
}

impl ProposedAction {
    pub fn validate(&self) -> Result<(), String> {
        if self.action_id.is_empty()
            || self.action_id.len() > 128
            || self.action_id.chars().any(char::is_control)
            || self.tool.is_empty()
            || self.tool.len() > 128
            || !self
                .tool
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err("extension proposed action identity is invalid".to_string());
        }
        if serde_json::to_vec(&self.input)
            .map_err(|error| format!("encode extension action input: {error}"))?
            .len()
            > MAX_ACTION_INPUT_BYTES
        {
            return Err("extension proposed action input exceeds its limit".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShutdownReason {
    TaskComplete,
    Disabled,
    ProtocolFailure,
}

pub fn validate_ready(
    request: &AbiRequest,
    response: &AbiResponse,
    min_version: u32,
    max_version: u32,
    required_features: &[String],
) -> Result<(), String> {
    validate_correlated(request, response)?;
    let ExtensionMessage::Ready {
        selected_version,
        accepted_features,
    } = &response.message
    else {
        return Err("extension did not answer initialize with ready".to_string());
    };
    if *selected_version != ABI_VERSION
        || *selected_version < min_version
        || *selected_version > max_version
        || response.protocol != *selected_version
    {
        return Err(
            "extension protocol downgrade or incompatible version was rejected".to_string(),
        );
    }
    if required_features
        .iter()
        .any(|feature| !accepted_features.contains(feature))
        || accepted_features.iter().any(|feature| {
            !matches!(
                feature.as_str(),
                FEATURE_OBSERVATIONAL_EVENTS | FEATURE_PROPOSED_ACTIONS
            )
        })
    {
        return Err("extension feature negotiation failed".to_string());
    }
    Ok(())
}

pub fn validate_result<'a>(
    request: &AbiRequest,
    response: &'a AbiResponse,
    event_id: &str,
    max_output_bytes: usize,
    max_actions: usize,
) -> Result<(&'a Option<String>, &'a [ProposedAction]), String> {
    validate_correlated(request, response)?;
    if response.protocol != ABI_VERSION {
        return Err("extension changed protocol version after initialization".to_string());
    }
    let ExtensionMessage::Result {
        event_id: actual,
        output,
        proposed_actions,
    } = &response.message
    else {
        return Err("extension did not answer event with result".to_string());
    };
    if actual != event_id {
        return Err("extension result did not correlate with the event".to_string());
    }
    if output
        .as_ref()
        .is_some_and(|output| output.len() > max_output_bytes)
        || proposed_actions.len() > max_actions.min(MAX_ACTIONS_PER_EVENT)
    {
        return Err("extension result exceeds negotiated limits".to_string());
    }
    for action in proposed_actions {
        action.validate()?;
    }
    Ok((output, proposed_actions))
}

pub fn validate_shutdown(request: &AbiRequest, response: &AbiResponse) -> Result<(), String> {
    validate_correlated(request, response)?;
    if response.protocol != ABI_VERSION
        || !matches!(response.message, ExtensionMessage::ShutdownAck)
    {
        return Err("extension did not acknowledge shutdown".to_string());
    }
    Ok(())
}

fn validate_correlated(request: &AbiRequest, response: &AbiResponse) -> Result<(), String> {
    request.binding.validate()?;
    response.binding.validate()?;
    if response.binding != request.binding || response.sequence != request.sequence {
        return Err("extension response binding or sequence did not correlate".to_string());
    }
    Ok(())
}

pub async fn write_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    request: &AbiRequest,
) -> Result<(), String> {
    write_frame(writer, REQUEST_KIND, request).await
}

pub async fn read_request<R: AsyncRead + Unpin>(reader: &mut R) -> Result<AbiRequest, String> {
    read_frame(reader, REQUEST_KIND).await
}

pub async fn write_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    response: &AbiResponse,
) -> Result<(), String> {
    write_frame(writer, RESPONSE_KIND, response).await
}

pub async fn read_response<R: AsyncRead + Unpin>(reader: &mut R) -> Result<AbiResponse, String> {
    read_frame(reader, RESPONSE_KIND).await
}

async fn write_frame<W, T>(writer: &mut W, kind: u8, value: &T) -> Result<(), String>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(value)
        .map_err(|error| format!("encode extension ABI frame: {error}"))?;
    if body.is_empty() || body.len() > MAX_ABI_FRAME_BYTES {
        return Err("extension ABI frame exceeds its limit".to_string());
    }
    let mut header = [0u8; HEADER_BYTES];
    header[..4].copy_from_slice(&MAGIC);
    header[4] = kind;
    header[5] = 0;
    header[6..].copy_from_slice(&(body.len() as u32).to_be_bytes());
    writer
        .write_all(&header)
        .await
        .map_err(|error| format!("write extension ABI header: {error}"))?;
    writer
        .write_all(&body)
        .await
        .map_err(|error| format!("write extension ABI body: {error}"))?;
    writer
        .flush()
        .await
        .map_err(|error| format!("flush extension ABI frame: {error}"))
}

async fn read_frame<R, T>(reader: &mut R, expected_kind: u8) -> Result<T, String>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut header = [0u8; HEADER_BYTES];
    reader
        .read_exact(&mut header)
        .await
        .map_err(|error| format!("read extension ABI header: {error}"))?;
    if header[..4] != MAGIC
        || header[4] != expected_kind
        || header[5] != 0
        || u32::from_be_bytes(header[6..].try_into().expect("length slice")) == 0
    {
        return Err("extension ABI frame header is malformed".to_string());
    }
    let length = u32::from_be_bytes(header[6..].try_into().expect("length slice")) as usize;
    if length > MAX_ABI_FRAME_BYTES {
        return Err("extension ABI frame exceeds its limit".to_string());
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|error| format!("read extension ABI body: {error}"))?;
    serde_json::from_slice(&body)
        .map_err(|_| "extension ABI frame is not a valid typed envelope".to_string())
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/extension_host/abi.rs"
    ));
}
