use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};

use crate::{ProtocolMetadata, ProtocolVersion};

pub const MAX_CHAT_ATTACHMENTS: usize = 4;
pub const MAX_CHAT_ATTACHMENT_BYTES: usize = 256 * 1024;
pub const MAX_CHAT_ATTACHMENT_NAME_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeEndpoint {
    pub port: u16,
    pub token: String,
    pub protocol_version: ProtocolVersion,
    pub min_protocol_version: ProtocolVersion,
}

impl BridgeEndpoint {
    pub const fn protocol_metadata(&self) -> ProtocolMetadata {
        ProtocolMetadata {
            protocol_version: self.protocol_version,
            min_protocol_version: self.min_protocol_version,
        }
    }

    pub const fn has_valid_version_range(&self) -> bool {
        self.protocol_metadata().is_valid()
    }

    pub const fn negotiate(&self, client: ProtocolMetadata) -> Option<ProtocolVersion> {
        self.protocol_metadata().negotiate_highest(client)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    ProtocolVersionRequired,
    IncompatibleProtocolVersion,
    InvalidRequest,
    NotFound,
    NotImplemented,
    ServiceUnavailable,
    UpstreamError,
    Timeout,
    PayloadTooLarge,
    UnsupportedMediaType,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: String,
    pub code: ErrorCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl ErrorEnvelope {
    pub fn new(code: ErrorCode, error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            code,
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChatAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub messages: Vec<ChatRequestMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_context: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatAttachment {
    pub name: String,
    pub media_type: String,
    pub data: String,
}

impl ChatAttachment {
    pub fn from_bytes(name: String, bytes: &[u8]) -> Result<Self, &'static str> {
        validate_attachment_name(&name)?;
        if bytes.is_empty() {
            return Err("Image attachment is empty.");
        }
        if bytes.len() > MAX_CHAT_ATTACHMENT_BYTES {
            return Err("Image attachment exceeds the size limit.");
        }
        let media_type = detect_media_type(bytes).ok_or("Unsupported image format.")?;
        Ok(Self {
            name,
            media_type: media_type.to_string(),
            data: STANDARD.encode(bytes),
        })
    }

    pub fn decoded_len(&self) -> Result<usize, &'static str> {
        validate_attachment_name(&self.name)?;
        let bytes = STANDARD
            .decode(self.data.as_bytes())
            .map_err(|_| "Image attachment is not valid base64.")?;
        if bytes.is_empty() || bytes.len() > MAX_CHAT_ATTACHMENT_BYTES {
            return Err("Image attachment has an invalid size.");
        }
        if STANDARD.encode(&bytes) != self.data {
            return Err("Image attachment is not canonical base64.");
        }
        if detect_media_type(&bytes) != Some(self.media_type.as_str()) {
            return Err("Image attachment does not match its declared media type.");
        }
        Ok(bytes.len())
    }
}

pub fn validate_chat_attachments(attachments: &[ChatAttachment]) -> Result<(), &'static str> {
    if attachments.len() > MAX_CHAT_ATTACHMENTS {
        return Err("Too many image attachments.");
    }
    let mut total = 0usize;
    for attachment in attachments {
        total = total
            .checked_add(attachment.decoded_len()?)
            .ok_or("Image attachment size overflow.")?;
        if total > MAX_CHAT_ATTACHMENT_BYTES {
            return Err("Image attachments exceed the total size limit.");
        }
    }
    Ok(())
}

fn validate_attachment_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty()
        || name.len() > MAX_CHAT_ATTACHMENT_NAME_BYTES
        || name.chars().count() > 128
        || name.chars().any(char::is_control)
        || name.contains('/')
        || name.contains('\\')
    {
        return Err("Image attachment name is invalid.");
    }
    Ok(())
}

fn detect_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) && bytes.ends_with(&[0xff, 0xd9]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

impl ChatRequest {
    /// Resolve the original prompt form and the legacy messages form.
    pub fn resolved_prompt(&self) -> String {
        if let Some(prompt) = self
            .prompt
            .as_ref()
            .filter(|value| !value.trim().is_empty())
        {
            return prompt.clone();
        }
        self.messages
            .iter()
            .rev()
            .find(|message| message.role == "user")
            .map(|message| message.content.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRequestMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation_id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ts_ms: Option<i64>,
    #[serde(default)]
    pub message_count: i64,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub manageable: bool,
    #[serde(default)]
    pub legacy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// Tool inputs are intentionally open JSON. Tool schemas are registered at
/// runtime and are not part of the desktop presentation protocol.
pub type ToolInput = serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallView {
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub input: ToolInput,
    #[serde(default)]
    pub partial_json: String,
    #[serde(default)]
    pub in_progress: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultView {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryMessage {
    pub role: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool_calls: Vec<ToolCallView>,
    #[serde(default)]
    pub tool_results: Vec<ToolResultView>,
    #[serde(default)]
    pub ts_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub session_id: String,
    #[serde(default)]
    pub n: usize,
    #[serde(default)]
    pub messages: Vec<HistoryMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSummary {
    pub id: String,
    pub provider: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelsResponse {
    pub ready: bool,
    pub provider: String,
    pub model: String,
    pub label: String,
    #[serde(default)]
    pub models: Vec<ModelSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelResponse {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub cancelled: bool,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceResponse {
    pub text: String,
    pub bytes_received: usize,
    pub mime_type: String,
    #[serde(default)]
    pub placeholder: bool,
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/http.rs"));
}
