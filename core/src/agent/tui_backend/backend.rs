//! Stable, deliberately narrow service definition used by the protocol.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use super::protocol::RpcError;

/// Only these owner-scoped operations are reachable from the adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Operation {
    ConversationCreate,
    ConversationGet,
    ConversationList,
    ConversationUpdate,
    ConversationFork,
    ConversationRevert,
    TaskSubmit,
    TaskGet,
    TaskList,
    TaskStream,
    TaskCancel,
    PermissionPending,
    PermissionStatus,
    SkillsList,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReviewDecision {
    ApproveOnce,
    Deny,
}

#[derive(Clone, Debug)]
pub(super) struct BackendInfo {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub provider: String,
    pub model: String,
    pub models: Vec<String>,
    pub ready: bool,
}

#[async_trait]
pub(super) trait Backend: Send + Sync {
    fn info(&self) -> &BackendInfo;

    async fn call(&self, operation: Operation, params: Value) -> Result<Value, RpcError>;

    /// Ask the installed, polkit-authorized helper to decide one exact pending
    /// request. Returning successfully is not itself a capability grant.
    async fn review(&self, id: &str, decision: ReviewDecision) -> Result<(), RpcError>;
}
