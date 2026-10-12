use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MemoryAccess {
    pub source: String,
    pub revision: String,
    pub reference: MemoryReference,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct MemorySourcesPayload {
    pub id: String,
    pub name: String,
    pub sources: Vec<MemoryAccess>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    #[default]
    Notes,
    Apps,
    Conversations,
    Semantic,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryReference {
    Note { name: String },
    App { id: String },
    Message { id: String },
    Semantic { namespace: String, key: String },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MemoryEntry {
    pub reference: MemoryReference,
    pub source: String,
    pub category: String,
    pub recorded_at_ms: Option<i64>,
    pub revision: String,
    pub content: String,
    pub editable: bool,
    pub deletable: bool,
    pub provenance: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct MemoryInventory {
    pub entries: Vec<MemoryEntry>,
    pub has_more: bool,
    pub coverage: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryInspectRequest {
    pub kind: MemoryKind,
    pub query: String,
    pub limit: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryReadRequest {
    pub reference: MemoryReference,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryUpdateRequest {
    pub name: String,
    pub revision: String,
    pub content: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryForgetRequest {
    pub reference: MemoryReference,
    pub revision: String,
    pub confirm: bool,
}
