//! LLM message / tool / streaming types.
//!
//! Designed to be the lossless union of what Anthropic, OpenAI, Gemini,
//! Ollama, Bedrock, OpenRouter, xAI, DeepSeek, and llama.cpp need. Provider
//! impls translate to/from their native shapes.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default)]
        is_error: bool,
        content: String,
    },
    /// Base64-encoded image (forward compatibility for vision-capable providers).
    Image {
        media_type: String,
        data: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    pub fn system_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool parameters.
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Tool { name: String },
}


#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Tool>,

    #[serde(default)]
    pub tool_choice: ToolChoice,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,

    /// Provider-specific extras (Anthropic `metadata`, OpenAI `seed`, etc.)
    /// — pass-through, opaque to the trait.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub extra: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolUse,
    Refusal,
    ContentFilter,
    Other,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Cached prompt tokens (Anthropic prompt caching, etc.).
    #[serde(default)]
    pub cache_read_tokens: u32,
    #[serde(default)]
    pub cache_write_tokens: u32,
}

/// Information about the engine that *actually produced* a response.
/// Captured for the audit trail (run record). Always reflects the
/// loaded runtime, never the engine_pkg registry — for local engines
/// the registry can race with the daemon's process-wide loaded
/// singleton (the new active version doesn't take effect until restart).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EngineInfo {
    /// Engine name. For local: matches `engine_pkg::KNOWN_ENGINES`
    /// (e.g. `"llama-cpp"`). For cloud: empty / not set.
    pub name: String,
    /// Engine version. For llama-cpp: the build number (`"b4001"`).
    /// For ort/ort-genai: SemVer string. Only meaningful for local
    /// engines.
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
}

/// Streaming event surface — designed to losslessly carry every provider's
/// incremental update kinds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamEvent {
    /// Incremental text delta.
    TextDelta { text: String },
    /// A tool call is starting (id + name known, input streamed).
    ToolUseStart { id: String, name: String },
    /// Incremental JSON-fragment for an in-progress tool input.
    ToolInputDelta { id: String, partial_json: String },
    /// A tool call has been fully formed.
    ToolUse(ToolCall),
    /// A complete (non-incremental) message — used by providers without true streaming.
    Message(ChatResponse),
    /// Final usage and finish reason for the response.
    Done { finish: FinishReason, usage: Usage },
    /// Recoverable warning (rate limit nearing, deprecation, etc.).
    Warning { message: String },
}
