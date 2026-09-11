//! Activity presentation only. Ownership, persistence, transitions and work
//! admission belong to clawd, shared with the terminal client.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Active,
    Paused,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityResource {
    pub label: String,
    pub reference: String,
}

/// Components only; canonical URI semantics belong to the shared broker/SDK.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppObjectReference {
    pub app_id: String,
    pub object_type: String,
    pub object_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityObjectAttachRequest {
    pub label: String,
    pub object: AppObjectReference,
}

/// A declaration is authenticated metadata, not proof of object access.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityObjectStatus {
    Declared,
    Unavailable,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppObjectInvocation {
    pub app_id: String,
    pub operation: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityOperationPreviewRequest {
    pub app_id: String,
    pub operation: String,
    pub args: Vec<String>,
}

impl From<&AppObjectInvocation> for ActivityOperationPreviewRequest {
    fn from(invocation: &AppObjectInvocation) -> Self {
        Self {
            app_id: invocation.app_id.clone(),
            operation: invocation.operation.clone(),
            args: invocation.args.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEffectKind {
    Read,
    Create,
    Update,
    Delete,
    External,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEffectRecovery {
    NotApplicable,
    Reversible,
    Compensatable,
    Irreversible,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEffectTargetKind {
    Path,
    Host,
    Name,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppEffectTargetState {
    Requested,
    Unspecified,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDeclaredEffect {
    pub kind: AppEffectKind,
    pub label: String,
    pub recovery: AppEffectRecovery,
    pub target_arg: Option<String>,
    pub target_kind: Option<AppEffectTargetKind>,
    pub requested_targets: Vec<String>,
    pub target_state: AppEffectTargetState,
}

/// Manifest metadata only, never execution evidence or an authorization grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityOperationPreview {
    pub schema: u32,
    pub app_id: String,
    pub app_name: String,
    pub app_version: String,
    pub package_digest: String,
    pub operation: String,
    pub operation_label: String,
    pub effects_declared: bool,
    pub effects: Vec<AppDeclaredEffect>,
    pub unresolved_arguments: Vec<String>,
    pub authorization_checked: bool,
    pub executed: bool,
    pub effects_confirmed: bool,
    pub notes: Vec<String>,
}

impl ActivityOperationPreview {
    pub fn is_metadata_only(&self) -> bool {
        self.schema == 1 && !self.authorization_checked && !self.executed && !self.effects_confirmed
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityReceiptSource {
    CallerReported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityReceiptOutcome {
    Returned,
    ReportedError,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptResultKind {
    Json,
    Text,
    Empty,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptResultSummary {
    pub kind: ReceiptResultKind,
    pub sha256: String,
    pub bytes: u64,
    pub preview: String,
    pub preview_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityReceiptReport {
    pub id: String,
    pub app_id: String,
    pub operation: String,
    pub package_digest: String,
    pub outcome: ActivityReceiptOutcome,
    #[serde(default)]
    pub result: Option<ReceiptResultSummary>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptDeclaredEffect {
    pub kind: AppEffectKind,
    pub label: String,
    pub recovery: AppEffectRecovery,
    #[serde(default)]
    pub target_arg: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityReceiptDeclaration {
    pub app_version: String,
    pub operation_label: String,
    pub effects: Vec<ReceiptDeclaredEffect>,
}

/// An immutable caller report, not an OS execution or mutation attestation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityReceiptView {
    pub id: String,
    pub activity_id: String,
    pub received_at: String,
    pub source: ActivityReceiptSource,
    pub report: ActivityReceiptReport,
    #[serde(default)]
    pub declaration: Option<ActivityReceiptDeclaration>,
    #[serde(default)]
    pub declaration_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityReceiptsResponse {
    pub schema: u32,
    pub activity_id: String,
    pub receipts: Vec<ActivityReceiptView>,
}

impl ActivityReceiptsResponse {
    pub fn matches_activity(&self, id: &str) -> bool {
        self.schema == 1
            && !id.trim().is_empty()
            && self.activity_id == id
            && self
                .receipts
                .iter()
                .all(|receipt| !receipt.id.trim().is_empty() && receipt.activity_id == id)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityReceiptsQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppObjectDescription {
    pub object: AppObjectReference,
    pub reference: String,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub object_label: String,
    #[serde(default)]
    pub object_summary: String,
    pub invocation: AppObjectInvocation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityObjectResourceView {
    pub label: String,
    pub reference: String,
    pub status: ActivityObjectStatus,
    #[serde(default)]
    pub description: Option<AppObjectDescription>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityObjectsResponse {
    pub activity_id: String,
    #[serde(default)]
    pub objects: Vec<ActivityObjectResourceView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityView {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub state: ActivityState,
    #[serde(default)]
    pub completion_criteria: String,
    #[serde(default)]
    pub boundaries: String,
    #[serde(default)]
    pub resources: Vec<ActivityResource>,
    #[serde(default)]
    pub completion_note: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityJobView {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub finished_at: Option<String>,
    #[serde(default)]
    pub response: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityApprovalView {
    pub id: String,
    pub session_id: String,
    pub label: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityListResponse {
    #[serde(default)]
    pub activities: Vec<ActivityView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityDetailResponse {
    pub activity: ActivityView,
    #[serde(default)]
    pub jobs: Vec<ActivityJobView>,
    #[serde(default)]
    pub sessions: Vec<String>,
    #[serde(default)]
    pub pending_approvals: Vec<ActivityApprovalView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approvals_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityListQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<ActivityState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityCreateRequest {
    pub title: String,
    pub goal: String,
    #[serde(default)]
    pub completion_criteria: String,
    #[serde(default)]
    pub boundaries: String,
    #[serde(default)]
    pub resources: Vec<ActivityResource>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityUpdateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_criteria: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boundaries: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Vec<ActivityResource>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityTransitionRequest {
    pub state: ActivityState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityRunRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_memory: Option<bool>,
}

/// Acknowledges durable work admission, not achievement of an Activity's goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityWorkResponse {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub activity_id: Option<String>,
}

#[cfg(test)]
mod tests {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test/unit/activities.rs"
    ));
}
