//! The model can read its Root-selected domain, never choose an owner or domain.

use super::{NoteDraft, ResourceTag, WorkspaceNote, WorkspaceRead};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, OnceLock};

pub const MAX_EXCHANGES: u32 = 256;
pub const MAX_REPLY_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prelude {
    pub workspace_id: String,
    pub name: String,
    pub revision: u64,
    pub background: String,
    pub notes: Vec<WorkspaceNote>,
    pub tags: Vec<ResourceTag>,
    pub notes_total: usize,
    pub tags_total: usize,
    pub temporary_reads: Vec<super::ReadLink>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecallInput {
    pub command: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub message_id: Option<i64>,
    #[serde(default = "read_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "read_chars")]
    pub max_chars: usize,
    #[serde(default)]
    pub revision: Option<String>,
}

fn read_limit() -> usize {
    10
}
fn read_chars() -> usize {
    4096
}

impl RecallInput {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.command.as_str(),
            "search" | "recent" | "show" | "sessions" | "stats"
        ) || !(1..=200).contains(&self.limit)
            || self.query.len() > 2048
            || self
                .session_id
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 128)
            || (self.command == "search" && self.query.trim().is_empty())
            || (self.command == "recent" && self.session_id.is_none())
            || (self.command == "show" && self.message_id.is_none_or(|id| id <= 0))
        {
            return Err("invalid bounded recall request".into());
        }
        validate_window(self.offset, self.max_chars, self.revision.as_deref())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticInput {
    pub command: String,
    #[serde(default)]
    pub query: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default = "read_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "read_chars")]
    pub max_chars: usize,
    #[serde(default)]
    pub revision: Option<String>,
}

impl SemanticInput {
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.command.as_str(), "search" | "read" | "count")
            || !(1..=50).contains(&self.limit)
            || self.query.len() > 2048
            || (self.command == "search" && self.query.trim().is_empty())
            || (self.namespace.is_some() && self.session_id.is_some())
            || self
                .key
                .as_ref()
                .is_some_and(|key| key.is_empty() || key.len() > 128)
            || self.namespace.as_ref().is_some_and(|ns| {
                ns.len() > 256 || !(ns.starts_with("session/") || ns.starts_with("app/"))
            })
            || self
                .session_id
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 128)
            || (self.command == "read" && (self.namespace.is_none() || self.key.is_none()))
        {
            return Err("invalid bounded semantic recall request".into());
        }
        validate_window(self.offset, self.max_chars, self.revision.as_deref())
    }

    pub fn namespace(&self) -> Option<String> {
        self.namespace.clone().or_else(|| {
            self.session_id.as_ref().map(|id| {
                if id.starts_with("session/") {
                    id.clone()
                } else {
                    format!("session/{id}")
                }
            })
        })
    }
}

pub fn validate_window(offset: usize, chars: usize, revision: Option<&str>) -> Result<(), String> {
    if !(1..=16384).contains(&chars)
        || offset > 50 * 1024 * 1024
        || (offset > 0 && revision.is_none())
        || revision.is_some_and(|id| id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("invalid revision-bound read window".into());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContextOperation {
    Check,
    Workspace { request: WorkspaceRead },
    Recall { input: RecallInput },
    SemanticRecall { input: SemanticInput },
    Report { note: NoteDraft },
}

impl ContextOperation {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Check => Ok(()),
            Self::Workspace { request } => request.validate(),
            Self::Recall { input } => input.validate(),
            Self::SemanticRecall { input } => input.validate(),
            Self::Report { note } => note.validate(),
        }
    }
}

pub trait ContextGateway: Send + Sync {
    fn request(&self, operation: ContextOperation) -> Result<Value, String>;
}

struct Installed {
    gateway: Arc<dyn ContextGateway>,
    workspace_bound: bool,
}

static CURRENT: OnceLock<Installed> = OnceLock::new();

pub(crate) fn install(
    gateway: Arc<dyn ContextGateway>,
    workspace_bound: bool,
) -> Result<(), String> {
    CURRENT
        .set(Installed {
            gateway,
            workspace_bound,
        })
        .map_err(|_| "task context gateway was already installed".into())
}

pub fn available() -> bool {
    CURRENT.get().is_some()
}
pub fn workspace_bound() -> bool {
    CURRENT.get().is_some_and(|state| state.workspace_bound)
}

pub async fn request(operation: ContextOperation) -> Result<Value, String> {
    operation.validate()?;
    let gateway = CURRENT
        .get()
        .ok_or("Root task context gateway is unavailable")?
        .gateway
        .clone();
    tokio::task::spawn_blocking(move || gateway.request(operation))
        .await
        .map_err(|e| format!("task context exchange failed: {e}"))?
}
