use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMetadata {
    pub name: String,
    pub media_type: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedTool {
    pub id: String,
    pub name: String,
    pub started: bool,
    pub ok: Option<bool>,
    pub latency_ms: Option<u64>,
    pub bytes_returned: Option<u64>,
    pub error_preview: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskPresentation {
    pub available: bool,
    pub reasoning: Vec<String>,
    pub usage: Option<RetainedUsage>,
    pub tools: Vec<RetainedTool>,
    pub attachments: Vec<AttachmentMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}
