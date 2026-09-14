//! Activity presentation only. Ownership, persistence, transitions and work
//! admission belong to clawd, shared with the terminal client.

use serde::{Deserialize, Serialize};

use crate::CapabilityPolicyScope;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityDecisionStatus {
    Pending,
    Approved,
    Denied,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityIssueKind {
    WaitingApproval,
    Indeterminate,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityDecisionRisk {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityNotificationSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityNotificationState {
    Unread,
    Read,
    Acknowledged,
    Dismissed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAttentionCounts {
    pub queued: u64,
    pub running: u64,
    pub waiting: u64,
    pub completed: u64,
    pub failed: u64,
    pub cancelled: u64,
    pub indeterminate: u64,
    pub pending_decisions: u64,
    pub unavailable_decisions: u64,
    pub unread_notifications: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAttentionDecision {
    pub id: String,
    pub job_id: String,
    pub session_id: Option<String>,
    pub status: ActivityDecisionStatus,
    pub requested_at: Option<u64>,
    pub verb: Option<String>,
    pub scope: Option<CapabilityPolicyScope>,
    pub risk: Option<ActivityDecisionRisk>,
    pub reason: Option<String>,
    pub review_id: Option<String>,
    pub error: Option<String>,
}

impl ActivityAttentionDecision {
    fn is_consistent(&self) -> bool {
        if self.id.is_empty() || self.job_id.is_empty() {
            return false;
        }
        let details = self.requested_at.is_some()
            && self.verb.as_ref().is_some_and(|value| !value.is_empty())
            && self.scope.is_some()
            && self.risk.is_some()
            && self.reason.is_some()
            && self
                .review_id
                .as_ref()
                .is_some_and(|value| !value.is_empty())
            && self.error.is_none();
        let unavailable = self.requested_at.is_none()
            && self.verb.is_none()
            && self.scope.is_none()
            && self.risk.is_none()
            && self.reason.is_none()
            && self.review_id.is_none()
            && self.error.as_ref().is_some_and(|value| !value.is_empty());
        match self.status {
            ActivityDecisionStatus::Unavailable => unavailable,
            _ => details,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAttentionIssue {
    pub job_id: String,
    pub session_id: Option<String>,
    pub kind: ActivityIssueKind,
    pub status: String,
    pub execution_phase: String,
    pub title: String,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAttentionNotification {
    pub id: String,
    pub source: String,
    pub kind: String,
    pub severity: ActivityNotificationSeverity,
    pub title: String,
    pub body: String,
    pub task_id: Option<String>,
    pub session_id: Option<String>,
    pub state: ActivityNotificationState,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAttentionTotals {
    pub decisions: u64,
    pub issues: u64,
    pub notifications: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAttentionHasMore {
    pub decisions: bool,
    pub issues: bool,
    pub notifications: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityAttentionResponse {
    pub schema: u32,
    pub activity_id: String,
    pub activity_state: ActivityState,
    pub limit: u32,
    pub counts: ActivityAttentionCounts,
    pub decisions: Vec<ActivityAttentionDecision>,
    pub issues: Vec<ActivityAttentionIssue>,
    pub notifications: Vec<ActivityAttentionNotification>,
    pub totals: ActivityAttentionTotals,
    pub has_more: ActivityAttentionHasMore,
}

impl ActivityAttentionResponse {
    pub fn matches_activity(&self, id: &str, state: ActivityState) -> bool {
        self.schema == 1
            && self.activity_id == id
            && self.activity_state == state
            && (1..=100).contains(&self.limit)
            && self.decisions.len() <= self.limit as usize
            && self.issues.len() <= self.limit as usize
            && self.notifications.len() <= self.limit as usize
            && self
                .decisions
                .iter()
                .all(ActivityAttentionDecision::is_consistent)
            && self.issues.iter().all(|issue| {
                !issue.job_id.is_empty()
                    && !issue.status.is_empty()
                    && !issue.execution_phase.is_empty()
            })
            && self.notifications.iter().all(|notification| {
                !notification.id.is_empty()
                    && notification
                        .task_id
                        .as_ref()
                        .is_some_and(|id| !id.is_empty())
            })
            && page_matches(
                self.totals.decisions,
                self.decisions.len(),
                self.has_more.decisions,
            )
            && page_matches(self.totals.issues, self.issues.len(), self.has_more.issues)
            && page_matches(
                self.totals.notifications,
                self.notifications.len(),
                self.has_more.notifications,
            )
    }
}

fn page_matches(total: u64, shown: usize, has_more: bool) -> bool {
    total >= shown as u64 && has_more == (total > shown as u64)
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<ActivityAttentionResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_error: Option<String>,
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
