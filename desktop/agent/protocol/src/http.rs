use serde::{Deserialize, Serialize};

use crate::{MIN_SUPPORTED_PROTOCOL_VERSION, ProtocolVersion};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeEndpoint {
    pub port: u16,
    pub token: String,
    pub protocol_version: ProtocolVersion,
    pub min_protocol_version: ProtocolVersion,
}

impl BridgeEndpoint {
    pub const fn has_valid_version_range(&self) -> bool {
        self.min_protocol_version.0 <= self.protocol_version.0
            && self.min_protocol_version.0 <= crate::CURRENT_PROTOCOL_VERSION
            && self.protocol_version.0 >= MIN_SUPPORTED_PROTOCOL_VERSION
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ts_ms: Option<i64>,
    #[serde(default)]
    pub message_count: i64,
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
