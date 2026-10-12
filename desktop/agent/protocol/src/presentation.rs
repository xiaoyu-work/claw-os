use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Preparing,
    Running,
    Completed,
    Failed,
    Unconfirmed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceStatus {
    NotRequired,
    Verified,
    Partial,
    Missing,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceSource {
    pub tool_call_id: String,
    pub tool_name: String,
    pub is_error: bool,
    pub result_sha256: String,
    pub result_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePresentation {
    pub status: EvidenceStatus,
    pub required: bool,
    pub verified_claims: u64,
    pub total_claims: u64,
    #[serde(default)]
    pub source_count: u64,
    pub sources: Vec<EvidenceSource>,
    pub warnings: Vec<String>,
    pub interpretation: String,
    #[serde(default)]
    pub truncated: bool,
}

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
    #[serde(default)]
    pub memory_sources: Vec<crate::MemoryAccess>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<EvidencePresentation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
}
