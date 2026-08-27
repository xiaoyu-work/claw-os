use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{ErrorCode, ToolInput};

pub const TASK_EVENT: &str = "task";
pub const DELTA_EVENT: &str = "delta";
pub const TEXT_EVENT_ALIAS: &str = "text";
pub const TOOL_USE_START_EVENT: &str = "tool_use_start";
pub const TOOL_INPUT_DELTA_EVENT: &str = "tool_input_delta";
pub const TOOL_USE_EVENT: &str = "tool_use";
pub const TOOL_START_EVENT: &str = "tool_start";
pub const TOOL_RESULT_EVENT: &str = "tool_result";
pub const WARNING_EVENT: &str = "warning";
pub const TURN_DONE_EVENT: &str = "turn_done";
pub const DONE_EVENT: &str = "done";
pub const ERROR_EVENT: &str = "error";

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    TaskStarted(TaskStarted),
    Delta(DeltaPayload),
    ToolUseStart(ToolUseStartPayload),
    ToolInputDelta(ToolInputDeltaPayload),
    ToolUse(ToolUsePayload),
    ToolStart(ToolStartPayload),
    ToolResult(ToolResultPayload),
    Warning(WarningPayload),
    TurnDone(TurnDonePayload),
    Done(DonePayload),
    Error(StreamError),
}

impl StreamEvent {
    pub const fn event_name(&self) -> &'static str {
        match self {
            Self::TaskStarted(_) => TASK_EVENT,
            Self::Delta(_) => DELTA_EVENT,
            Self::ToolUseStart(_) => TOOL_USE_START_EVENT,
            Self::ToolInputDelta(_) => TOOL_INPUT_DELTA_EVENT,
            Self::ToolUse(_) => TOOL_USE_EVENT,
            Self::ToolStart(_) => TOOL_START_EVENT,
            Self::ToolResult(_) => TOOL_RESULT_EVENT,
            Self::Warning(_) => WARNING_EVENT,
            Self::TurnDone(_) => TURN_DONE_EVENT,
            Self::Done(_) => DONE_EVENT,
            Self::Error(_) => ERROR_EVENT,
        }
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        match self {
            Self::TaskStarted(payload) => serde_json::to_string(payload),
            Self::Delta(payload) => serde_json::to_string(payload),
            Self::ToolUseStart(payload) => serde_json::to_string(payload),
            Self::ToolInputDelta(payload) => serde_json::to_string(payload),
            Self::ToolUse(payload) => serde_json::to_string(payload),
            Self::ToolStart(payload) => serde_json::to_string(payload),
            Self::ToolResult(payload) => serde_json::to_string(payload),
            Self::Warning(payload) => serde_json::to_string(payload),
            Self::TurnDone(payload) => serde_json::to_string(payload),
            Self::Done(payload) => serde_json::to_string(payload),
            Self::Error(payload) => serde_json::to_string(payload),
        }
    }

    pub fn from_json(event_name: &str, data: &str) -> serde_json::Result<Option<Self>> {
        fn decode<T: DeserializeOwned>(data: &str) -> serde_json::Result<T> {
            serde_json::from_str(data)
        }

        let event = match event_name {
            TASK_EVENT => Self::TaskStarted(decode(data)?),
            DELTA_EVENT => Self::Delta(decode(data)?),
            TEXT_EVENT_ALIAS => {
                let alias: TextAliasPayload = decode(data)?;
                Self::Delta(DeltaPayload::new(alias.delta))
            }
            TOOL_USE_START_EVENT => Self::ToolUseStart(decode(data)?),
            TOOL_INPUT_DELTA_EVENT => Self::ToolInputDelta(decode(data)?),
            TOOL_USE_EVENT => Self::ToolUse(decode(data)?),
            TOOL_START_EVENT => Self::ToolStart(decode(data)?),
            TOOL_RESULT_EVENT => Self::ToolResult(decode(data)?),
            WARNING_EVENT => Self::Warning(decode(data)?),
            TURN_DONE_EVENT => Self::TurnDone(decode(data)?),
            DONE_EVENT => Self::Done(decode(data)?),
            ERROR_EVENT => Self::Error(decode(data)?),
            _ => return Ok(None),
        };
        Ok(Some(event))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStarted {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeltaPayload {
    #[serde(rename = "type", default = "delta_type")]
    pub event_type: String,
    #[serde(default)]
    pub text: String,
}

impl DeltaPayload {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            event_type: delta_type(),
            text: text.into(),
        }
    }
}

fn delta_type() -> String {
    DELTA_EVENT.to_string()
}

#[derive(Debug, Deserialize)]
struct TextAliasPayload {
    #[serde(default)]
    delta: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolUseStartPayload {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolInputDeltaPayload {
    #[serde(default)]
    pub id: String,
    #[serde(default, alias = "partial")]
    pub delta: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolUsePayload {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ToolInput>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolStartPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ToolInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl ToolResultPayload {
    pub fn presented_text(&self) -> String {
        self.preview
            .as_ref()
            .or(self.output.as_ref())
            .or(self.content.as_ref())
            .or(self.text.as_ref())
            .cloned()
            .unwrap_or_default()
    }

    pub fn presented_is_error(&self) -> bool {
        self.is_error.unwrap_or_else(|| !self.ok.unwrap_or(true))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarningPayload {
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u32,
    #[serde(default)]
    pub output_tokens: u32,
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnDonePayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish: Option<String>,
    #[serde(default)]
    pub usage: Usage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DonePayload {
    #[serde(rename = "type", default = "done_type")]
    pub event_type: String,
    #[serde(default)]
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Compatibility field emitted by pre-v1 bridge builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turns_used: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl DonePayload {
    pub fn presented_answer(&self) -> Option<String> {
        self.answer
            .as_ref()
            .or(self.response.as_ref())
            .filter(|answer| !answer.is_empty())
            .cloned()
    }
}

fn done_type() -> String {
    DONE_EVENT.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamError {
    #[serde(rename = "type", default = "error_type")]
    pub event_type: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<ErrorCode>,
}

impl StreamError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            event_type: error_type(),
            message: message.into(),
            error: None,
            code: None,
        }
    }

    pub fn presented_message(&self) -> String {
        if self.message.is_empty() {
            self.error
                .clone()
                .unwrap_or_else(|| "stream error".to_string())
        } else {
            self.message.clone()
        }
    }
}

fn error_type() -> String {
    ERROR_EVENT.to_string()
}

#[cfg(test)]
mod tests {
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/test/unit/stream.rs"));
}
